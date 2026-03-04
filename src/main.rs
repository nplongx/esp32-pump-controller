use esp_idf_hal::gpio::PinDriver;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use log::{error, info};
use std::thread;
use std::time::Duration;

mod config;
mod controller;
mod mqtt;
mod pump;

use config::create_shared_config;
use controller::start_control_loop;
use pump::PumpController;

use crate::mqtt::create_shared_sensor_data;

const WIFI_SSID: &str = "YOUR_WIFI_SSID";
const WIFI_PASS: &str = "YOUR_WIFI_PASSWORD";
const MQTT_URL: &str = "mqtt://broker.emqx.io:1883";
const DEVICE_ID: &str = "ESP32_AGITECH_001";

fn main() -> anyhow::Result<()> {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    info!("Khởi động hệ thống điều khiển thủy canh Agitech...");

    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

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

    let shared_config = create_shared_config();
    let shared_sensor_data = create_shared_sensor_data();

    if let Ok(mut cfg) = shared_config.write() {
        cfg.device_id = DEVICE_ID.to_string();
    }

    let _mqtt_client = match mqtt::init_mqtt_client(
        MQTT_URL,
        DEVICE_ID,
        shared_config.clone(),
        shared_sensor_data.clone(),
    ) {
        Ok(client) => {
            info!("Khởi tạo MQTT Client thành công.");
            Some(client)
        }
        Err(e) => {
            error!("Lỗi khởi tạo MQTT: {:?}", e);
            None
        }
    };

    let pump_a_pin = PinDriver::output(peripherals.pins.gpio4)?;
    let pump_b_pin = PinDriver::output(peripherals.pins.gpio5)?;
    let pump_ph_up_pin = PinDriver::output(peripherals.pins.gpio18)?;
    let pump_ph_down_pin = PinDriver::output(peripherals.pins.gpio19)?;

    let pump_controller =
        PumpController::new(pump_a_pin, pump_b_pin, pump_ph_up_pin, pump_ph_down_pin)?;

    start_control_loop(
        shared_config.clone(),
        pump_controller,
        shared_sensor_data.clone(),
    );

    loop {
        thread::sleep(Duration::from_secs(10));
        info!("Hệ thống đang hoạt động...");
    }
}
