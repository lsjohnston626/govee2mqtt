use crate::ble::{h7105_celsius_to_fahrenheit_hundredths, H7105FanMode, H7105OscillationConfig};
use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::hass_mqtt::number::NumberConfig;
use crate::hass_mqtt::select::SelectConfig;
use crate::hass_mqtt::sensor::SensorConfig;
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
                step: 0.5,
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
                H7105OscillationSide::Start => config.start_degrees(),
                H7105OscillationSide::End => config.end_degrees(),
            };
            self.config
                .notify_state(client, &format_h7105_angle(value))
                .await?;
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

fn h7105_angle_to_tenths(value: f32) -> anyhow::Result<i16> {
    anyhow::ensure!(value.is_finite(), "invalid H7105 angle");
    let tenths = (value * 10.0).round();
    anyhow::ensure!(
        (tenths - value * 10.0).abs() < 0.01
            && (-750.0..=750.0).contains(&tenths)
            && (tenths as i16) % 5 == 0,
        "H7105 angles must be -75 through 75 degrees in half-degree steps"
    );
    Ok(tenths as i16)
}

fn format_h7105_angle(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
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
    Payload(value): Payload<f32>,
    Params(FanOscillationAngleParams { id, side }): Params<FanOscillationAngleParams>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let value_tenths = h7105_angle_to_tenths(value)?;
    let device = state.resolve_device_for_control(&id).await?;
    let fan = device.h7105_fan_state;
    let mut config = H7105OscillationConfig::from_params(
        fan.oscillation_params
            .ok_or_else(|| anyhow::anyhow!("H7105 oscillation parameters unavailable"))?,
    )?;
    match side.as_str() {
        "left" => {
            config.start_tenths = value_tenths;
            if config.flags == 0 {
                config.end_tenths = -value_tenths;
            }
        }
        "right" => {
            config.end_tenths = value_tenths;
            if config.flags == 0 {
                config.start_tenths = -value_tenths;
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
        let half_span = (config.end_tenths - config.start_tenths) / 2;
        config.start_tenths = -half_span;
        config.end_tenths = half_span;
    }
    state
        .h7105_set_oscillation_config(&device, fan.oscillating.unwrap_or(false), config)
        .await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum H7105AutoNumberKind {
    OnTemperature,
    KeepTemperature,
    OnSpeed,
    KeepSpeed,
    StartAngle,
    EndAngle,
}

impl H7105AutoNumberKind {
    fn field(self) -> &'static str {
        match self {
            Self::OnTemperature => "on-temperature",
            Self::KeepTemperature => "keep-temperature",
            Self::OnSpeed => "on-speed",
            Self::KeepSpeed => "keep-speed",
            Self::StartAngle => "start-angle",
            Self::EndAngle => "end-angle",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::OnTemperature => "Auto On Temperature",
            Self::KeepTemperature => "Auto Keep Temperature",
            Self::OnSpeed => "Auto On Speed",
            Self::KeepSpeed => "Auto Keep Speed",
            Self::StartAngle => "Auto Oscillation Start Angle",
            Self::EndAngle => "Auto Oscillation End Angle",
        }
    }

    fn range(self) -> (f32, f32, f32, Option<&'static str>) {
        match self {
            Self::OnTemperature | Self::KeepTemperature => (10.0, 40.0, 1.0, Some("°C")),
            Self::OnSpeed | Self::KeepSpeed => (1.0, 12.0, 1.0, None),
            Self::StartAngle | Self::EndAngle => (-75.0, 75.0, 0.5, Some("deg")),
        }
    }
}

pub struct H7105AutoNumber {
    config: NumberConfig,
    device_id: String,
    state: StateHandle,
    kind: H7105AutoNumberKind,
}

impl H7105AutoNumber {
    pub fn all(device: &ServiceDevice, state: &StateHandle) -> Vec<Self> {
        [
            H7105AutoNumberKind::OnTemperature,
            H7105AutoNumberKind::KeepTemperature,
            H7105AutoNumberKind::OnSpeed,
            H7105AutoNumberKind::KeepSpeed,
            H7105AutoNumberKind::StartAngle,
            H7105AutoNumberKind::EndAngle,
        ]
        .into_iter()
        .map(|kind| Self::new(device, state, kind))
        .collect()
    }

    fn new(device: &ServiceDevice, state: &StateHandle, kind: H7105AutoNumberKind) -> Self {
        let (min, max, step, unit) = kind.range();
        let field = kind.field();
        Self {
            config: NumberConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(kind.name().to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-auto-{field}", topic_safe_id(device)),
                    entity_category: Some("config".to_string()),
                    icon: None,
                },
                command_topic: topic(device, &format!("auto/{field}/command")),
                state_topic: Some(topic(device, &format!("auto/{field}/state"))),
                min: Some(min),
                max: Some(max),
                step,
                unit_of_measurement: unit,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            kind,
        }
    }
}

#[async_trait]
impl EntityInstance for H7105AutoNumber {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        let auto = device.h7105_auto_config()?;
        let value = match self.kind {
            H7105AutoNumberKind::OnTemperature => auto.on_temperature_c.to_string(),
            H7105AutoNumberKind::KeepTemperature => auto.keep_temperature_c.to_string(),
            H7105AutoNumberKind::OnSpeed => auto.on_speed.to_string(),
            H7105AutoNumberKind::KeepSpeed => auto.keep_speed.to_string(),
            H7105AutoNumberKind::StartAngle => format_h7105_angle(auto.oscillation.start_degrees()),
            H7105AutoNumberKind::EndAngle => format_h7105_angle(auto.oscillation.end_degrees()),
        };
        self.config.notify_state(client, &value).await
    }
}

#[derive(Clone, Copy)]
enum H7105AutoSwitchKind {
    Oscillation,
    Symmetric,
}

pub struct H7105AutoSwitch {
    config: SwitchConfig,
    device_id: String,
    state: StateHandle,
    kind: H7105AutoSwitchKind,
}

impl H7105AutoSwitch {
    pub fn all(device: &ServiceDevice, state: &StateHandle) -> [Self; 2] {
        [
            Self::new(device, state, H7105AutoSwitchKind::Oscillation),
            Self::new(device, state, H7105AutoSwitchKind::Symmetric),
        ]
    }

    fn new(device: &ServiceDevice, state: &StateHandle, kind: H7105AutoSwitchKind) -> Self {
        let (field, name, icon) = match kind {
            H7105AutoSwitchKind::Oscillation => {
                ("oscillation", "Auto Oscillation", "mdi:rotate-3d-variant")
            }
            H7105AutoSwitchKind::Symmetric => (
                "symmetric",
                "Auto Symmetric Oscillation",
                "mdi:arrow-left-right",
            ),
        };
        Self {
            config: SwitchConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(name.to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-auto-{field}", topic_safe_id(device)),
                    entity_category: Some("config".to_string()),
                    icon: Some(icon.to_string()),
                },
                command_topic: topic(device, &format!("auto/{field}/command")),
                state_topic: topic(device, &format!("auto/{field}/state")),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            kind,
        }
    }
}

#[async_trait]
impl EntityInstance for H7105AutoSwitch {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        let auto = device.h7105_auto_config()?;
        let on = match self.kind {
            H7105AutoSwitchKind::Oscillation => auto.oscillating,
            H7105AutoSwitchKind::Symmetric => auto.oscillation.flags == 0,
        };
        client
            .publish(&self.config.state_topic, if on { "ON" } else { "OFF" })
            .await
    }
}

pub struct H7105AutoOscillationSpeed {
    config: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl H7105AutoOscillationSpeed {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        Self {
            config: SelectConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Auto Oscillation Speed".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{}-auto-oscillation-speed", topic_safe_id(device)),
                    entity_category: Some("config".to_string()),
                    icon: Some("mdi:speedometer".to_string()),
                },
                command_topic: topic(device, "auto/oscillation-speed/command"),
                state_topic: topic(device, "auto/oscillation-speed/state"),
                options: vec!["Low".to_string(), "High".to_string()],
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for H7105AutoOscillationSpeed {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        let auto = device.h7105_auto_config()?;
        client
            .publish(
                &self.config.state_topic,
                oscillation_speed_name(auto.oscillation.speed)?,
            )
            .await
    }
}

#[derive(Deserialize)]
pub struct H7105AutoControlParams {
    id: String,
    field: String,
}

pub async fn mqtt_h7105_auto_control(
    Payload(payload): Payload<String>,
    Params(H7105AutoControlParams { id, field }): Params<H7105AutoControlParams>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let mut auto = device.h7105_auto_config()?;
    let mut packets = device.h7105_fan_state.mode_config_packets[(H7105FanMode::Auto as usize) - 1];
    let packet = packets[0]
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("H7105 Auto configuration unavailable"))?;

    match field.as_str() {
        "on-temperature" => auto.on_temperature_c = parse_h7105_u8(&payload)?,
        "keep-temperature" => auto.keep_temperature_c = parse_h7105_u8(&payload)?,
        "on-speed" => auto.on_speed = parse_h7105_u8(&payload)?,
        "keep-speed" => auto.keep_speed = parse_h7105_u8(&payload)?,
        "oscillation" => {
            auto.oscillating = parse_on_off(&payload, "Auto oscillation")?;
        }
        "oscillation-speed" => auto.oscillation.speed = parse_oscillation_speed(&payload)?,
        "symmetric" => {
            let symmetric = parse_on_off(&payload, "Auto symmetry")?;
            auto.oscillation.flags = if symmetric { 0 } else { 1 };
            if symmetric {
                center_h7105_oscillation(&mut auto.oscillation);
            }
        }
        "start-angle" => {
            let value = h7105_angle_to_tenths(payload.parse()?)?;
            auto.oscillation.start_tenths = value;
            if auto.oscillation.flags == 0 {
                auto.oscillation.end_tenths = -value;
            }
        }
        "end-angle" => {
            let value = h7105_angle_to_tenths(payload.parse()?)?;
            auto.oscillation.end_tenths = value;
            if auto.oscillation.flags == 0 {
                auto.oscillation.start_tenths = -value;
            }
        }
        _ => anyhow::bail!("invalid H7105 Auto field {field:?}"),
    }
    auto.validate()?;
    let oscillation_params = auto.oscillation.to_params()?;
    packet[3] = auto.on_speed;
    packet[6] = auto.keep_speed;
    packet[9] = u8::from(auto.oscillating);
    packet[10..15].copy_from_slice(&oscillation_params);
    if field == "on-temperature" {
        packet[4..6].copy_from_slice(
            &h7105_celsius_to_fahrenheit_hundredths(auto.on_temperature_c)?.to_be_bytes(),
        );
    }
    if field == "keep-temperature" {
        packet[7..9].copy_from_slice(
            &h7105_celsius_to_fahrenheit_hundredths(auto.keep_temperature_c)?.to_be_bytes(),
        );
    }

    state.h7105_send_mode_config(&device, packets).await?;
    state.poll_iot_api(&device).await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum H7105CustomSensorKind {
    ActiveStage,
    Remaining(u8),
}

pub struct H7105CustomSensor {
    config: SensorConfig,
    device_id: String,
    state: StateHandle,
    kind: H7105CustomSensorKind,
}

impl H7105CustomSensor {
    pub fn all(device: &ServiceDevice, state: &StateHandle) -> [Self; 3] {
        [
            Self::new(device, state, H7105CustomSensorKind::ActiveStage),
            Self::new(device, state, H7105CustomSensorKind::Remaining(1)),
            Self::new(device, state, H7105CustomSensorKind::Remaining(2)),
        ]
    }

    fn new(device: &ServiceDevice, state: &StateHandle, kind: H7105CustomSensorKind) -> Self {
        let (suffix, name, unit, icon) = match kind {
            H7105CustomSensorKind::ActiveStage => (
                "custom-active-stage".to_string(),
                "Custom Active Stage".to_string(),
                None,
                "mdi:format-list-numbered",
            ),
            H7105CustomSensorKind::Remaining(stage) => (
                format!("custom-stage-{stage}-remaining"),
                format!("Custom Stage {stage} Remaining"),
                Some("min"),
                "mdi:timer-outline",
            ),
        };
        let unique_id = format!("gv2mqtt-{}-{suffix}", topic_safe_id(device));
        Self {
            config: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(name),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    entity_category: Some("diagnostic".to_string()),
                    icon: Some(icon.to_string()),
                },
                state_topic: topic(device, &format!("{suffix}/state")),
                state_class: None,
                unit_of_measurement: unit,
                json_attributes_topic: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            kind,
        }
    }
}

#[async_trait]
impl EntityInstance for H7105CustomSensor {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.config.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let device = self
            .state
            .device_by_id(&self.device_id)
            .await
            .expect("device to exist");
        let stages = device.h7105_custom_stages()?;
        let value = match self.kind {
            H7105CustomSensorKind::ActiveStage => stages
                .iter()
                .find(|stage| stage.active)
                .map(|stage| stage.stage.to_string())
                .unwrap_or_else(|| "0".to_string()),
            H7105CustomSensorKind::Remaining(stage) => stages[(stage - 1) as usize]
                .remaining_minutes
                .map(|minutes| minutes.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        };
        self.config.notify_state(client, &value).await
    }
}

fn parse_on_off(payload: &str, name: &str) -> anyhow::Result<bool> {
    match payload {
        "ON" => Ok(true),
        "OFF" => Ok(false),
        _ => anyhow::bail!("invalid H7105 {name} payload {payload:?}"),
    }
}

fn parse_h7105_u8(payload: &str) -> anyhow::Result<u8> {
    Ok(parse_h7105_whole_number(payload)?.try_into()?)
}

fn parse_h7105_whole_number(payload: &str) -> anyhow::Result<u16> {
    let value: f32 = payload.parse()?;
    anyhow::ensure!(
        value.is_finite()
            && value >= 0.0
            && value <= u16::MAX as f32
            && value.fract().abs() < f32::EPSILON,
        "H7105 value must be a whole number"
    );
    Ok(value as u16)
}

fn parse_oscillation_speed(payload: &str) -> anyhow::Result<u8> {
    match payload {
        "Low" => Ok(1),
        "High" => Ok(3),
        _ => anyhow::bail!("invalid H7105 oscillation speed {payload:?}"),
    }
}

fn center_h7105_oscillation(config: &mut H7105OscillationConfig) {
    let half_span = (config.end_tenths - config.start_tenths) / 2;
    config.start_tenths = -half_span;
    config.end_tenths = half_span;
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn h7105_parses_home_assistant_number_payloads() {
        assert_eq!(parse_h7105_whole_number("25").unwrap(), 25);
        assert_eq!(parse_h7105_whole_number("25.0").unwrap(), 25);
        assert!(parse_h7105_whole_number("25.5").is_err());
    }
}
