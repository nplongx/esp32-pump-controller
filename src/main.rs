use esp_idf_hal::gpio::PinDriver;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::mqtt::client::{EspMqttClient, QoS};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

mod config;
mod controller;
mod mqtt;
mod pump;

use config::create_shared_config;
use controller::start_fsm_control_loop;
use mqtt::{create_shared_sensor_data, ConnectionState};
use pump::PumpController;

const WIFI_SSID: &str = "Huynh Hong";
const WIFI_PASS: &str = "123443215";
const MQTT_URL: &str = "mqtt://192.168.1.4:1883";
const DEVICE_ID: &str = "device_001";

fn main() -> anyhow::Result<()> {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    info!("🚀 Khởi động hệ thống FSM Thủy canh Agitech...");

    // ===============================
    // 1. Hardware Init
    // ===============================
    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let shared_config = create_shared_config();
    let shared_sensor_data = create_shared_sensor_data();
    if let Ok(mut cfg) = shared_config.write() {
        cfg.device_id = DEVICE_ID.to_string();
    }

    // ===============================
    // 2. Channels (Kênh Giao Tiếp)
    // ===============================
    let (conn_tx, conn_rx) = mpsc::channel::<ConnectionState>();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (fsm_tx, fsm_rx) = mpsc::channel::<String>();

    // ===============================
    // 3. Hardware & Engine Thread (FSM)
    // ===============================
    let pump_a = PinDriver::output(peripherals.pins.gpio4)?;
    let pump_b = PinDriver::output(peripherals.pins.gpio5)?;
    let pump_ph_up = PinDriver::output(peripherals.pins.gpio18)?;
    let pump_ph_down = PinDriver::output(peripherals.pins.gpio19)?;
    let l298n_in1 = PinDriver::output(peripherals.pins.gpio26)?;
    let l298n_in2 = PinDriver::output(peripherals.pins.gpio27)?;
    let l298n_in3 = PinDriver::output(peripherals.pins.gpio32)?;
    let l298n_in4 = PinDriver::output(peripherals.pins.gpio33)?;

    let pump_controller = PumpController::new(
        pump_a,
        pump_b,
        pump_ph_up,
        pump_ph_down,
        l298n_in1,
        l298n_in2,
        l298n_in3,
        l298n_in4,
    )?;

    start_fsm_control_loop(
        shared_config.clone(),
        shared_sensor_data.clone(),
        pump_controller,
        nvs.clone(),
        cmd_rx,
        fsm_tx, // FSM gửi trạng thái ra main loop
    );

    // ===============================
    // 4. WiFi Connect & Monitor
    // ===============================
    let mut wifi = EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs.clone()))?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASS.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;

    wifi.start()?;
    wifi.connect()?;

    // Luồng nền chuyên giám sát Wi-Fi và đẩy sự kiện
    // Luồng nền chuyên giám sát Wi-Fi và đẩy sự kiện
    let conn_tx_wifi = conn_tx.clone();
    thread::spawn(move || {
        let mut was_connected = false;
        loop {
            // SỬA Ở ĐÂY: Kiểm tra L2 (Đã kết nối vật lý)
            let is_l2_connected = wifi.is_connected().unwrap_or(false);

            // Kiểm tra L3 (Đã được cấp IP thực sự)
            let has_ip = wifi
                .sta_netif()
                .get_ip_info()
                .map(|info| !info.ip.is_unspecified()) // Đảm bảo IP không phải 0.0.0.0
                .unwrap_or(false);

            // Chỉ xác nhận là "CÓ MẠNG" khi cả 2 điều kiện đều đúng
            let is_fully_connected = is_l2_connected && has_ip;

            if is_fully_connected && !was_connected {
                let _ = conn_tx_wifi.send(ConnectionState::WifiConnected);
                was_connected = true;
            } else if !is_fully_connected && was_connected {
                let _ = conn_tx_wifi.send(ConnectionState::WifiDisconnected);
                was_connected = false;

                // Thử kết nối lại ngầm nếu rớt hẳn Wi-Fi
                if !is_l2_connected {
                    let _ = wifi.connect();
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    });

    // ===============================
    // 5. Main Connection Event Loop
    // ===============================
    let mut mqtt_client: Option<EspMqttClient> = None;
    let mut is_mqtt_connected = false;

    info!("🔄 Đang chạy Main Event Loop...");

    loop {
        // --- XỬ LÝ SỰ KIỆN KẾT NỐI MẠNG ---
        if let Ok(state) = conn_rx.try_recv() {
            match state {
                ConnectionState::WifiConnected => {
                    info!("🛜 Đã kết nối WiFi. Tiến hành khởi tạo MQTT...");
                    // Chỉ khởi tạo nếu chưa có client (tránh rò rỉ bộ nhớ khi rớt wifi kết nối lại)
                    if mqtt_client.is_none() {
                        match mqtt::init_mqtt_client(
                            MQTT_URL,
                            DEVICE_ID,
                            shared_config.clone(),
                            shared_sensor_data.clone(),
                            cmd_tx.clone(),
                            conn_tx.clone(),
                        ) {
                            Ok(client) => {
                                mqtt_client = Some(client);
                            }
                            Err(e) => error!("❌ Lỗi khởi tạo MQTT: {:?}", e),
                        }
                    }
                }
                ConnectionState::WifiDisconnected => {
                    warn!("⚠️ Rớt mạng WiFi!");
                    is_mqtt_connected = false;
                    // Tùy chọn: mqtt_client = None; nếu muốn khởi tạo lại hoàn toàn khi có mạng
                }
                ConnectionState::MqttConnected => {
                    info!("📡 MQTT Client báo cáo: ĐÃ KẾT NỐI THÀNH CÔNG");
                    is_mqtt_connected = true;

                    if let Some(client) = mqtt_client.as_mut() {
                        let topic_config = format!("AGITECH/{}/config", DEVICE_ID);
                        let topic_command = format!("AGITECH/{}/command", DEVICE_ID);
                        let topic_sensors = "AGITECH/sensor/+/data";

                        if let Err(e) = client.subscribe(&topic_config, QoS::AtLeastOnce) {
                            error!("❌ Lỗi subscribe config: {:?}", e);
                        }
                        if let Err(e) = client.subscribe(&topic_command, QoS::AtLeastOnce) {
                            error!("❌ Lỗi subscribe command: {:?}", e);
                        }
                        if let Err(e) = client.subscribe(topic_sensors, QoS::AtMostOnce) {
                            error!("❌ Lỗi subscribe sensors: {:?}", e);
                        }

                        info!("✅ Lệnh Subscribe đã được gửi thành công!");
                    }
                }
                ConnectionState::MqttDisconnected => {
                    warn!("📡 MQTT Client báo cáo: MẤT KẾT NỐI");
                    is_mqtt_connected = false;
                }
            }
        }

        // --- XỬ LÝ SỰ KIỆN TỪ FSM ĐỂ BÁO CÁO LÊN CLOUD ---
        if let Ok(payload) = fsm_rx.try_recv() {
            if is_mqtt_connected {
                if let Some(client) = mqtt_client.as_mut() {
                    let topic = format!("AGITECH/{}/fsm", DEVICE_ID);
                    if let Err(e) =
                        client.publish(&topic, QoS::AtLeastOnce, false, payload.as_bytes())
                    {
                        warn!("⚠️ Lỗi Publish FSM: {:?}", e);
                    } else {
                        info!("🚀 Published FSM: {}", payload);
                    }
                }
            } else {
                warn!("🗑️ Rớt mạng. Bỏ qua gói tin FSM: {}", payload);
            }
        }

        thread::sleep(Duration::from_millis(100)); // Nhường CPU
    }
}
