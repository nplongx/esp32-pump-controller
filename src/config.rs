use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlMode {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub device_id: String,
    pub control_mode: ControlMode,
    pub is_enabled: bool,

    // --- 1. DEVICE CONFIG (Ngưỡng mục tiêu) ---
    pub ec_target: f32,
    pub ec_tolerance: f32,
    pub ph_target: f32,
    pub ph_tolerance: f32,

    // --- 2. WATER CONFIG (Nước) ---
    pub water_level_min: f32,
    pub water_level_target: f32,
    pub water_level_max: f32,
    pub water_level_tolerance: f32,
    pub auto_refill_enabled: bool,
    pub auto_drain_overflow: bool,

    // TÍNH NĂNG MỚI: PHA LOÃNG (DILUTION)
    pub auto_dilute_enabled: bool, // Bật/tắt tự động xả nước khi EC quá đặc
    pub dilute_drain_amount_cm: f32, // Mỗi lần pha loãng sẽ xả đi bao nhiêu cm nước (VD: 3.0 cm)

    // TÍNH NĂNG MỚI: LỊCH THAY NƯỚC (SCHEDULE - BÁN PHẦN)
    pub scheduled_water_change_enabled: bool,
    pub water_change_interval_sec: u64, // Bao lâu thay nước 1 lần (VD: 3 ngày = 259200s)
    pub scheduled_drain_amount_cm: f32, // Đến hạn thì xả đi bao nhiêu cm? (VD: 5.0 cm)

    // --- 3. SAFETY CONFIG (An toàn & Khẩn cấp) ---
    pub emergency_shutdown: bool,
    pub max_ec_limit: f32,
    pub min_ph_limit: f32,
    pub max_ph_limit: f32,
    pub max_ec_delta: f32,
    pub max_ph_delta: f32,
    pub max_dose_per_cycle: f32,
    pub water_level_critical_min: f32,
    pub max_refill_duration_sec: u64,
    pub max_drain_duration_sec: u64,

    // --- 4. DOSING & PUMP (Định lượng & Phần cứng) ---
    pub ec_gain_per_ml: f32,
    pub ph_shift_up_per_ml: f32,
    pub ph_shift_down_per_ml: f32,
    pub mixing_delay_sec: u64,
    pub ec_step_ratio: f32,
    pub ph_step_ratio: f32,
    pub pump_capacity_ml_per_sec: f32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            device_id: String::from("ESP32_PUMP_NODE"),
            control_mode: ControlMode::Auto,
            is_enabled: true,

            ec_target: 1.2,
            ec_tolerance: 0.05,
            ph_target: 6.0,
            ph_tolerance: 0.1,

            water_level_min: 15.0,
            water_level_target: 20.0,
            water_level_max: 24.0,
            water_level_tolerance: 1.0,
            auto_refill_enabled: true,
            auto_drain_overflow: true,

            // Cấu hình mới
            auto_dilute_enabled: true,
            dilute_drain_amount_cm: 2.0, // Rút đi 2cm nước cũ để châm thêm nước mới
            scheduled_water_change_enabled: false,
            water_change_interval_sec: 259200, // Mặc định 7 ngày
            scheduled_drain_amount_cm: 5.0,    // cm

            emergency_shutdown: false,
            max_ec_limit: 3.5,
            min_ph_limit: 4.0,
            max_ph_limit: 8.5,
            max_ec_delta: 1.0,
            max_ph_delta: 1.5,
            max_dose_per_cycle: 2.0,
            water_level_critical_min: 5.0,
            max_refill_duration_sec: 120,
            max_drain_duration_sec: 120,

            ec_gain_per_ml: 0.015,
            ph_shift_up_per_ml: 0.02,
            ph_shift_down_per_ml: 0.025,
            mixing_delay_sec: 300,
            ec_step_ratio: 0.4,
            ph_step_ratio: 0.2,
            pump_capacity_ml_per_sec: 0.00833,
        }
    }
}

pub type SharedConfig = Arc<RwLock<DeviceConfig>>;

pub fn create_shared_config() -> SharedConfig {
    Arc::new(RwLock::new(DeviceConfig::default()))
}
