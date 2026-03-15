use esp_idf_hal::gpio::OutputPin;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use log::{error, info, warn};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Tùy theo cấu trúc project của bạn mà đường dẫn crate có thể khác đôi chút
use crate::config::{ControlMode, DeviceConfig, SharedConfig};
use crate::mqtt::{MqttCommandPayload, SensorData};
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
            last_water_change_time: 0,
        }
    }
}

fn get_current_time_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

pub fn start_fsm_control_loop<PA, PB, PC, PD, PE, PF, PG, PH>(
    shared_config: SharedConfig,
    shared_sensors: SharedSensorData,
    mut pump_ctrl: PumpController<PA, PB, PC, PD, PE, PF, PG, PH>,
    nvs_partition: EspDefaultNvsPartition,
    cmd_rx: Receiver<MqttCommandPayload>,
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

        let mut nvs = match EspNvs::new(nvs_partition, "agitech", true) {
            Ok(n) => Some(n),
            Err(e) => {
                error!("❌ Không thể mở NVS Namespace: {:?}", e);
                None
            }
        };

        let current_time_on_boot = get_current_time_sec();
        if let Some(ref mut flash) = nvs {
            match flash.get_u64("last_w_change") {
                Ok(Some(saved_time)) => {
                    ctx.last_water_change_time = saved_time;
                    info!(
                        "💾 Đã phục hồi mốc thay nước từ NVS: {} (Unix Timestamp)",
                        saved_time
                    );
                }
                _ => {
                    ctx.last_water_change_time = current_time_on_boot;
                    let _ = flash.set_u64("last_w_change", current_time_on_boot);
                    info!("💾 Lần đầu khởi động, đã tạo mốc thay nước mới vào NVS.");
                }
            }
        } else {
            ctx.last_water_change_time = current_time_on_boot;
        }

        info!("🚀 Bắt đầu chạy Máy trạng thái (FSM) Điều khiển trung tâm...");

        loop {
            // Nhường CPU 1 giây để MQTT và Wifi hoạt động
            std::thread::sleep(Duration::from_secs(1));

            // 1. Snapshot dữ liệu hiện tại
            let config = shared_config.read().unwrap().clone();
            let sensors = shared_sensors.read().unwrap().clone();
            let current_time = get_current_time_sec();

            while let Ok(cmd) = cmd_rx.try_recv() {
                if config.control_mode == ControlMode::Auto {
                    warn!(
                        "Bỏ qua lệnh thủ công ({}) vì hệ thống đang ở chế độ AUTO.",
                        cmd.pump
                    );
                    continue; // Nếu đang Auto thì phớt lờ lệnh Manual
                }

                // Nếu đang ở MANUAL, tiến hành điều khiển bơm
                let is_on = cmd.action == "pump_on";
                info!(
                    "🕹️ MANUAL MODE: Thực thi lệnh {} = {}",
                    cmd.pump,
                    if is_on { "BẬT" } else { "TẮT" }
                );

                let _ = match cmd.pump.as_str() {
                    "A" => pump_ctrl.set_pump_state(PumpType::NutrientA, is_on),
                    "B" => pump_ctrl.set_pump_state(PumpType::NutrientB, is_on),
                    "PH_UP" => pump_ctrl.set_pump_state(PumpType::PhUp, is_on),
                    "PH_DOWN" => pump_ctrl.set_pump_state(PumpType::PhDown, is_on),
                    "CIRCULATION" => pump_ctrl.set_chamber_pump(is_on),
                    "WATER_PUMP" => {
                        // Tạm quy ước: Manual bật Water_Pump -> Bơm vào. Tắt -> Stop.
                        // Nếu sau này Web gửi nút Drain riêng, ta sẽ thêm nhánh "WATER_DRAIN"
                        if is_on {
                            pump_ctrl.set_water_pump(WaterDirection::In)
                        } else {
                            pump_ctrl.set_water_pump(WaterDirection::Stop)
                        }
                    }
                    _ => {
                        warn!("Tên bơm không hợp lệ: {}", cmd.pump);
                        Ok(())
                    }
                };
            }

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
            if config.control_mode == ControlMode::Auto {
                match ctx.current_state {
                    SystemState::Monitoring => {
                        // ƯU TIÊN 2.0: LỊCH THAY NƯỚC ĐỊNH KỲ (SCHEDULE)
                        if config.scheduled_water_change_enabled
                            && (current_time - ctx.last_water_change_time
                                > config.water_change_interval_sec)
                        {
                            let drain_target = (sensors.water_level
                                - config.scheduled_drain_amount_cm)
                                .max(config.water_level_min);

                            info!(
                                "⏰ Đến lịch thay nước! Bắt đầu xả {:.1}cm nước cũ...",
                                config.scheduled_drain_amount_cm
                            );

                            ctx.last_water_change_time = current_time; // Reset bộ đếm cho chu kỳ tiếp theo

                            if let Some(ref mut flash) = nvs {
                                if let Err(e) = flash.set_u64("last_w_change", current_time) {
                                    error!("❌ Lỗi ghi NVS: {:?}", e);
                                } else {
                                    info!("💾 Đã cập nhật mốc thay nước mới vào Flash!");
                                }
                            }

                            ctx.current_state = SystemState::WaterDraining {
                                target_level: drain_target,
                                start_time: current_time,
                            };
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Out);

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
                            let drain_target = (sensors.water_level
                                - config.dilute_drain_amount_cm)
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
                            let ml_needed =
                                (ec_diff / config.ec_gain_per_ml) * config.ec_step_ratio;
                            let mut dose_ml = ml_needed.min(config.max_dose_per_cycle);
                            if dose_ml < 0.0 {
                                dose_ml = 0.0;
                            }

                            let pump_duration_sec =
                                (dose_ml / config.pump_capacity_ml_per_sec) as u64;

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
                            let pump_duration_sec =
                                (dose_ml / config.pump_capacity_ml_per_sec) as u64;

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
                        } else if sensors.ph_value > (config.ph_target - config.ph_tolerance) {
                            // Tính toán ml tương tự (Ví dụ logic cơ bản cho thiếu pH)
                            let ph_diff = config.ph_target - sensors.ph_value;
                            let ml_needed =
                                (ph_diff / config.ph_shift_down_per_ml) * config.ph_step_ratio;
                            let dose_ml = ml_needed.min(config.max_dose_per_cycle);
                            let pump_duration_sec =
                                (dose_ml / config.pump_capacity_ml_per_sec) as u64;

                            if pump_duration_sec > 0 {
                                info!(
                                    "pH cao. Chuyển sang DosingPH ({}s). BẬT BƠM BUỒNG TRỘN.",
                                    pump_duration_sec
                                );
                                ctx.current_state = SystemState::DosingPH {
                                    finish_time: current_time + pump_duration_sec,
                                };

                                let _ = pump_ctrl.set_chamber_pump(true); // Tạo dòng chảy mồi
                                let _ = pump_ctrl.set_pump_state(PumpType::PhDown, true);
                                // Nhỏ pH Down
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

                            info!(
                                "Bơm đầy. Chờ {}s để nước ổn định...",
                                config.mixing_delay_sec
                            );

                            let _ = pump_ctrl.set_chamber_pump(false); // Chắc chắn bơm buồng trộn ĐÃ TẮT
                            ctx.current_state = SystemState::Mixing {
                                finish_time: current_time + config.mixing_delay_sec,
                            };

                            ctx.current_state = SystemState::Monitoring;
                        }
                    }

                    SystemState::WaterDraining {
                        target_level,
                        start_time,
                    } => {
                        let run_duration = current_time - start_time;
                        let target_reached = sensors.water_level <= target_level;
                        let timeout = run_duration > config.max_drain_duration_sec;

                        if target_reached || timeout {
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Stop);

                            if timeout {
                                warn!("Dừng xả nước do quá thời gian!");
                            }

                            info!(
                                "Xả xong. Chờ {}s để mặt nước phẳng lại...",
                                config.mixing_delay_sec / 2
                            );

                            let _ = pump_ctrl.set_chamber_pump(false);
                            ctx.current_state = SystemState::Mixing {
                                finish_time: current_time + (config.mixing_delay_sec / 2),
                            };
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

                    SystemState::EmergencyStop => {
                        //không làm gì cả
                    }
                }
            } else {
                if ctx.current_state != SystemState::Monitoring {
                    info!("Chuyển sang chế độ MANUAL. Đang hủy các tiến trình tự động dở dang...");
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::Monitoring;
                }
            }
        }
    });
}
