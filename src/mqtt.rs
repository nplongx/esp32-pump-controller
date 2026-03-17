use crate::config::{DeviceConfig, SharedConfig};
use esp_idf_svc::mqtt::client::{EspMqttClient, EventPayload, MqttClientConfiguration, QoS};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::{mpsc::Sender, Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectionState {
    WifiConnected,
    WifiDisconnected,
    MqttConnected,
    MqttDisconnected,
}

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

#[derive(Debug, Deserialize, Clone)]
pub struct MqttCommandPayload {
    pub action: String,
    pub pump: String,
    pub duration_sec: Option<u64>,
}

pub fn init_mqtt_client(
    broker_url: &str,
    device_id: &str,
    shared_config: SharedConfig,
    shared_sensor_data: SharedSensorData,
    cmd_tx: Sender<MqttCommandPayload>,
    conn_tx: Sender<ConnectionState>,
) -> anyhow::Result<EspMqttClient<'static>> {
    info!("🚀 Initializing MQTT client...");
    info!("Broker: {}", broker_url);
    info!("Device ID: {}", device_id);

    let client_id = device_id.to_string();
    let topic_config = format!("AGITECH/{}/config", client_id);
    let topic_command = format!("AGITECH/{}/command", client_id);
    let topic_sensors_wildcard = "AGITECH/sensor/+/data".to_string();

    info!("Subscribing topics:");
    info!("Config: {}", topic_config);
    info!("Command: {}", topic_command);
    info!("Sensors: {}", topic_sensors_wildcard);

    let topic_config_cb = topic_config.clone();
    let topic_command_cb = topic_command.clone();

    let mqtt_config = MqttClientConfiguration {
        client_id: Some(&client_id),
        buffer_size: 4096,
        keep_alive_interval: Some(std::time::Duration::from_secs(60)),
        ..Default::default()
    };

    let client = EspMqttClient::new_cb(broker_url, &mqtt_config, move |event| {
        debug!("📩 MQTT Event Received");

        match event.payload() {
            EventPayload::Connected(_) => {
                info!("✅ MQTT Broker Callback: Connected");

                if let Err(e) = conn_tx.send(ConnectionState::MqttConnected) {
                    error!("Failed to send MQTT connected state: {:?}", e);
                }
            }

            EventPayload::Disconnected => {
                warn!("⚠️ MQTT Broker Callback: Disconnected");

                if let Err(e) = conn_tx.send(ConnectionState::MqttDisconnected) {
                    error!("Failed to send MQTT disconnected state: {:?}", e);
                }
            }

            EventPayload::Received { topic, data, .. } => {
                let topic_str = topic.unwrap_or("");

                debug!(
                    "📥 MQTT message received | topic: {} | size: {} bytes",
                    topic_str,
                    data.len()
                );

                // ---- CONFIG UPDATE ----
                if topic_str == topic_config_cb {
                    debug!("⚙️ Processing CONFIG update");

                    match serde_json::from_slice::<DeviceConfig>(data) {
                        Ok(new_config) => {
                            info!("📦 New config received: {:?}", new_config);

                            if let Ok(mut config) = shared_config.write() {
                                *config = new_config;
                                info!("✅ Device config updated");
                            } else {
                                error!("❌ Failed to acquire config write lock");
                            }
                        }

                        Err(e) => {
                            error!("❌ Config JSON parse error: {:?}", e);
                        }
                    }
                }
                // ---- COMMAND ----
                else if topic_str == topic_command_cb {
                    debug!("🎮 Processing COMMAND");

                    match serde_json::from_slice::<MqttCommandPayload>(data) {
                        Ok(cmd) => {
                            info!("🎯 Command received: {:?}", cmd);

                            if let Err(e) = cmd_tx.send(cmd) {
                                error!("❌ Failed to forward command: {:?}", e);
                            }
                        }

                        Err(e) => {
                            error!("❌ Command JSON parse error: {:?}", e);
                        }
                    }
                }
                // ---- SENSOR DATA ----
                else if topic_str.starts_with("AGITECH/sensor/") {
                    debug!("📊 Processing SENSOR data");

                    // Debug raw payload
                    if let Ok(payload_str) = std::str::from_utf8(data) {
                        debug!("📦 Raw sensor payload: {}", payload_str);
                    }

                    match serde_json::from_slice::<IncomingSensorPayload>(data) {
                        Ok(payload) => {
                            debug!(
                                "Parsed sensor payload -> value: {} unit: {} ts: {}",
                                payload.value, payload.unit, payload.timestamp
                            );

                            // Parse topic: AGITECH/sensor/<type>/data
                            let parts: Vec<&str> = topic_str.split('/').collect();

                            if parts.len() >= 4 {
                                let sensor_type = parts[2].to_ascii_lowercase();

                                if let Ok(mut sensors) = shared_sensor_data.write() {
                                    match sensor_type.as_str() {
                                        "ec" => {
                                            sensors.ec_value = payload.value;
                                            info!("🌱 EC updated -> {}", sensors.ec_value);
                                        }
                                        "ph" => {
                                            sensors.ph_value = payload.value;
                                            info!("🧪 PH updated -> {}", sensors.ph_value);
                                        }
                                        "temp" => {
                                            sensors.temp_value = payload.value;
                                            info!("🌡 TEMP updated -> {}", sensors.temp_value);
                                        }
                                        "water" | "water_level" => {
                                            sensors.water_level = payload.value;
                                            info!(
                                                "💧 Water level updated -> {}",
                                                sensors.water_level
                                            );
                                        }
                                        other => {
                                            warn!("⚠️ Unknown sensor type: {}", other);
                                        }
                                    }
                                } else {
                                    error!("❌ Failed to acquire sensor write lock");
                                }
                            } else {
                                warn!("⚠️ Invalid sensor topic format: {}", topic_str);
                            }
                        }

                        Err(e) => {
                            error!("❌ Sensor JSON parse error: {:?}", e);

                            if let Ok(payload_str) = std::str::from_utf8(data) {
                                error!("Payload received: {}", payload_str);
                            }
                        }
                    }
                }
            }

            other => {
                debug!("Other MQTT event: {:?}", other);
            }
        }
    })?;

    info!("✅ MQTT client initialized");

    Ok(client)
}
