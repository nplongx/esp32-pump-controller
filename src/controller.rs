use crate::config::{ControlMode, SharedConfig};
use crate::mqtt::SharedSensorData;
use crate::pump::{PumpController, PumpType};
use esp_idf_hal::gpio::OutputPin;
use log::{info, warn};
use std::thread;
use std::time::Duration;

pub fn start_control_loop<A: OutputPin, B: OutputPin, C: OutputPin, D: OutputPin>(
    shared_config: SharedConfig,
    mut pump_controller: PumpController<A, B, C, D>,
    shared_sensor_data: SharedSensorData, // Nhận thêm pointer của Sensor Data
) {
    thread::spawn(move || {
        info!("Bắt đầu luồng Controller Loop...");

        loop {
            let config = {
                if let Ok(lock) = shared_config.read() {
                    lock.clone()
                } else {
                    warn!("Không thể đọc cấu hình, thử lại sau...");
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };

            if !config.is_enabled {
                let _ = pump_controller.stop_all();
                thread::sleep(Duration::from_secs(5));
                continue;
            }

            if config.control_mode == ControlMode::Manual {
                thread::sleep(Duration::from_secs(2));
                continue;
            }
            let current_ec = shared_sensor_data.read().unwrap().ec_value;
            let current_ph = shared_sensor_data.read().unwrap().ph_value;
            let current_temp = shared_sensor_data.read().unwrap().temp_value;

            info!(
                "Auto Mode | EC: {:.2} | pH: {:.2} | Temp: {:.1}°C",
                current_ec, current_ph, current_temp
            );

            let mut is_pumping = false;
            if current_ec < (config.ec_target - config.ec_tolerance) {
                warn!(
                    "EC thấp ({} < {}). Đang bù dinh dưỡng...",
                    current_ec, config.ec_target
                );

                let _ = pump_controller.pulse_pump(PumpType::NutrientA, 2000);
                let _ = pump_controller.pulse_pump(PumpType::NutrientB, 2000);
                is_pumping = true;
            }
            if current_ph < (config.ph_min - config.ph_tolerance) {
                warn!(
                    "pH thấp ({} < {}). Đang bơm pH Up...",
                    current_ph, config.ph_min
                );
                let _ = pump_controller.pulse_pump(PumpType::PhUp, 1500);
                is_pumping = true;
            }
            // Theo thuật toán: Bơm pH Down nếu pH > pH_max + tolerance [cite: 520-523]
            else if current_ph > (config.ph_max + config.ph_tolerance) {
                warn!(
                    "pH cao ({} > {}). Đang bơm pH Down...",
                    current_ph, config.ph_max
                );
                let _ = pump_controller.pulse_pump(PumpType::PhDown, 1500);
                is_pumping = true;
            }

            if is_pumping {
                info!("Đang chờ dung dịch hòa trộn vào bể...");
                thread::sleep(Duration::from_secs(60));
            } else {
                thread::sleep(Duration::from_secs(5));
            }
        }
    });
}
