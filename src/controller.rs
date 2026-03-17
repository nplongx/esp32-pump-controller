use esp_idf_hal::gpio::OutputPin;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};
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

    pub ec_retry_count: u8,
    pub ph_retry_count: u8,
    pub water_refill_retry_count: u8,

    pub last_ec_before_dosing: Option<f32>,
    pub last_ph_before_dosing: Option<f32>,
    pub last_water_before_refill: Option<f32>,

    pub previous_ec_value: Option<f32>,
    pub previous_ph_value: Option<f32>,
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
            previous_ec_value: None,
            previous_ph_value: None,
        }
    }
}

// ==========================================
// CÁC HÀM XỬ LÝ LOGIC CỦA CONTROL CONTEXT
// ==========================================
impl ControlContext {
    /// Xóa toàn bộ bộ đếm lỗi để reset hệ thống
    fn reset_faults(&mut self) {
        self.ec_retry_count = 0;
        self.ph_retry_count = 0;
        self.water_refill_retry_count = 0;
        self.last_ec_before_dosing = None;
        self.last_ph_before_dosing = None;
        self.last_water_before_refill = None;
        self.current_state = SystemState::Monitoring;
    }

    /// Lọc nhiễu cảm biến dựa vào Delta. Trả về `true` nếu có nhiễu.
    fn check_and_update_noise(&mut self, sensors: &SensorData, config: &DeviceConfig) -> bool {
        let mut is_noisy = false;

        if let Some(prev_ec) = self.previous_ec_value {
            if (sensors.ec_value - prev_ec).abs() > config.max_ec_delta {
                warn!("⚠️ Nhiễu EC. Bỏ qua nhịp này!");
                is_noisy = true;
            }
        }
        if let Some(prev_ph) = self.previous_ph_value {
            if (sensors.ph_value - prev_ph).abs() > config.max_ph_delta {
                warn!("⚠️ Nhiễu pH. Bỏ qua nhịp này!");
                is_noisy = true;
            }
        }

        self.previous_ec_value = Some(sensors.ec_value);
        self.previous_ph_value = Some(sensors.ph_value);
        is_noisy
    }

    /// Kiểm tra xem cảm biến có thay đổi sau khi bơm không (ACK)
    fn verify_sensor_ack(&mut self, sensors: &SensorData) {
        if let Some(last_ec) = self.last_ec_before_dosing {
            if (sensors.ec_value - last_ec).abs() >= 0.05 {
                self.ec_retry_count = 0;
            } else {
                self.ec_retry_count += 1;
            }
            self.last_ec_before_dosing = None;
        }

        if let Some(last_ph) = self.last_ph_before_dosing {
            if (sensors.ph_value - last_ph).abs() >= 0.1 {
                self.ph_retry_count = 0;
            } else {
                self.ph_retry_count += 1;
            }
            self.last_ph_before_dosing = None;
        }

        if let Some(last_water) = self.last_water_before_refill {
            if (sensors.water_level - last_water).abs() >= 0.5 {
                self.water_refill_retry_count = 0;
            } else {
                self.water_refill_retry_count += 1;
            }
            self.last_water_before_refill = None;
        }
    }
}

fn get_current_time_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// ==========================================
// VÒNG LẶP ĐIỀU KHIỂN CHÍNH
// ==========================================
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

        // 1. Khởi tạo NVS và phục hồi mốc thời gian
        let mut nvs = EspNvs::new(nvs_partition, "agitech", true).ok();
        let current_time_on_boot = get_current_time_sec();

        ctx.last_water_change_time = nvs
            .as_mut()
            .and_then(|flash| flash.get_u64("last_w_change").unwrap_or(None))
            .unwrap_or_else(|| {
                if let Some(flash) = nvs.as_mut() {
                    let _ = flash.set_u64("last_w_change", current_time_on_boot);
                }
                current_time_on_boot
            });

        info!("🚀 Bắt đầu chạy Máy trạng thái (FSM) Điều khiển trung tâm...");

        loop {
            std::thread::sleep(Duration::from_secs(1));

            let config = shared_config.read().unwrap().clone();
            let sensors = shared_sensors.read().unwrap().clone();
            let current_time = get_current_time_sec();

            // 2. Xử lý lệnh từ MQTT (Manual & Reset)
            process_mqtt_commands(&cmd_rx, &config, &mut pump_ctrl, &mut ctx);

            // 3. Lọc Nhiễu Cảm Biến
            if ctx.check_and_update_noise(&sensors, &config)
                && config.control_mode == ControlMode::Auto
            {
                continue; // Bỏ qua nhịp này chờ cảm biến ổn định
            }

            // 4. Kiểm Tra An Toàn Tuyệt Đối (E-Stop)
            let is_water_critical = sensors.water_level < config.water_level_critical_min;
            let is_ec_out_of_bounds =
                sensors.ec_value < config.min_ec_limit || sensors.ec_value > config.max_ec_limit;
            let is_ph_out_of_bounds =
                sensors.ph_value < config.min_ph_limit || sensors.ph_value > config.max_ph_limit;

            if config.emergency_shutdown
                || is_water_critical
                || is_ec_out_of_bounds
                || is_ph_out_of_bounds
            {
                if ctx.current_state != SystemState::EmergencyStop {
                    error!(
                        "⚠️ DỪNG KHẨN CẤP! E-Stop: {}, Cạn đáy: {}, Lỗi EC: {}, Lỗi pH: {}",
                        config.emergency_shutdown,
                        is_water_critical,
                        is_ec_out_of_bounds,
                        is_ph_out_of_bounds
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
                // 5. Chạy Máy Trạng Thái (FSM Auto Mode)
                run_auto_fsm(
                    current_time,
                    &config,
                    &sensors,
                    &mut ctx,
                    &mut pump_ctrl,
                    &mut nvs,
                );
            } else if !matches!(
                ctx.current_state,
                SystemState::Monitoring | SystemState::SystemFault(_)
            ) {
                // Fallback nếu đang chạy Auto mà bị user ép sang Manual
                info!("Chuyển sang chế độ MANUAL. Hủy tiến trình tự động dở dang...");
                let _ = pump_ctrl.stop_all();
                ctx.current_state = SystemState::Monitoring;
            }

            // 6. Cập nhật MQTT nếu state thay đổi
            let current_state_str = ctx.current_state.to_payload_string();
            if current_state_str != last_reported_state {
                let payload = format!(r#"{{"current_state": "{}"}}"#, current_state_str);
                if fsm_mqtt_tx.send(payload).is_ok() {
                    info!("📡 Trạng thái hệ thống chuyển sang [{}]", current_state_str);
                }
                last_reported_state = current_state_str;
            }
        }
    });
}

// ==========================================
// HÀM HỖ TRỢ: XỬ LÝ LỆNH MQTT
// ==========================================
fn process_mqtt_commands<PA, PB, PC, PD, PE, PF, PG, PH>(
    cmd_rx: &Receiver<MqttCommandPayload>,
    config: &DeviceConfig,
    pump_ctrl: &mut PumpController<PA, PB, PC, PD, PE, PF, PG, PH>,
    ctx: &mut ControlContext,
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
    while let Ok(cmd) = cmd_rx.try_recv() {
        if cmd.action == "reset_fault" {
            info!("🔄 Nhận lệnh Reset. Khôi phục hệ thống...");
            let _ = pump_ctrl.stop_all();
            ctx.reset_faults();
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
            "WATER_PUMP" => pump_ctrl.set_water_pump(if is_on {
                WaterDirection::In
            } else {
                WaterDirection::Stop
            }),
            "DRAIN_PUMP" => pump_ctrl.set_water_pump(if is_on {
                WaterDirection::Out
            } else {
                WaterDirection::Stop
            }),
            _ => {
                warn!("Tên bơm không hợp lệ");
                Ok(())
            }
        };
    }
}

// ==========================================
// HÀM HỖ TRỢ: CHẠY LOGIC AUTO FSM
// ==========================================
fn run_auto_fsm<PA, PB, PC, PD, PE, PF, PG, PH>(
    current_time: u64,
    config: &DeviceConfig,
    sensors: &SensorData,
    ctx: &mut ControlContext,
    pump_ctrl: &mut PumpController<PA, PB, PC, PD, PE, PF, PG, PH>,
    nvs: &mut Option<EspDefaultNvs>,
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
    match ctx.current_state {
        SystemState::SystemFault(ref reason) => {
            warn!(
                "🚨 BÁO LỖI: [{}]. Chờ reset (lệnh 'reset_fault')...",
                reason
            );
        }

        SystemState::Monitoring => {
            ctx.verify_sensor_ack(sensors);

            // 1. Kiểm tra lịch thay nước
            if config.scheduled_water_change_enabled
                && (current_time - ctx.last_water_change_time > config.water_change_interval_sec)
            {
                let target = (sensors.water_level - config.scheduled_drain_amount_cm)
                    .max(config.water_level_min);
                info!("⏰ Xả {:.1}cm nước cũ...", config.scheduled_drain_amount_cm);

                ctx.last_water_change_time = current_time;
                if let Some(flash) = nvs.as_mut() {
                    let _ = flash.set_u64("last_w_change", current_time);
                }

                ctx.current_state = SystemState::WaterDraining {
                    target_level: target,
                    start_time: current_time,
                };
                let _ = pump_ctrl.set_water_pump(WaterDirection::Out);

            // 2. Cấp nước tự động
            } else if config.auto_refill_enabled
                && sensors.water_level < (config.water_level_target - config.water_level_tolerance)
            {
                if ctx.water_refill_retry_count >= 3 {
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::SystemFault("WATER_REFILL_FAILED".to_string());
                } else {
                    ctx.last_water_before_refill = Some(sensors.water_level);
                    ctx.current_state = SystemState::WaterRefilling {
                        target_level: config.water_level_target,
                        start_time: current_time,
                    };
                    let _ = pump_ctrl.set_water_pump(WaterDirection::In);
                    let _ = pump_ctrl.set_chamber_pump(false);
                }

            // 3. Xả tràn tự động
            } else if config.auto_drain_overflow && sensors.water_level > config.water_level_max {
                ctx.current_state = SystemState::WaterDraining {
                    target_level: config.water_level_target,
                    start_time: current_time,
                };
                let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                let _ = pump_ctrl.set_chamber_pump(false);

            // 4. Pha loãng EC
            } else if config.auto_dilute_enabled
                && sensors.ec_value > (config.ec_target + config.ec_tolerance)
            {
                let target = (sensors.water_level - config.dilute_drain_amount_cm)
                    .max(config.water_level_min);
                ctx.current_state = SystemState::WaterDraining {
                    target_level: target,
                    start_time: current_time,
                };
                let _ = pump_ctrl.set_water_pump(WaterDirection::Out);
                let _ = pump_ctrl.set_chamber_pump(false);

            // 5. Châm EC
            } else if sensors.ec_value < (config.ec_target - config.ec_tolerance) {
                if ctx.ec_retry_count >= 3 {
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::SystemFault("EC_DOSING_FAILED".to_string());
                } else {
                    ctx.last_ec_before_dosing = Some(sensors.ec_value);
                    let dose_ml = ((config.ec_target - sensors.ec_value) / config.ec_gain_per_ml
                        * config.ec_step_ratio)
                        .clamp(0.0, config.max_dose_per_cycle);
                    let duration = (dose_ml / config.dosing_pump_capacity_ml_per_sec) as u64;

                    if duration > 0 {
                        ctx.current_state = SystemState::DosingEC {
                            finish_time: current_time + duration,
                        };
                        let _ = pump_ctrl.set_chamber_pump(true);
                        let _ = pump_ctrl.set_pump_state(PumpType::NutrientA, true);
                        let _ = pump_ctrl.set_pump_state(PumpType::NutrientB, true);
                    }
                }

            // 6. Chỉnh pH
            } else if (sensors.ph_value - config.ph_target).abs() > config.ph_tolerance {
                if ctx.ph_retry_count >= 3 {
                    let _ = pump_ctrl.stop_all();
                    ctx.current_state = SystemState::SystemFault("PH_DOSING_FAILED".to_string());
                } else {
                    ctx.last_ph_before_dosing = Some(sensors.ph_value);
                    let is_ph_up = sensors.ph_value < config.ph_target;
                    let diff = (sensors.ph_value - config.ph_target).abs();
                    let ratio = if is_ph_up {
                        config.ph_shift_up_per_ml
                    } else {
                        config.ph_shift_down_per_ml
                    };
                    let dose_ml =
                        (diff / ratio * config.ph_step_ratio).clamp(0.0, config.max_dose_per_cycle);
                    let duration = (dose_ml / config.dosing_pump_capacity_ml_per_sec) as u64;

                    if duration > 0 {
                        ctx.current_state = SystemState::DosingPH {
                            finish_time: current_time + duration,
                        };
                        let _ = pump_ctrl.set_chamber_pump(true);
                        let _ = pump_ctrl.set_pump_state(
                            if is_ph_up {
                                PumpType::PhUp
                            } else {
                                PumpType::PhDown
                            },
                            true,
                        );
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
                let _ = pump_ctrl.set_chamber_pump(false);
                ctx.current_state = SystemState::Stabilizing {
                    finish_time: current_time + config.sensor_stabilize_sec,
                };
            }
        }

        SystemState::Stabilizing { finish_time } => {
            if current_time >= finish_time {
                ctx.current_state = SystemState::Monitoring;
            }
        }

        SystemState::EmergencyStop => {}
    }
}
