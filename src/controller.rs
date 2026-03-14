use esp_idf_hal::gpio::OutputPin;
use log::{error, info, warn};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Tùy theo cấu trúc project của bạn mà đường dẫn crate có thể khác đôi chút
use crate::config::{DeviceConfig, SharedConfig};
use crate::mqtt::SensorData;
use crate::pump::{PumpController, PumpType, WaterDirection};

// Kiểu dữ liệu dùng chung cho Cảm biến (Đọc từ MQTT/I2C)
pub type SharedSensorData = Arc<RwLock<SensorData>>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemState {
    Monitoring,
    EmergencyStop,
    WaterRefilling { target_level: f32, start_time: u64 },
    WaterDraining { target_level: f32, start_time: u64 },
    Mixing { finish_time: u64 },
    DosingEC { finish_time: u64 },
    DosingPH { finish_time: u64 },
}

pub struct ControlContext {
    pub current_state: SystemState,
    pub last_water_change_time: u64,
}

impl Default for ControlContext {
    fn default() -> Self {
        Self {
            current_state: SystemState::Monitoring,
            last_water_change_time: get_current_time_sec(),
        }
    }
}

// Hàm lấy Unix timestamp (giây) hiện tại
fn get_current_time_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// Luồng FSM (Máy trạng thái) chính - Đã cập nhật đủ 8 Generic type cho các chân GPIO
pub fn start_fsm_control_loop<PA, PB, PC, PD, PE, PF, PG, PH>(
    shared_config: SharedConfig,
    shared_sensors: SharedSensorData,
    mut pump_ctrl: PumpController<PA, PB, PC, PD, PE, PF, PG, PH>,
) where
    PA: OutputPin,
    PB: OutputPin,
    PC: OutputPin,
    PD: OutputPin,
    PE: OutputPin,
    PF: OutputPin,
    PG: OutputPin,
    PH: OutputPin,
{
    std::thread::spawn(move || {
        let mut ctx = ControlContext::default();
        info!("🚀 Bắt đầu chạy Máy trạng thái (FSM) Điều khiển trung tâm...");

        loop {
            // Nhường CPU 1 giây để MQTT và Wifi hoạt động
            std::thread::sleep(Duration::from_secs(1));

            // 1. Snapshot dữ liệu hiện tại
            let config = shared_config.read().unwrap().clone();
            let sensors = shared_sensors.read().unwrap().clone();
            let current_time = get_current_time_sec();

            // ==========================================
            // ƯU TIÊN 1: KIỂM TRA AN TOÀN TUYỆT ĐỐI
            // ==========================================
            let is_water_critical = sensors.water_level < config.water_level_critical_min;

            if config.emergency_shutdown || is_water_critical {
                if ctx.current_state != SystemState::EmergencyStop {
                    error!(
                        "⚠️ KÍCH HOẠT DỪNG KHẨN CẤP! (E-Stop: {}, Cạn đáy: {})",
                        config.emergency_shutdown, is_water_critical
                    );
                    let _ = pump_ctrl.stop_all(); // Đã bao gồm cả việc tắt Bơm Buồng Trộn
                    ctx.current_state = SystemState::EmergencyStop;
                }
                continue; // Kẹt ở đây, không chạy các bước dưới cho đến khi hết lỗi
            }

            // Phục hồi từ lỗi
            if ctx.current_state == SystemState::EmergencyStop {
                info!("✅ Hệ thống an toàn trở lại. Khôi phục trạng thái...");
                ctx.current_state = SystemState::Monitoring;
            }

            // Nếu người dùng tắt hệ thống (is_enabled = false)
            if !config.is_enabled {
                if ctx.current_state != SystemState::Monitoring {
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::Monitoring;
                }
                continue;
            }

            // ==========================================
            // LOGIC CHUYỂN TRẠNG THÁI CHÍNH
            // ==========================================
            match ctx.current_state {
                SystemState::Monitoring => {
                    // ƯU TIÊN 2.1: CẤP NƯỚC SẠCH
                    // ƯU TIÊN 2.0: LỊCH THAY NƯỚC ĐỊNH KỲ (SCHEDULE)
                    if config.scheduled_water_change_enabled
                        && (current_time - ctx.last_water_change_time
                            > config.water_change_interval_sec)
                    {
                        info!("⏰ Đến hạn thay nước định kỳ! Bắt đầu xả toàn bộ nước.");
                        ctx.last_water_change_time = current_time; // Reset bộ đếm
                        ctx.current_state = SystemState::WaterDraining {
                            target_level: config.water_level_min, // Xả xuống mức min (xả cạn)
                            start_time: current_time,
                        };
                        let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                        let _ = pump_ctrl.set_chamber_pump(false);

                    // ƯU TIÊN 2.1: CẤP NƯỚC SẠCH (Do cạn hoặc do vừa xả xong)
                    } else if config.auto_refill_enabled
                        && sensors.water_level
                            < (config.water_level_target - config.water_level_tolerance)
                    {
                        info!(
                            "Phát hiện nước thấp ({}cm). Bắt đầu cấp nước.",
                            sensors.water_level
                        );
                        ctx.current_state = SystemState::WaterRefilling {
                            target_level: config.water_level_target,
                            start_time: current_time,
                        };
                        let _ = pump_ctrl.set_water_pump(WaterDirection::In);
                        let _ = pump_ctrl.set_chamber_pump(false);

                    // ƯU TIÊN 2.2: XẢ NƯỚC CHỐNG TRÀN
                    } else if config.auto_drain_overflow
                        && sensors.water_level > config.water_level_max
                    {
                        warn!(
                            "Phát hiện nước ngập ({}cm). Bắt đầu xả tràn.",
                            sensors.water_level
                        );
                        ctx.current_state = SystemState::WaterDraining {
                            target_level: config.water_level_target, // Xả về mức chuẩn
                            start_time: current_time,
                        };
                        let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                        let _ = pump_ctrl.set_chamber_pump(false);

                    // ƯU TIÊN 2.3: PHA LOÃNG (DILUTION - KHI EC QUÁ ĐẶC)
                    } else if config.auto_dilute_enabled
                        && sensors.ec_value > (config.ec_target + config.ec_tolerance)
                    {
                        // Nếu EC quá đặc, ta tính mức nước cần xả đi
                        let drain_target = (sensors.water_level - config.dilute_drain_amount_cm)
                            .max(config.water_level_min); // Không xả thấp hơn ngưỡng an toàn

                        info!(
                            "EC quá đặc ({} > {}). Bắt đầu xả {}cm nước cũ để pha loãng.",
                            sensors.ec_value, config.ec_target, config.dilute_drain_amount_cm
                        );

                        ctx.current_state = SystemState::WaterDraining {
                            target_level: drain_target,
                            start_time: current_time,
                        };
                        let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                        let _ = pump_ctrl.set_chamber_pump(false);

                    // ƯU TIÊN 3: CHÂM DINH DƯỠNG (EC)
                    } else if sensors.ec_value < (config.ec_target - config.ec_tolerance) {
                        let ec_diff = config.ec_target - sensors.ec_value;
                        let ml_needed = (ec_diff / config.ec_gain_per_ml) * config.ec_step_ratio;
                        let mut dose_ml = ml_needed.min(config.max_dose_per_cycle);
                        if dose_ml < 0.0 {
                            dose_ml = 0.0;
                        }

                        let pump_duration_sec = (dose_ml / config.pump_capacity_ml_per_sec) as u64;

                        if pump_duration_sec > 0 {
                            info!("EC thấp ({} < {}). Chuyển sang DosingEC ({}s). BẬT BƠM BUỒNG TRỘN.", sensors.ec_value, config.ec_target, pump_duration_sec);
                            ctx.current_state = SystemState::DosingEC {
                                finish_time: current_time + pump_duration_sec,
                            };

                            let _ = pump_ctrl.set_chamber_pump(true); // Tạo dòng chảy mồi
                            let _ = pump_ctrl.set_pump_state(PumpType::NutrientA, true); // Nhỏ phân
                            let _ = pump_ctrl.set_pump_state(PumpType::NutrientB, true);
                        }

                    // ƯU TIÊN 4: ĐIỀU CHỈNH pH
                    } else if sensors.ph_value < (config.ph_target - config.ph_tolerance) {
                        // Tính toán ml tương tự (Ví dụ logic cơ bản cho thiếu pH)
                        let ph_diff = config.ph_target - sensors.ph_value;
                        let ml_needed =
                            (ph_diff / config.ph_shift_up_per_ml) * config.ph_step_ratio;
                        let dose_ml = ml_needed.min(config.max_dose_per_cycle);
                        let pump_duration_sec = (dose_ml / config.pump_capacity_ml_per_sec) as u64;

                        if pump_duration_sec > 0 {
                            info!(
                                "pH thấp. Chuyển sang DosingPH ({}s). BẬT BƠM BUỒNG TRỘN.",
                                pump_duration_sec
                            );
                            ctx.current_state = SystemState::DosingPH {
                                finish_time: current_time + pump_duration_sec,
                            };

                            let _ = pump_ctrl.set_chamber_pump(true); // Tạo dòng chảy mồi
                            let _ = pump_ctrl.set_pump_state(PumpType::PhUp, true);
                            // Nhỏ pH Up
                        }
                    } else {
                        // Trạng thái nghỉ ngơi, mọi thông số đều đẹp -> TẮT BƠM TRỘN tiết kiệm điện
                        let _ = pump_ctrl.set_chamber_pump(false);
                    }
                }

                // --- CÁC TRẠNG THÁI ĐANG THỰC THI THỜI GIAN THỰC ---
                SystemState::WaterRefilling {
                    target_level,
                    start_time,
                } => {
                    let run_duration = current_time - start_time;
                    let target_reached = sensors.water_level >= target_level;
                    let timeout = run_duration > config.max_refill_duration_sec;

                    if target_reached || timeout {
                        let _ = pump_ctrl.set_water_pump(WaterDirection::Stop);
                        if timeout {
                            warn!("Dừng cấp nước do quá thời gian!");
                        }

                        let _ = pump_ctrl.set_chamber_pump(true);
                        ctx.current_state = SystemState::Mixing {
                            finish_time: current_time + config.mixing_delay_sec,
                        };
                    }
                }

                SystemState::WaterDraining {
                    target_level,
                    start_time,
                } => {
                    let run_duration = current_time - start_time;
                    // Chú ý: xả nước thì sensor level sẽ giảm dần, nên so sánh là <=
                    let target_reached = sensors.water_level <= target_level;
                    let timeout = run_duration > config.max_drain_duration_sec;

                    if target_reached || timeout {
                        let _ = pump_ctrl.set_water_pump(WaterDirection::Stop);

                        // Sau khi xả nước xong, chuyển sang Mixing ngắn để chờ ổn định mặt nước
                        let _ = pump_ctrl.set_chamber_pump(true);
                        ctx.current_state = SystemState::Mixing {
                            finish_time: current_time + (config.mixing_delay_sec / 2),
                        };
                        // LƯU Ý: Sau khi Mixing xong quay về Monitoring.
                        // Tại Monitoring, nó sẽ thấy `water_level < target` và TỰ ĐỘNG nhảy sang WaterRefilling.
                        // Đây chính là chu trình "Xả đi rồi Cấp lại" hoàn hảo!
                    }
                }

                SystemState::DosingEC { finish_time } => {
                    if current_time >= finish_time {
                        // Hết thời gian châm -> Tắt bơm nhu động
                        let _ = pump_ctrl.set_pump_state(PumpType::NutrientA, false);
                        let _ = pump_ctrl.set_pump_state(PumpType::NutrientB, false);
                        info!("Châm EC xong. Chuyển sang Mixing (Đang súc rửa buồng trộn).");

                        // Chuyển sang Mixing (Bơm buồng trộn vẫn ĐANG CHẠY để rửa sạch ống)
                        ctx.current_state = SystemState::Mixing {
                            finish_time: current_time + config.mixing_delay_sec,
                        };
                    }
                }

                SystemState::DosingPH { finish_time } => {
                    if current_time >= finish_time {
                        let _ = pump_ctrl.set_pump_state(PumpType::PhUp, false);
                        let _ = pump_ctrl.set_pump_state(PumpType::PhDown, false);
                        info!("Chỉnh pH xong. Chuyển sang Mixing (Đang súc rửa buồng trộn).");

                        ctx.current_state = SystemState::Mixing {
                            finish_time: current_time + config.mixing_delay_sec,
                        };
                    }
                }

                SystemState::Mixing { finish_time } => {
                    // Trong suốt State này, `chamber_pump` đang chạy để cuốn hòa tan
                    if current_time >= finish_time {
                        info!("Hòa trộn xong. TẮT bơm buồng trộn và đo đạc lại.");
                        let _ = pump_ctrl.set_chamber_pump(false); // Kết thúc quy trình, tắt bơm
                        ctx.current_state = SystemState::Monitoring;
                    }
                }

                SystemState::EmergencyStop => {}
            }
        }
    });
}
