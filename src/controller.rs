use esp_idf_hal::gpio::OutputPin;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use log::{error, info, warn};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{ControlMode, DeviceConfig, SharedConfig};
use crate::mqtt::{MqttCommandPayload, SensorData};
use crate::pump::{PumpController, PumpType, WaterDirection};

pub type SharedSensorData = Arc<RwLock<SensorData>>;

#[derive(Debug, Clone, PartialEq)]
pub enum SystemState {
    Monitoring,
    EmergencyStop,
    SystemFault(String),
    WaterRefilling { target_level: f32, start_time: u64 },
    WaterDraining { target_level: f32, start_time: u64 },
    DosingEC { finish_time: u64 },
    DosingPH { finish_time: u64 },
    ActiveMixing { finish_time: u64 },
    Stabilizing { finish_time: u64 },
}

impl SystemState {
    pub fn to_payload_string(&self) -> String {
        match self {
            SystemState::Monitoring => "Monitoring".to_string(),
            SystemState::EmergencyStop => "EmergencyStop".to_string(),
            SystemState::SystemFault(reason) => format!("SystemFault:{}", reason),
            SystemState::WaterRefilling { .. } => "WaterRefilling".to_string(),
            SystemState::WaterDraining { .. } => "WaterDraining".to_string(),
            SystemState::DosingEC { .. } => "DosingEC".to_string(),
            SystemState::DosingPH { .. } => "DosingPH".to_string(),
            SystemState::ActiveMixing { .. } => "ActiveMixing".to_string(),
            SystemState::Stabilizing { .. } => "Stabilizing".to_string(),
        }
    }
}

pub struct ControlContext {
    pub current_state: SystemState,
    pub last_water_change_time: u64,

    // Bộ đếm số lần bơm thử mà không có tác dụng
    pub ec_retry_count: u8,
    pub ph_retry_count: u8,
    pub water_refill_retry_count: u8,

    // Lưu giá trị trước khi bơm để so sánh (ACK)
    pub last_ec_before_dosing: Option<f32>,
    pub last_ph_before_dosing: Option<f32>,
    pub last_water_before_refill: Option<f32>,
}

impl Default for ControlContext {
    fn default() -> Self {
        Self {
            current_state: SystemState::Monitoring,
            last_water_change_time: 0,
            ec_retry_count: 0,
            ph_retry_count: 0,
            water_refill_retry_count: 0,
            last_ec_before_dosing: None,
            last_ph_before_dosing: None,
            last_water_before_refill: None,
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
    fsm_mqtt_tx: Sender<String>,
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
        let mut last_reported_state = "".to_string();

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
                    info!("💾 Đã phục hồi mốc thay nước từ NVS: {}", saved_time);
                }
                _ => {
                    ctx.last_water_change_time = current_time_on_boot;
                    let _ = flash.set_u64("last_w_change", current_time_on_boot);
                    info!("💾 Lần đầu khởi động, đã tạo mốc thay nước mới.");
                }
            }
        } else {
            ctx.last_water_change_time = current_time_on_boot;
        }

        info!("🚀 Bắt đầu chạy Máy trạng thái (FSM) Điều khiển trung tâm...");

        loop {
            std::thread::sleep(Duration::from_secs(1));

            let config = shared_config.read().unwrap().clone();
            let sensors = shared_sensors.read().unwrap().clone();
            let current_time = get_current_time_sec();

            // ==========================================
            // XỬ LÝ LỆNH TỪ MQTT
            // ==========================================
            while let Ok(cmd) = cmd_rx.try_recv() {
                if cmd.action == "reset_fault" {
                    info!("🔄 Nhận lệnh Reset. Xóa bộ đếm lỗi và khôi phục hệ thống...");
                    ctx.ec_retry_count = 0;
                    ctx.ph_retry_count = 0;
                    ctx.water_refill_retry_count = 0;
                    ctx.last_ec_before_dosing = None;
                    ctx.last_ph_before_dosing = None;
                    ctx.last_water_before_refill = None;
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::Monitoring;
                    continue;
                }

                if config.control_mode == ControlMode::Auto {
                    warn!("Bỏ qua lệnh thủ công ({}) vì đang ở AUTO.", cmd.pump);
                    continue;
                }

                let is_on = cmd.action == "pump_on";
                info!(
                    "🕹️ MANUAL MODE: Lệnh {} = {}",
                    cmd.pump,
                    if is_on { "BẬT" } else { "TẮT" }
                );

                let _ = match cmd.pump.as_str() {
                    "A" => pump_ctrl.set_pump_state(PumpType::NutrientA, is_on),
                    "B" => pump_ctrl.set_pump_state(PumpType::NutrientB, is_on),
                    "PH_UP" => pump_ctrl.set_pump_state(PumpType::PhUp, is_on),
                    "PH_DOWN" => pump_ctrl.set_pump_state(PumpType::PhDown, is_on),
                    "CHAMBER_PUMP" => pump_ctrl.set_chamber_pump(is_on),
                    "WATER_PUMP" => {
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
            // KIỂM TRA AN TOÀN TUYỆT ĐỐI (Ưu tiên 1)
            // ==========================================
            let is_water_critical = sensors.water_level < config.water_level_critical_min;

            if config.emergency_shutdown || is_water_critical {
                if ctx.current_state != SystemState::EmergencyStop {
                    error!(
                        "⚠️ DỪNG KHẨN CẤP! (E-Stop: {}, Cạn đáy: {})",
                        config.emergency_shutdown, is_water_critical
                    );
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::EmergencyStop;
                }
            } else if !config.is_enabled {
                if ctx.current_state != SystemState::Monitoring {
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::Monitoring;
                }
            } else if ctx.current_state == SystemState::EmergencyStop {
                info!("✅ Hệ thống an toàn trở lại. Khôi phục trạng thái...");
                ctx.current_state = SystemState::Monitoring;
            } else if config.control_mode == ControlMode::Auto {
                // ==========================================
                // VÒNG LẶP FSM TỰ ĐỘNG
                // ==========================================
                match ctx.current_state {
                    SystemState::SystemFault(ref reason) => {
                        warn!("🚨 HỆ THỐNG ĐANG BÁO LỖI: [{}]. Đang chờ reset từ MQTT (lệnh 'reset_fault')...", reason);
                    }

                    SystemState::Monitoring => {
                        // ====================================================
                        // KIỂM TRA SỰ THAY ĐỔI ĐỂ RESET HOẶC TĂNG BỘ ĐẾM LỖI (ACK)
                        // ====================================================

                        // 1. Kiểm tra biến động EC
                        if let Some(last_ec) = ctx.last_ec_before_dosing {
                            if (sensors.ec_value - last_ec).abs() >= 0.05 {
                                // Ngưỡng nhận diện có thay đổi
                                ctx.ec_retry_count = 0;
                            } else {
                                ctx.ec_retry_count += 1;
                            }
                            ctx.last_ec_before_dosing = None; // Reset sau khi check
                        }

                        // 2. Kiểm tra biến động pH
                        if let Some(last_ph) = ctx.last_ph_before_dosing {
                            if (sensors.ph_value - last_ph).abs() >= 0.1 {
                                ctx.ph_retry_count = 0;
                            } else {
                                ctx.ph_retry_count += 1;
                            }
                            ctx.last_ph_before_dosing = None;
                        }

                        // 3. Kiểm tra biến động Mực Nước
                        if let Some(last_water) = ctx.last_water_before_refill {
                            if (sensors.water_level - last_water).abs() >= 0.5 {
                                ctx.water_refill_retry_count = 0;
                            } else {
                                ctx.water_refill_retry_count += 1;
                            }
                            ctx.last_water_before_refill = None;
                        }

                        // ====================================================
                        // LOGIC ĐIỀU KHIỂN XẢ / CẤP / CHÂM
                        // ====================================================
                        if config.scheduled_water_change_enabled
                            && (current_time - ctx.last_water_change_time
                                > config.water_change_interval_sec)
                        {
                            let drain_target = (sensors.water_level
                                - config.scheduled_drain_amount_cm)
                                .max(config.water_level_min);
                            info!(
                                "⏰ Lịch thay nước: Xả {:.1}cm nước cũ...",
                                config.scheduled_drain_amount_cm
                            );
                            ctx.last_water_change_time = current_time;
                            if let Some(ref mut flash) = nvs {
                                let _ = flash.set_u64("last_w_change", current_time);
                            }
                            ctx.current_state = SystemState::WaterDraining {
                                target_level: drain_target,
                                start_time: current_time,
                            };
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                        } else if config.auto_refill_enabled
                            && sensors.water_level
                                < (config.water_level_target - config.water_level_tolerance)
                        {
                            if ctx.water_refill_retry_count >= 3 {
                                error!("🚨 FAILED ACK: Bơm cấp nước 3 lần nhưng mực nước không tăng. Ngắt hệ thống!");
                                let _ = pump_ctrl.stop_all();
                                ctx.current_state =
                                    SystemState::SystemFault("WATER_REFILL_FAILED".to_string());
                            } else {
                                ctx.last_water_before_refill = Some(sensors.water_level); // Lưu vết
                                info!(
                                    "💧 Nước thấp ({}cm). Bắt đầu cấp nước (Lần thử {}/3).",
                                    sensors.water_level,
                                    ctx.water_refill_retry_count + 1
                                );
                                ctx.current_state = SystemState::WaterRefilling {
                                    target_level: config.water_level_target,
                                    start_time: current_time,
                                };
                                let _ = pump_ctrl.set_water_pump(WaterDirection::In);
                                let _ = pump_ctrl.set_chamber_pump(false);
                            }
                        } else if config.auto_drain_overflow
                            && sensors.water_level > config.water_level_max
                        {
                            warn!("Nước ngập ({}cm). Bắt đầu xả tràn.", sensors.water_level);
                            ctx.current_state = SystemState::WaterDraining {
                                target_level: config.water_level_target,
                                start_time: current_time,
                            };
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                            let _ = pump_ctrl.set_chamber_pump(false);
                        } else if config.auto_dilute_enabled
                            && sensors.ec_value > (config.ec_target + config.ec_tolerance)
                        {
                            let drain_target = (sensors.water_level
                                - config.dilute_drain_amount_cm)
                                .max(config.water_level_min);
                            info!(
                                "EC đặc. Xả {}cm nước cũ để pha loãng.",
                                config.dilute_drain_amount_cm
                            );
                            ctx.current_state = SystemState::WaterDraining {
                                target_level: drain_target,
                                start_time: current_time,
                            };
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                            let _ = pump_ctrl.set_chamber_pump(false);
                        } else if sensors.ec_value < (config.ec_target - config.ec_tolerance) {
                            if ctx.ec_retry_count >= 3 {
                                error!("🚨 FAILED ACK: Bơm EC 3 lần nhưng chỉ số không đổi. Ngắt hệ thống!");
                                let _ = pump_ctrl.stop_all();
                                ctx.current_state =
                                    SystemState::SystemFault("EC_DOSING_FAILED".to_string());
                            } else {
                                ctx.last_ec_before_dosing = Some(sensors.ec_value); // Lưu vết

                                let ec_diff = config.ec_target - sensors.ec_value;
                                let ml_needed =
                                    (ec_diff / config.ec_gain_per_ml) * config.ec_step_ratio;
                                let mut dose_ml = ml_needed.min(config.max_dose_per_cycle);
                                if dose_ml < 0.0 {
                                    dose_ml = 0.0;
                                }
                                let pump_duration_sec =
                                    (dose_ml / config.dosing_pump_capacity_ml_per_sec) as u64;

                                if pump_duration_sec > 0 {
                                    info!(
                                        "🧪 EC thấp. Bơm DosingEC ({}s) - Lần thử {}/3.",
                                        pump_duration_sec,
                                        ctx.ec_retry_count + 1
                                    );
                                    ctx.current_state = SystemState::DosingEC {
                                        finish_time: current_time + pump_duration_sec,
                                    };
                                    let _ = pump_ctrl.set_chamber_pump(true);
                                    let _ = pump_ctrl.set_pump_state(PumpType::NutrientA, true);
                                    let _ = pump_ctrl.set_pump_state(PumpType::NutrientB, true);
                                }
                            }
                        } else if sensors.ph_value < (config.ph_target - config.ph_tolerance)
                            || sensors.ph_value > (config.ph_target + config.ph_tolerance)
                        {
                            if ctx.ph_retry_count >= 3 {
                                error!("🚨 FAILED ACK: Bơm pH 3 lần nhưng chỉ số không đổi. Ngắt hệ thống!");
                                let _ = pump_ctrl.stop_all();
                                ctx.current_state =
                                    SystemState::SystemFault("PH_DOSING_FAILED".to_string());
                            } else {
                                ctx.last_ph_before_dosing = Some(sensors.ph_value); // Lưu vết

                                let (is_ph_up, ph_diff) = if sensors.ph_value < config.ph_target {
                                    (true, config.ph_target - sensors.ph_value)
                                } else {
                                    (false, sensors.ph_value - config.ph_target)
                                };

                                let ratio = if is_ph_up {
                                    config.ph_shift_up_per_ml
                                } else {
                                    config.ph_shift_down_per_ml
                                };
                                let ml_needed = (ph_diff / ratio) * config.ph_step_ratio;
                                let dose_ml = ml_needed.min(config.max_dose_per_cycle);
                                let pump_duration_sec =
                                    (dose_ml / config.dosing_pump_capacity_ml_per_sec) as u64;

                                if pump_duration_sec > 0 {
                                    info!(
                                        "⚖️ pH sai lệch. Bơm DosingPH ({}s) - Lần thử {}/3.",
                                        pump_duration_sec,
                                        ctx.ph_retry_count + 1
                                    );
                                    ctx.current_state = SystemState::DosingPH {
                                        finish_time: current_time + pump_duration_sec,
                                    };
                                    let _ = pump_ctrl.set_chamber_pump(true);
                                    if is_ph_up {
                                        let _ = pump_ctrl.set_pump_state(PumpType::PhUp, true);
                                    } else {
                                        let _ = pump_ctrl.set_pump_state(PumpType::PhDown, true);
                                    }
                                }
                            }
                        } else {
                            let _ = pump_ctrl.set_chamber_pump(false);
                        }
                    }

                    SystemState::WaterRefilling {
                        target_level,
                        start_time,
                    } => {
                        if sensors.water_level >= target_level
                            || (current_time - start_time) > config.max_refill_duration_sec
                        {
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Stop);
                            let _ = pump_ctrl.set_chamber_pump(true);
                            ctx.current_state = SystemState::ActiveMixing {
                                finish_time: current_time + config.active_mixing_sec,
                            };
                        }
                    }

                    SystemState::WaterDraining {
                        target_level,
                        start_time,
                    } => {
                        if sensors.water_level <= target_level
                            || (current_time - start_time) > config.max_drain_duration_sec
                        {
                            let _ = pump_ctrl.set_water_pump(WaterDirection::Stop);
                            let _ = pump_ctrl.set_chamber_pump(false);
                            ctx.current_state = SystemState::Stabilizing {
                                finish_time: current_time + config.sensor_stabilize_sec,
                            };
                        }
                    }

                    SystemState::DosingEC { finish_time } => {
                        if current_time >= finish_time {
                            let _ = pump_ctrl.set_pump_state(PumpType::NutrientA, false);
                            let _ = pump_ctrl.set_pump_state(PumpType::NutrientB, false);
                            ctx.current_state = SystemState::ActiveMixing {
                                finish_time: current_time + config.active_mixing_sec,
                            };
                        }
                    }

                    SystemState::DosingPH { finish_time } => {
                        if current_time >= finish_time {
                            let _ = pump_ctrl.set_pump_state(PumpType::PhUp, false);
                            let _ = pump_ctrl.set_pump_state(PumpType::PhDown, false);
                            ctx.current_state = SystemState::ActiveMixing {
                                finish_time: current_time + config.active_mixing_sec,
                            };
                        }
                    }

                    SystemState::ActiveMixing { finish_time } => {
                        if current_time >= finish_time {
                            info!("🌀 Đã khuấy xong dung dịch. Chờ nước tĩnh...");
                            let _ = pump_ctrl.set_chamber_pump(false);
                            ctx.current_state = SystemState::Stabilizing {
                                finish_time: current_time + config.sensor_stabilize_sec,
                            };
                        }
                    }

                    SystemState::Stabilizing { finish_time } => {
                        if current_time >= finish_time {
                            info!("📊 Nước đã tĩnh. Đọc lại cảm biến để xác nhận (ACK).");
                            ctx.current_state = SystemState::Monitoring;
                        }
                    }

                    SystemState::EmergencyStop => {}
                }
            } else if !matches!(
                ctx.current_state,
                SystemState::Monitoring | SystemState::SystemFault(_)
            ) {
                info!("Chuyển sang chế độ MANUAL. Đang hủy các tiến trình tự động dở dang...");
                let _ = pump_ctrl.stop_all();
                ctx.current_state = SystemState::Monitoring;
            }

            // ==========================================
            // XUẤT TRẠNG THÁI RA MQTT CHO MAIN LUỒNG
            // ==========================================
            let current_state_str = ctx.current_state.to_payload_string();

            if current_state_str != last_reported_state {
                let fsm_payload = format!(r#"{{"current_state": "{}"}}"#, current_state_str);

                if let Err(e) = fsm_mqtt_tx.send(fsm_payload) {
                    error!("Lỗi gửi trạng thái FSM qua channel: {:?}", e);
                } else {
                    info!(
                        "📡 Đã báo cáo MQTT: Trạng thái hệ thống chuyển sang [{}]",
                        current_state_str
                    );
                }

                last_reported_state = current_state_str.to_string();
            }
        }
    });
}
