use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, topic_safe_id, HassClient};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use mosquitto_rs::router::{Params, Payload, State};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Clone, Debug)]
pub struct FanConfig {
    #[serde(flatten)]
    pub base: EntityConfig,
    pub command_topic: String,
    pub state_topic: String,
    pub percentage_command_topic: String,
    pub percentage_state_topic: String,
    pub preset_mode_command_topic: String,
    pub preset_mode_state_topic: String,
    pub preset_modes: Vec<String>,
    pub oscillation_state_topic: String,
    pub payload_on: String,
    pub payload_off: String,
    pub payload_oscillation_on: String,
    pub payload_oscillation_off: String,
    pub payload_available: String,
    pub speed_range_min: u8,
    pub speed_range_max: u8,
    pub optimistic: bool,
}

#[derive(Clone)]
pub struct H7105Fan {
    config: FanConfig,
    device_id: String,
    state: StateHandle,
}

fn topic(device: &ServiceDevice, suffix: &str) -> String {
    format!("gv2mqtt/fan/{}/{suffix}", topic_safe_id(device))
}

impl H7105Fan {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        Self {
            config: FanConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: None,
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-fan", topic_safe_id(device)),
                    entity_category: None,
                    icon: Some("mdi:fan".to_string()),
                },
                command_topic: topic(device, "command"),
                state_topic: topic(device, "state"),
                percentage_command_topic: topic(device, "speed/command"),
                percentage_state_topic: topic(device, "speed/state"),
                preset_mode_command_topic: topic(device, "preset/command"),
                preset_mode_state_topic: topic(device, "preset/state"),
                preset_modes: vec!["Auto".to_string()],
                oscillation_state_topic: topic(device, "oscillation/state"),
                payload_on: "ON".to_string(),
                payload_off: "OFF".to_string(),
                payload_oscillation_on: "ON".to_string(),
                payload_oscillation_off: "OFF".to_string(),
                payload_available: "online".to_string(),
                speed_range_min: 1,
                speed_range_max: 12,
                optimistic: false,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for H7105Fan {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("fan", state, client, &self.config.base, &self.config).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        let fan = device.h7105_fan_state;
        let on = device.device_state().map(|state| state.on).unwrap_or(false);

        client
            .publish(&self.config.state_topic, if on { "ON" } else { "OFF" })
            .await?;
        if let Some(speed) = fan.speed {
            client
                .publish(&self.config.percentage_state_topic, speed.to_string())
                .await?;
        }
        client
            .publish(
                &self.config.preset_mode_state_topic,
                if fan.auto { "Auto" } else { "None" },
            )
            .await?;
        if let Some(oscillating) = fan.oscillating {
            client
                .publish(
                    &self.config.oscillation_state_topic,
                    if oscillating { "ON" } else { "OFF" },
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct FanId {
    id: String,
}

pub async fn mqtt_h7105_power(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let on = match payload.as_str() {
        "ON" => true,
        "OFF" => false,
        _ => anyhow::bail!("invalid H7105 power payload {payload:?}"),
    };
    state.h7105_set_power(&device, on).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

pub async fn mqtt_h7105_speed(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let speed: u8 = payload.parse()?;
    state.h7105_set_speed(&device, speed).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

pub async fn mqtt_h7105_preset(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    anyhow::ensure!(payload.eq_ignore_ascii_case("auto"), "unsupported H7105 preset {payload:?}");
    let device = state.resolve_device_for_control(&id).await?;
    state.h7105_set_auto(&device).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}
