use crate::config::{DeviceConfig, SharedConfig};
use esp_idf_svc::mqtt::client::{EspMqttClient, EventPayload, MqttClientConfiguration, QoS};
use log::{error, info, warn};

pub fn init_mqtt_client(
    broker_url: &str,
    device_id: &str,
    shared_config: SharedConfig,
    shared_sensor_data: SharedSensorData,
) -> anyhow::Result<EspMqttClient<'static>> {
    let client_id = device_id.to_string();

    let topic_config = format!("farm/{}/config", client_id);
    let topic_sensors = format!("farm/{}/sensors", client_id);
    let topic_config_cb = topic_config.clone();
    let topic_sensors_cb = topic_sensors.clone();

    let mqtt_config = MqttClientConfiguration {
        client_id: Some(&client_id),
        keep_alive_interval: Some(std::time::Duration::from_secs(60)),
        ..Default::default()
    };

    let mut client = EspMqttClient::new_cb(broker_url, &mqtt_config, move |event| {
        match event.payload() {
            EventPayload::Connected(_sessin) => {
                info!("Đã kết nối đến MQTT Broker.");
            }
            EventPayload::Received { topic, data, .. } => {
                if topic == Some(&topic_config_cb) {
                    match serde_json::from_slice::<DeviceConfig>(data) {
                        Ok(new_config) => {
                            if let Ok(mut config) = shared_config.write() {
                                *config = new_config;
                                info!("Đã cập nhật cấu hình runtime từ MQTT!");
                            }
                        }
                        Err(e) => error!("Lỗi parse JSON cấu hình: {}", e),
                    }
                } else if topic == Some(&topic_sensors_cb) {
                    match serde_json::from_slice::<SensorData>(data) {
                        Ok(new_sensors) => {
                            if let Ok(mut sensors) = shared_sensor_data.write() {
                                *sensors = new_sensors;
                            }
                        }
                        Err(e) => error!("Lỗi parse JSON dữ liệu cảm biến: {}", e),
                    }
                }
            }
            EventPayload::Disconnected => warn!("Mất kết nối với MQTT Broker."),
            _ => {}
        }
    })?;

    client.subscribe(&topic_config, QoS::AtLeastOnce)?;
    client.subscribe(&topic_sensors, QoS::AtMostOnce)?;
    info!(
        "Đã subscribe các topics: {}, {}",
        topic_config, topic_sensors
    );

    Ok(client)
}

use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorData {
    pub ec_value: f32,
    pub ph_value: f32,
    pub temp_value: f32,
}

impl Default for SensorData {
    fn default() -> Self {
        Self {
            ec_value: 0.0,
            ph_value: 7.0,
            temp_value: 25.0,
        }
    }
}

pub type SharedSensorData = Arc<RwLock<SensorData>>;

pub fn create_shared_sensor_data() -> SharedSensorData {
    Arc::new(RwLock::new(SensorData::default()))
}
