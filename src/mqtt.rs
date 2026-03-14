use crate::config::{DeviceConfig, SharedConfig};
use esp_idf_svc::mqtt::client::{EspMqttClient, EventPayload, MqttClientConfiguration, QoS};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Debug, Deserialize)]
pub struct IncomingSensorPayload {
    pub value: f32,
    pub unit: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorData {
    pub ec_value: f32,
    pub ph_value: f32,
    pub temp_value: f32,
    pub water_level: f32,
}

impl Default for SensorData {
    fn default() -> Self {
        Self {
            ec_value: 0.0,
            ph_value: 7.0,
            temp_value: 25.0,
            water_level: 20.0,
        }
    }
}

pub type SharedSensorData = Arc<RwLock<SensorData>>;

pub fn create_shared_sensor_data() -> SharedSensorData {
    Arc::new(RwLock::new(SensorData::default()))
}

pub fn init_mqtt_client(
    broker_url: &str,
    device_id: &str,
    shared_config: SharedConfig,
    shared_sensor_data: SharedSensorData,
) -> anyhow::Result<EspMqttClient<'static>> {
    let client_id = device_id.to_string();

    let topic_config = format!("AGITECH/{}/config", client_id);
    let topic_sensors_wildcard = "AGITECH/sensor/+/data".to_string();

    let topic_config_cb = topic_config.clone();

    let mqtt_config = MqttClientConfiguration {
        client_id: Some(&client_id),
        keep_alive_interval: Some(std::time::Duration::from_secs(60)),
        ..Default::default()
    };

    let mut client = EspMqttClient::new_cb(broker_url, &mqtt_config, move |event| {
        match event.payload() {
            EventPayload::Connected(_) => info!("Đã kết nối đến MQTT Broker."),
            EventPayload::Received { topic, data, .. } => {
                let topic_str = topic.unwrap_or("");

                if topic_str == topic_config_cb {
                    match serde_json::from_slice::<DeviceConfig>(data) {
                        Ok(new_config) => {
                            if let Ok(mut config) = shared_config.write() {
                                *config = new_config;
                                info!("Đã cập nhật cấu hình runtime từ MQTT!");
                            }
                        }
                        Err(e) => error!("Lỗi parse JSON cấu hình: {}", e),
                    }
                } else if topic_str.starts_with("AGITECH/sensor/") && topic_str.ends_with("/data") {
                    if let Ok(payload) = serde_json::from_slice::<IncomingSensorPayload>(data) {
                        if let Ok(mut sensors) = shared_sensor_data.write() {
                            if topic_str.contains("/ec/") {
                                sensors.ec_value = payload.value;
                            } else if topic_str.contains("/ph/") {
                                sensors.ph_value = payload.value;
                            } else if topic_str.contains("/temp/") {
                                sensors.temp_value = payload.value;
                            }
                        }
                    }
                }
            }
            EventPayload::Disconnected => warn!("Mất kết nối với MQTT Broker."),
            _ => {}
        }
    })?;

    client.subscribe(&topic_config, QoS::AtLeastOnce)?;
    client.subscribe(&topic_sensors_wildcard, QoS::AtMostOnce)?;
    info!(
        "Đã subscribe các topics: {}, {}",
        topic_config, topic_sensors_wildcard
    );

    Ok(client)
}
