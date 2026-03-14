use esp_idf_hal::gpio::PinDriver;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use log::{error, info};
use std::thread;
use std::time::Duration;

// Khai báo các module trong dự án
mod config;
mod controller;
mod mqtt;
mod pump;

use config::create_shared_config;
use controller::start_fsm_control_loop;
use mqtt::create_shared_sensor_data;
use pump::PumpController;

// Thông tin cấu hình mạng và MQTT
const WIFI_SSID: &str = "YOUR_WIFI_SSID";
const WIFI_PASS: &str = "YOUR_WIFI_PASSWORD";
const MQTT_URL: &str = "mqtt://broker.emqx.io:1883"; // Thay bằng broker thật của bạn
const DEVICE_ID: &str = "ESP32_AGITECH_001";

fn main() -> anyhow::Result<()> {
    // 1. KHỞI TẠO HỆ THỐNG CƠ BẢN
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    info!("🚀 Khởi động hệ thống FSM Thủy canh Agitech (Phiên bản Buồng Trộn)...");

    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // 2. KẾT NỐI WI-FI
    let mut wifi = EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs.clone()))?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASS.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?;
    info!("Đang kết nối Wi-Fi...");

    thread::sleep(Duration::from_secs(5));
    info!("Trạng thái Wi-Fi: {:?}", wifi.is_connected());

    // 3. KHỞI TẠO BỘ NHỚ DÙNG CHUNG (SHARED STATE)
    let shared_config = create_shared_config();
    let shared_sensor_data = create_shared_sensor_data();

    if let Ok(mut cfg) = shared_config.write() {
        cfg.device_id = DEVICE_ID.to_string();
    }

    // 4. KHỞI TẠO MQTT CLIENT
    let _mqtt_client = match mqtt::init_mqtt_client(
        MQTT_URL,
        DEVICE_ID,
        shared_config.clone(),
        shared_sensor_data.clone(),
    ) {
        Ok(client) => {
            info!("✅ Khởi tạo MQTT Client thành công.");
            Some(client)
        }
        Err(e) => {
            error!("❌ Lỗi khởi tạo MQTT: {:?}", e);
            None
        }
    };

    // 5. CẤU HÌNH PHẦN CỨNG (GPIO)
    info!("Đang cấu hình 8 chân GPIO cho hệ thống máy bơm...");

    // --- Nhóm 1: 4 Bơm Nhu Động (Châm phân bón & pH) ---
    let pump_a_pin = PinDriver::output(peripherals.pins.gpio4)?;
    let pump_b_pin = PinDriver::output(peripherals.pins.gpio5)?;
    let pump_ph_up_pin = PinDriver::output(peripherals.pins.gpio18)?;
    let pump_ph_down_pin = PinDriver::output(peripherals.pins.gpio19)?;

    // --- Nhóm 2: Module Cầu H L298N (Điều khiển 2 động cơ nước) ---
    // Động cơ A: Bơm Đảo Chiều (Cấp/Xả nước từ bồn chính) -> Nối IN1, IN2
    let l298n_in1_pin = PinDriver::output(peripherals.pins.gpio26)?;
    let l298n_in2_pin = PinDriver::output(peripherals.pins.gpio27)?;

    // Động cơ B: Bơm Buồng Trộn (Hút từ bồn chính sang buồng trộn) -> Nối IN3, IN4
    let l298n_in3_pin = PinDriver::output(peripherals.pins.gpio32)?;
    let l298n_in4_pin = PinDriver::output(peripherals.pins.gpio33)?;

    // Giao phần cứng cho PumpController quản lý (Truyền đủ 8 chân)
    let pump_controller = PumpController::new(
        pump_a_pin,
        pump_b_pin,
        pump_ph_up_pin,
        pump_ph_down_pin,
        l298n_in1_pin,
        l298n_in2_pin,
        l298n_in3_pin,
        l298n_in4_pin,
    )?;

    // 6. KHỞI CHẠY MÁY TRẠNG THÁI (FSM) ĐIỀU KHIỂN
    // Chuyển giao config, sensor và cụm điều khiển bơm cho luồng FSM chạy ngầm
    start_fsm_control_loop(
        shared_config.clone(),
        shared_sensor_data.clone(),
        pump_controller,
    );

    // 7. VÒNG LẶP CHÍNH CỦA THIẾT BỊ
    loop {
        thread::sleep(Duration::from_secs(10)); // Cứ 10 giây log trạng thái ra màn hình 1 lần

        if let Ok(sensors) = shared_sensor_data.read() {
            info!(
                "📊 [TRẠNG THÁI] Nước: {:.1}cm | EC: {:.2} | pH: {:.2}",
                sensors.water_level, sensors.ec_value, sensors.ph_value
            );
        }
    }
}
