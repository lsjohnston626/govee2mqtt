use crate::ble::{H7105FanMode, H7105OscillationConfig};
use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::hass_mqtt::number::NumberConfig;
use crate::hass_mqtt::select::SelectConfig;
use crate::hass_mqtt::switch::SwitchConfig;
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
    pub oscillation_command_topic: String,
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
                preset_modes: H7105FanMode::ALL
                    .into_iter()
                    .map(|mode| mode.name().to_string())
                    .collect(),
                oscillation_command_topic: topic(device, "oscillation/command"),
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
        if let Some(mode) = fan.mode {
            client
                .publish(&self.config.preset_mode_state_topic, mode.name())
                .await?;
        }
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
    let mode = H7105FanMode::from_name(&payload)?;
    let device = state.resolve_device_for_control(&id).await?;
    state.h7105_set_mode(&device, mode).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

pub async fn mqtt_h7105_oscillation(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let oscillating = match payload.as_str() {
        "ON" => true,
        "OFF" => false,
        _ => anyhow::bail!("invalid H7105 oscillation payload {payload:?}"),
    };
    let device = state.resolve_device_for_control(&id).await?;
    state.h7105_set_oscillation(&device, oscillating).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

#[derive(Clone, Copy)]
pub enum H7105OscillationSide {
    Start,
    End,
}

impl H7105OscillationSide {
    fn topic_name(self) -> &'static str {
        match self {
            Self::Start => "left",
            Self::End => "right",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Start => "Oscillation Start Angle",
            Self::End => "Oscillation End Angle",
        }
    }
}

pub struct H7105OscillationAngle {
    config: NumberConfig,
    device_id: String,
    state: StateHandle,
    side: H7105OscillationSide,
}

impl H7105OscillationAngle {
    pub fn all(device: &ServiceDevice, state: &StateHandle) -> [Self; 2] {
        [
            Self::new(device, state, H7105OscillationSide::Start),
            Self::new(device, state, H7105OscillationSide::End),
        ]
    }

    fn new(device: &ServiceDevice, state: &StateHandle, side: H7105OscillationSide) -> Self {
        let side_name = side.topic_name();
        Self {
            config: NumberConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(side.display_name().to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!(
                        "gv2mqtt-{}-oscillation-{side_name}-angle",
                        topic_safe_id(device)
                    ),
                    entity_category: None,
                    icon: Some("mdi:angle-acute".to_string()),
                },
                command_topic: topic(device, &format!("oscillation-angle/{side_name}/command")),
                state_topic: Some(topic(
                    device,
                    &format!("oscillation-angle/{side_name}/state"),
                )),
                min: Some(-75.0),
                max: Some(75.0),
                step: 5.0,
                unit_of_measurement: Some("deg"),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            side,
        }
    }
}

#[async_trait]
impl EntityInstance for H7105OscillationAngle {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        if let Some(params) = device.h7105_fan_state.oscillation_params {
            let config = H7105OscillationConfig::from_params(params)?;
            let value = match self.side {
                H7105OscillationSide::Start => config.start_degrees,
                H7105OscillationSide::End => config.end_degrees,
            };
            self.config.notify_state(client, &value.to_string()).await?;
        }
        Ok(())
    }
}

pub struct H7105OscillationSpeed {
    config: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl H7105OscillationSpeed {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        Self {
            config: SelectConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Oscillation Speed".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-oscillation-speed", topic_safe_id(device)),
                    entity_category: None,
                    icon: Some("mdi:speedometer".to_string()),
                },
                command_topic: topic(device, "oscillation/speed/command"),
                state_topic: topic(device, "oscillation/speed/state"),
                options: vec!["Low".to_string(), "High".to_string()],
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for H7105OscillationSpeed {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        if let Some(params) = device.h7105_fan_state.oscillation_params {
            let speed = H7105OscillationConfig::from_params(params)?.speed;
            client
                .publish(&self.config.state_topic, oscillation_speed_name(speed)?)
                .await?;
        }
        Ok(())
    }
}

fn oscillation_speed_name(speed: u8) -> anyhow::Result<&'static str> {
    match speed {
        1 => Ok("Low"),
        3 => Ok("High"),
        _ => anyhow::bail!("invalid H7105 oscillation speed {speed}"),
    }
}

pub struct H7105OscillationSymmetric {
    config: SwitchConfig,
    device_id: String,
    state: StateHandle,
}

impl H7105OscillationSymmetric {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        Self {
            config: SwitchConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Symmetric Oscillation".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-oscillation-symmetric", topic_safe_id(device)),
                    entity_category: None,
                    icon: Some("mdi:arrow-left-right".to_string()),
                },
                command_topic: topic(device, "oscillation/symmetric/command"),
                state_topic: topic(device, "oscillation/symmetric/state"),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for H7105OscillationSymmetric {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        if let Some(params) = device.h7105_fan_state.oscillation_params {
            let symmetric = H7105OscillationConfig::from_params(params)?.flags == 0;
            client
                .publish(
                    &self.config.state_topic,
                    if symmetric { "ON" } else { "OFF" },
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct FanOscillationAngleParams {
    id: String,
    side: String,
}

pub async fn mqtt_h7105_oscillation_angle(
    Payload(value): Payload<i8>,
    Params(FanOscillationAngleParams { id, side }): Params<FanOscillationAngleParams>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        (-75..=75).contains(&value) && value % 5 == 0,
        "invalid H7105 angle"
    );
    let device = state.resolve_device_for_control(&id).await?;
    let fan = device.h7105_fan_state;
    let mut config = H7105OscillationConfig::from_params(
        fan.oscillation_params
            .ok_or_else(|| anyhow::anyhow!("H7105 oscillation parameters unavailable"))?,
    )?;
    match side.as_str() {
        "left" => {
            config.start_degrees = value;
            if config.flags == 0 {
                config.end_degrees = -value;
            }
        }
        "right" => {
            config.end_degrees = value;
            if config.flags == 0 {
                config.start_degrees = -value;
            }
        }
        _ => anyhow::bail!("invalid H7105 oscillation side {side:?}"),
    }
    state
        .h7105_set_oscillation_config(&device, fan.oscillating.unwrap_or(false), config)
        .await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

pub async fn mqtt_h7105_oscillation_speed(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let speed = match payload.as_str() {
        "Low" => 1,
        "High" => 3,
        _ => anyhow::bail!("invalid H7105 oscillation speed {payload:?}"),
    };
    let device = state.resolve_device_for_control(&id).await?;
    let fan = device.h7105_fan_state;
    let mut config = H7105OscillationConfig::from_params(
        fan.oscillation_params
            .ok_or_else(|| anyhow::anyhow!("H7105 oscillation parameters unavailable"))?,
    )?;
    config.speed = speed;
    state
        .h7105_set_oscillation_config(&device, fan.oscillating.unwrap_or(false), config)
        .await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

pub async fn mqtt_h7105_oscillation_symmetric(
    Payload(payload): Payload<String>,
    Params(FanId { id }): Params<FanId>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let symmetric = match payload.as_str() {
        "ON" => true,
        "OFF" => false,
        _ => anyhow::bail!("invalid H7105 symmetry payload {payload:?}"),
    };
    let device = state.resolve_device_for_control(&id).await?;
    let fan = device.h7105_fan_state;
    let mut config = H7105OscillationConfig::from_params(
        fan.oscillation_params
            .ok_or_else(|| anyhow::anyhow!("H7105 oscillation parameters unavailable"))?,
    )?;
    config.flags = if symmetric { 0 } else { 1 };
    if symmetric {
        config.start_degrees = -25;
        config.end_degrees = 25;
    }
    state
        .h7105_set_oscillation_config(&device, fan.oscillating.unwrap_or(false), config)
        .await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}
