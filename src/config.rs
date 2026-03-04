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
    pub ec_target: f32,
    pub ec_tolerance: f32,
    pub ph_min: f32,
    pub ph_max: f32,
    pub ph_tolerance: f32,
    pub temp_min: f32,
    pub temp_max: f32,
    pub control_mode: ControlMode,
    pub is_enabled: bool,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            device_id: String::from("ESP32_UNASSIGNED"),
            ec_target: 1.2,
            ec_tolerance: 0.05,
            ph_min: 5.5,
            ph_max: 6.5,
            ph_tolerance: 0.05,
            temp_min: 18.0,
            temp_max: 24.0,
            control_mode: ControlMode::Manual,
            is_enabled: false,
        }
    }
}

// Định nghĩa type alias để dễ sử dụng ở các module khác
// Arc: Atomic Reference Counting (Cho phép chia sẻ reference an toàn giữa các thread)
// RwLock: Read-Write Lock (Nhiều luồng có thể đọc cùng lúc, nhưng chỉ 1 luồng được ghi)
pub type SharedConfig = Arc<RwLock<DeviceConfig>>;

// Hàm helper để khởi tạo SharedConfig trong main.rs
pub fn create_shared_config() -> SharedConfig {
    Arc::new(RwLock::new(DeviceConfig::default()))
}
