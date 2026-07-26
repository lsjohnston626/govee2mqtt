use anyhow::anyhow;
use once_cell::sync::Lazy;
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use serde::{Deserialize, Deserializer};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

static MGR: Lazy<PacketManager> = Lazy::new(PacketManager::new);

#[derive(Clone, PartialEq, Eq)]
pub struct HexBytes(Vec<u8>);

impl std::fmt::Debug for HexBytes {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.write_fmt(format_args!("{:02X?}", self.0))
    }
}

#[allow(clippy::type_complexity)]
pub struct PacketCodec {
    encode: Box<dyn Fn(&dyn Any) -> anyhow::Result<Vec<u8>> + Sync + Send>,
    decode: Box<dyn Fn(&[u8]) -> anyhow::Result<GoveeBlePacket> + Sync + Send>,
    supported_skus: &'static [&'static str],
    type_id: TypeId,
}

impl PacketCodec {
    pub fn new<T: 'static>(
        supported_skus: &'static [&'static str],
        encode: impl Fn(&T) -> anyhow::Result<Vec<u8>> + 'static + Sync + Send,
        decode: impl Fn(&[u8]) -> anyhow::Result<GoveeBlePacket> + 'static + Sync + Send,
    ) -> Self {
        Self {
            encode: Box::new(move |any| {
                let type_id = TypeId::of::<T>();
                let value = any.downcast_ref::<T>().ok_or_else(|| {
                    anyhow!("cannot downcast to {type_id:?} in PacketCodec encoder")
                })?;
                (encode)(value)
            }),
            decode: Box::new(decode),
            supported_skus,
            type_id: TypeId::of::<T>(),
        }
    }
}

pub struct PacketManager {
    codec_by_sku: Mutex<HashMap<String, HashMap<TypeId, Arc<PacketCodec>>>>,
    all_codecs: Vec<Arc<PacketCodec>>,
}

impl PacketManager {
    fn map_for_sku(&self, sku: &str) -> MappedMutexGuard<'_, HashMap<TypeId, Arc<PacketCodec>>> {
        MutexGuard::map(self.codec_by_sku.lock(), |codecs| {
            codecs.entry(sku.to_string()).or_insert_with(|| {
                let mut map = HashMap::new();

                for codec in &self.all_codecs {
                    if codec.supported_skus.contains(&sku)
                        && map.insert(codec.type_id, codec.clone()).is_some()
                    {
                        eprintln!("Conflicting PacketCodecs for {sku} {:?}", codec.type_id);
                    }
                }

                map
            })
        })
    }

    fn resolve_by_sku(&self, sku: &str, type_id: &TypeId) -> anyhow::Result<Arc<PacketCodec>> {
        let map = self.map_for_sku(sku);

        map.get(type_id)
            .cloned()
            .ok_or_else(|| anyhow!("sku {sku} has no codec for type {type_id:?}"))
    }

    pub fn decode_for_sku(&self, sku: &str, data: &[u8]) -> GoveeBlePacket {
        let map = self.map_for_sku(sku);

        for codec in map.values() {
            if let Ok(value) = (codec.decode)(data) {
                return value;
            }
        }

        GoveeBlePacket::Generic(HexBytes(data.to_vec()))
    }

    pub fn encode_for_sku<T: 'static>(&self, sku: &str, value: &T) -> anyhow::Result<Vec<u8>> {
        let type_id = TypeId::of::<T>();
        let codec = self.resolve_by_sku(sku, &type_id)?;

        (codec.encode)(value)
    }

    pub fn new() -> Self {
        let mut all_codecs = vec![];

        macro_rules! encode_body {
            // Tail case: nothing to do
            ($target:expr,$input:expr,) => {};

            // Match a constant byte; emit it
            ($target:expr,$input:expr, $expected:literal, $($tail:tt)*) => {
                    $target.push($expected);
                    encode_body!($target, $input, $($tail)*);
            };

            // Match a field; emit it from the struct
            ($target:expr, $input:expr, $field_name:ident, $($tail:tt)*) => {
                    $input.$field_name.encode_param($target);
                    encode_body!($target, $input, $($tail)*);
            };
        }

        macro_rules! decode_body {
            // Tail case; verify that remaining bytes are zero
            ($target:expr, $data:expr,) => {
                while !$data.is_empty() {
                    anyhow::ensure!($data[0] == 0);
                    $data = &$data[1..];
                }
            };

            // Match a constant byte; check that it is what we expect
            ($target:expr, $data:expr, $expected:literal, $($tail:tt)*) => {
                    let maybe_byte = $data.get(0);
                    anyhow::ensure!(maybe_byte == Some(&$expected),"expected {} but got {maybe_byte:?}", $expected);
                    $data = &$data[1..];
                    decode_body!($target, $data, $($tail)*);
            };

            // Match a field; parse it into the struct
            ($target:expr, $data:expr, $field_name:ident, $($tail:tt)*) => {
                    let remain = $target.$field_name.decode_param($data)?;
                    $data = remain;
                    decode_body!($target, $data, $($tail)*);
            };
        }

        /// Helper for defining a PacketCodec.
        /// The first param is the list of SKUs which are known to support
        /// this packet.
        /// The second parameter is the name of the type which will be
        /// encoded into raw bytes when encoding. It must impl Default.
        /// The third parameter is the name of the GoveeBlePacket enum
        /// variant that holds that type.
        /// The subsequent parameters are rules that match the bytes
        /// in the packet when decoding, or form the bytes in the packet
        /// when encoding. They are listed in the same sequence that they
        /// have in the packet.
        macro_rules! packet {
            ($skus:expr, $struct:ident, $variant:ident, $($body:tt)*) => {
                PacketCodec::new(
                    $skus,
                    |input_value: &$struct| {
                        let mut bytes = vec![];
                        encode_body!(&mut bytes, input_value, $($body)*);
                        Ok(finish(bytes))
                    },
                    |data| {
                        let mut data = &data[0..data.len().saturating_sub(1)];
                        let mut value = $struct::default();
                        decode_body!(&mut value, data, $($body)*);
                        Ok(GoveeBlePacket::$variant(value))
                    }
                )
            }
        }

        all_codecs.push(packet!(
            &["H7160"],
            SetHumidifierMode,
            SetHumidifierMode,
            0x33,
            0x05,
            mode,
            param,
        ));
        all_codecs.push(packet!(
            &["H7160"],
            NotifyHumidifierMode,
            NotifyHumidifierMode,
            0xaa,
            0x05,
            0x00,
            mode,
            param,
        ));
        all_codecs.push(packet!(
            &["H7160"],
            HumidifierAutoMode,
            NotifyHumidifierAutoMode,
            0xaa,
            0x05,
            0x03,
            target_humidity,
        ));
        all_codecs.push(packet!(
            &["H7160"],
            NotifyHumidifierNightlightParams,
            NotifyHumidifierNightlight,
            0xaa,
            0x1b,
            on,
            brightness,
            r,
            g,
            b,
        ));
        all_codecs.push(packet!(
            &["H7160"],
            SetHumidifierNightlightParams,
            SetHumidifierNightlight,
            0x33,
            0x1b,
            on,
            brightness,
            r,
            g,
            b,
        ));
        all_codecs.push(PacketCodec::new(
            &["H5086"],
            |_reading: &H5086PowerReading| {
                anyhow::bail!("H5086 power readings are device notifications")
            },
            H5086PowerReading::decode,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            SetH7105FanSpeed,
            SetH7105FanSpeed,
            0x33,
            0x05,
            0x01,
            speed,
        ));
        all_codecs.push(PacketCodec::new(
            &["H7105"],
            |_speed: &NotifyH7105FanSpeed| {
                anyhow::bail!("H7105 fan speeds are device notifications")
            },
            NotifyH7105FanSpeed::decode,
        ));
        all_codecs.push(PacketCodec::new(
            &["H7105"],
            SetH7105FanMode::encode,
            SetH7105FanMode::decode,
        ));
        all_codecs.push(PacketCodec::new(
            &["H7105"],
            |_mode: &NotifyH7105FanMode| anyhow::bail!("H7105 fan modes are device notifications"),
            NotifyH7105FanMode::decode,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            SetH7105NightlightPower,
            SetH7105NightlightPower,
            0x3a,
            0x1b,
            0x01,
            0x01,
            on,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            SetH7105NightlightBrightness,
            SetH7105NightlightBrightness,
            0x3a,
            0x1b,
            0x01,
            0x02,
            brightness,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            SetH7105NightlightColor,
            SetH7105NightlightColor,
            0x3a,
            0x1b,
            0x05,
            0x0d,
            r,
            g,
            b,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            NotifyH7105NightlightState,
            NotifyH7105NightlightState,
            0xaa,
            0x1b,
            0x01,
            on,
            brightness,
        ));
        all_codecs.push(packet!(
            &["H7105"],
            NotifyH7105NightlightColor,
            NotifyH7105NightlightColor,
            0xaa,
            0x1b,
            0x05,
            0x0d,
            r,
            g,
            b,
        ));
        all_codecs.push(PacketCodec::new(
            &["H7105"],
            SetH7105Oscillation::encode,
            SetH7105Oscillation::decode,
        ));
        all_codecs.push(PacketCodec::new(
            &["H7105"],
            |_state: &NotifyH7105Oscillation| {
                anyhow::bail!("H7105 oscillation states are device notifications")
            },
            NotifyH7105Oscillation::decode,
        ));
        all_codecs.push(PacketCodec::new(
            &["Generic:Light"],
            SetSceneCode::encode,
            SetSceneCode::decode,
        ));

        all_codecs.push(packet!(
            &["Generic:Light", "H7105"],
            SetDevicePower,
            SetDevicePower,
            0x33,
            0x01,
            on,
        ));

        Self {
            codec_by_sku: Mutex::new(HashMap::new()),
            all_codecs: all_codecs.into_iter().map(Arc::new).collect(),
        }
    }
}

pub trait DecodePacketParam {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]>;
    fn encode_param(&self, target: &mut Vec<u8>);
}

impl DecodePacketParam for u8 {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        *self = *data.first().ok_or_else(|| anyhow!("EOF"))?;
        Ok(&data[1..])
    }

    fn encode_param(&self, target: &mut Vec<u8>) {
        target.push(*self);
    }
}

impl DecodePacketParam for u16 {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        let lo = *data.first().ok_or_else(|| anyhow!("EOF"))?;
        let hi = *data.get(1).ok_or_else(|| anyhow!("EOF"))?;
        *self = ((hi as u16) << 8) | lo as u16;
        Ok(&data[2..])
    }

    fn encode_param(&self, target: &mut Vec<u8>) {
        let hi = (*self >> 8) as u8;
        let lo = (*self & 0xff) as u8;
        target.push(lo);
        target.push(hi);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SetHumidifierNightlightParams {
    pub on: bool,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub brightness: u8,
}

impl From<NotifyHumidifierNightlightParams> for SetHumidifierNightlightParams {
    fn from(val: NotifyHumidifierNightlightParams) -> Self {
        SetHumidifierNightlightParams {
            on: val.on,
            r: val.r,
            g: val.g,
            b: val.b,
            brightness: val.brightness,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NotifyHumidifierNightlightParams {
    pub on: bool,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub brightness: u8,
}

/// Data is offset by 128 with increments of 1%,
/// so 0% is 128, 100% is 228%
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetHumidity(u8);

impl From<TargetHumidity> for u8 {
    fn from(val: TargetHumidity) -> Self {
        val.0
    }
}

impl DecodePacketParam for TargetHumidity {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        self.0.decode_param(data)
    }

    fn encode_param(&self, target: &mut Vec<u8>) {
        target.push(self.0);
    }
}

impl TargetHumidity {
    pub fn as_percent(&self) -> u8 {
        self.0 & 0x7f
    }

    pub fn into_inner(self) -> u8 {
        self.0
    }

    pub fn from_percent(percent: u8) -> Self {
        Self(percent + 128)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct SetHumidifierMode {
    pub mode: u8,
    pub param: u8,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct NotifyHumidifierMode {
    pub mode: u8,
    pub param: u8,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct HumidifierAutoMode {
    pub target_humidity: TargetHumidity,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct SetSceneCode {
    code: u16,
    scence_param: String,
}

impl SetSceneCode {
    pub fn new(code: u16, scence_param: String) -> Self {
        Self { code, scence_param }
    }

    /// For reference, see:
    /// <https://github.com/egold555/Govee-Reverse-Engineering/issues/11#issuecomment-2565692233>
    /// <https://github.com/AlgoClaw/Govee/blob/main/decoded/explanation>
    fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let bytes = data_encoding::BASE64.decode(self.scence_param.as_bytes())?;

        let mut data = vec![0xa3, 0x00, 0x01, 0x00 /* line count */, 0x02];
        let mut num_lines = 0u8;
        let mut last_line_marker = 1;

        for b in bytes {
            if data.len().is_multiple_of(19) {
                num_lines += 1;

                data.push(0xa3);
                last_line_marker = data.len();

                data.push(num_lines);
            }

            data.push(b);
        }
        // The last line uses 0xff as the indicator, rather than its line number
        data[last_line_marker] = 0xff;
        // back-patch the number of lines into the packet
        data[3] = num_lines + 1;

        // Now apply padding and checksums
        let mut padded = vec![];
        for chunk in data.chunks(19) {
            let mut padded_chunk = chunk.to_vec();
            padded_chunk = finish(padded_chunk);
            padded.append(&mut padded_chunk);
        }

        // and finally encode the scene code as the final packet "line"
        let hi = (self.code >> 8) as u8;
        let lo = (self.code & 0xff) as u8;
        padded.append(&mut finish(vec![0x33, 0x05, 0x04, lo, hi]));
        Ok(padded)
    }

    fn decode(_data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::bail!("SetSceneCode::decode is not implemented");
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct SetDevicePower {
    pub on: bool,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct H5086PowerReading {
    pub powered_on_seconds: u32,
    pub energy_deci_watt_hours: u32,
    pub voltage_centivolts: u16,
    pub current_centi_amps: u16,
    pub power_centi_watts: u32,
    pub power_factor_percent: u8,
}

impl H5086PowerReading {
    fn u24_be(data: &[u8]) -> u32 {
        u32::from_be_bytes([0, data[0], data[1], data[2]])
    }

    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H5086 packet");
        anyhow::ensure!(
            matches!(data[0], 0xaa | 0xee) && data[1] == 0x19,
            "not an H5086 power reading"
        );
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid H5086 packet checksum"
        );

        Ok(GoveeBlePacket::H5086PowerReading(Self {
            powered_on_seconds: Self::u24_be(&data[2..5]),
            energy_deci_watt_hours: Self::u24_be(&data[5..8]),
            voltage_centivolts: u16::from_be_bytes([data[8], data[9]]),
            current_centi_amps: u16::from_be_bytes([data[10], data[11]]),
            power_centi_watts: Self::u24_be(&data[12..15]),
            power_factor_percent: data[15],
        }))
    }

    pub fn energy_watt_hours(self) -> f64 {
        self.energy_deci_watt_hours as f64 / 10.0
    }

    pub fn voltage(self) -> f64 {
        self.voltage_centivolts as f64 / 100.0
    }

    pub fn current(self) -> f64 {
        self.current_centi_amps as f64 / 100.0
    }

    pub fn power(self) -> f64 {
        self.power_centi_watts as f64 / 100.0
    }

    pub fn power_factor_percent(self) -> u8 {
        self.power_factor_percent.min(100)
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105FanSpeed {
    pub speed: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NotifyH7105FanSpeed {
    pub speed: u8,
}

impl NotifyH7105FanSpeed {
    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H7105 packet");
        anyhow::ensure!(
            data[0..3] == [0xaa, 0x05, 0x01],
            "not an H7105 fan speed notification"
        );
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid checksum"
        );
        anyhow::ensure!((1..=12).contains(&data[3]), "invalid H7105 fan speed");
        Ok(GoveeBlePacket::NotifyH7105FanSpeed(Self { speed: data[3] }))
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum H7105FanMode {
    #[default]
    Normal = 1,
    Auto = 2,
    Nature = 3,
    Custom = 4,
    Sleep = 5,
}

impl H7105FanMode {
    pub const ALL: [Self; 5] = [
        Self::Normal,
        Self::Auto,
        Self::Nature,
        Self::Custom,
        Self::Sleep,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Custom => "Custom",
            Self::Auto => "Auto",
            Self::Sleep => "Sleep",
            Self::Nature => "Nature",
        }
    }

    pub fn from_name(name: &str) -> anyhow::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow!("unsupported H7105 mode {name:?}"))
    }
}

impl TryFrom<u8> for H7105FanMode {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> anyhow::Result<Self> {
        match value {
            1 => Ok(Self::Normal),
            2 => Ok(Self::Auto),
            3 => Ok(Self::Nature),
            4 => Ok(Self::Custom),
            5 => Ok(Self::Sleep),
            _ => anyhow::bail!("unknown H7105 fan mode {value}"),
        }
    }
}

impl DecodePacketParam for H7105FanMode {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        let mut value = 0u8;
        let remain = value.decode_param(data)?;
        *self = value.try_into()?;
        Ok(remain)
    }

    fn encode_param(&self, target: &mut Vec<u8>) {
        target.push(*self as u8);
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105FanMode {
    pub mode: H7105FanMode,
}

impl SetH7105FanMode {
    fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(finish(vec![0x33, 0x05, 0x00, self.mode as u8]))
    }

    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H7105 packet");
        anyhow::ensure!(
            data[0..3] == [0x33, 0x05, 0x00],
            "not an H7105 fan mode command"
        );
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid checksum"
        );
        Ok(GoveeBlePacket::SetH7105FanMode(Self {
            mode: data[3].try_into()?,
        }))
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NotifyH7105FanMode {
    pub mode: H7105FanMode,
}

impl NotifyH7105FanMode {
    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H7105 packet");
        anyhow::ensure!(
            data[0..3] == [0xaa, 0x05, 0x00],
            "not an H7105 fan mode notification"
        );
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid checksum"
        );
        Ok(GoveeBlePacket::NotifyH7105FanMode(Self {
            mode: data[3].try_into()?,
        }))
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105NightlightPower {
    pub on: bool,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105NightlightBrightness {
    pub brightness: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105NightlightColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NotifyH7105NightlightState {
    pub on: bool,
    pub brightness: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NotifyH7105NightlightColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SetH7105Oscillation {
    pub oscillating: bool,
    pub params: [u8; 5],
}

impl SetH7105Oscillation {
    fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = vec![0x3a, 0x1d, btoi(self.oscillating)];
        bytes.extend(self.params);
        Ok(finish(bytes))
    }

    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H7105 packet");
        anyhow::ensure!(
            data[0..2] == [0x3a, 0x1d],
            "not an H7105 oscillation command"
        );
        anyhow::ensure!(matches!(data[2], 0x00 | 0x01), "invalid oscillation value");
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid checksum"
        );
        Ok(GoveeBlePacket::SetH7105Oscillation(Self {
            oscillating: data[2] == 0x01,
            params: data[3..8].try_into()?,
        }))
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NotifyH7105Oscillation {
    pub oscillating: bool,
    pub params: [u8; 5],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct H7105OscillationConfig {
    pub speed: u8,
    pub start_degrees: i8,
    pub end_degrees: i8,
    pub flags: u8,
}

impl H7105OscillationConfig {
    pub fn from_params(params: [u8; 5]) -> anyhow::Result<Self> {
        let speed = params[0];
        let span = params[1] as i16;
        let position = u16::from_be_bytes([params[2], params[3]]) as i16;
        let center_tenths = 900 - position;
        let start_tenths = center_tenths - span * 5;
        let end_tenths = center_tenths + span * 5;
        anyhow::ensure!(matches!(speed, 1 | 3), "invalid oscillation speed");
        anyhow::ensure!(matches!(params[4], 0 | 1), "invalid symmetry flag");
        anyhow::ensure!(
            start_tenths % 10 == 0 && end_tenths % 10 == 0,
            "oscillation positions are not in whole degrees"
        );
        let start_degrees: i8 = (start_tenths / 10).try_into()?;
        let end_degrees: i8 = (end_tenths / 10).try_into()?;
        anyhow::ensure!(
            (-75..=75).contains(&start_degrees) && (-75..=75).contains(&end_degrees),
            "oscillation positions must be between -75 and 75 degrees"
        );
        Ok(Self {
            speed,
            start_degrees,
            end_degrees,
            flags: params[4],
        })
    }

    pub fn to_params(self) -> anyhow::Result<[u8; 5]> {
        anyhow::ensure!(matches!(self.speed, 1 | 3), "invalid oscillation speed");
        anyhow::ensure!(
            (-75..=75).contains(&self.start_degrees)
                && (-75..=75).contains(&self.end_degrees),
            "oscillation positions must be between -75 and 75 degrees"
        );
        anyhow::ensure!(
            self.start_degrees < self.end_degrees,
            "oscillation start must be lower than end"
        );
        let span: u16 = (self.end_degrees as i16 - self.start_degrees as i16).try_into()?;
        let position =
            900i16 - (self.start_degrees as i16 + self.end_degrees as i16) * 5;
        let position: u16 = position.try_into()?;
        let [position_hi, position_lo] = position.to_be_bytes();
        Ok([
            self.speed,
            span.try_into()?,
            position_hi,
            position_lo,
            self.flags,
        ])
    }
}

impl NotifyH7105Oscillation {
    fn decode(data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::ensure!(data.len() == 20, "expected a 20-byte H7105 packet");
        anyhow::ensure!(
            data[0] == 0xaa && data[1] == 0x1d && matches!(data[2], 0x00 | 0x01),
            "not an H7105 oscillation notification"
        );
        anyhow::ensure!(
            calculate_checksum(&data[..19]) == data[19],
            "invalid checksum"
        );
        Ok(GoveeBlePacket::NotifyH7105Oscillation(Self {
            oscillating: data[2] == 0x01,
            params: data[3..8].try_into()?,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoveeBlePacket {
    Generic(HexBytes),
    H5086PowerReading(H5086PowerReading),
    SetH7105FanSpeed(SetH7105FanSpeed),
    NotifyH7105FanSpeed(NotifyH7105FanSpeed),
    SetH7105FanMode(SetH7105FanMode),
    NotifyH7105FanMode(NotifyH7105FanMode),
    SetH7105NightlightPower(SetH7105NightlightPower),
    SetH7105NightlightBrightness(SetH7105NightlightBrightness),
    SetH7105NightlightColor(SetH7105NightlightColor),
    NotifyH7105NightlightState(NotifyH7105NightlightState),
    NotifyH7105NightlightColor(NotifyH7105NightlightColor),
    SetH7105Oscillation(SetH7105Oscillation),
    NotifyH7105Oscillation(NotifyH7105Oscillation),
    #[allow(unused)] // can remove if/when SetSceneCode::decode has an impl
    SetSceneCode(SetSceneCode),
    SetDevicePower(SetDevicePower),
    SetHumidifierNightlight(SetHumidifierNightlightParams),
    NotifyHumidifierMode(NotifyHumidifierMode),
    SetHumidifierMode(SetHumidifierMode),
    NotifyHumidifierAutoMode(HumidifierAutoMode),
    NotifyHumidifierNightlight(NotifyHumidifierNightlightParams),
}

#[derive(Debug)]
pub struct Base64HexBytes(HexBytes);

impl Base64HexBytes {
    pub fn decode_for_sku(&self, sku: &str) -> GoveeBlePacket {
        MGR.decode_for_sku(sku, &self.0 .0)
    }

    pub fn encode_for_sku<T: 'static>(sku: &str, value: &T) -> anyhow::Result<Self> {
        MGR.encode_for_sku(sku, value)
            .map(|bytes| Base64HexBytes(HexBytes(bytes)))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0 .0
    }

    pub fn base64(&self) -> Vec<String> {
        let mut result = vec![];
        for chunk in self.0 .0.chunks(20) {
            result.push(data_encoding::BASE64.encode(chunk));
        }
        result
    }

    pub fn with_bytes(bytes: Vec<u8>) -> Self {
        Self(HexBytes(finish(bytes)))
    }
}

impl<'de> Deserialize<'de> for Base64HexBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;
        let encoded = String::deserialize(deserializer)?;
        let decoded = data_encoding::BASE64
            .decode(encoded.as_ref())
            .map_err(|e| D::Error::custom(format!("{e:#}")))?;
        Ok(Self(HexBytes(decoded)))
    }
}

fn calculate_checksum(data: &[u8]) -> u8 {
    let mut checksum: u8 = 0;
    for &b in data {
        checksum ^= b;
    }
    checksum
}

fn finish(mut data: Vec<u8>) -> Vec<u8> {
    let checksum = calculate_checksum(&data);
    data.resize(19, 0);
    data.push(checksum);
    data
}

impl DecodePacketParam for bool {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        let mut byte = 0u8;
        let remain = byte.decode_param(data)?;
        *self = itob(&byte);
        Ok(remain)
    }

    fn encode_param(&self, target: &mut Vec<u8>) {
        target.push(btoi(*self));
    }
}

fn btoi(on: bool) -> u8 {
    if on {
        1
    } else {
        0
    }
}

fn itob(i: &u8) -> bool {
    *i != 0
}

impl GoveeBlePacket {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn packet_manager() {
        assert_eq!(
            MGR.decode_for_sku(
                "H7160",
                &[0x33, 0x05, 0x01, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 23]
            ),
            GoveeBlePacket::SetHumidifierMode(SetHumidifierMode {
                mode: 1,
                param: 0x20
            })
        );

        assert_eq!(
            MGR.encode_for_sku(
                "H7160",
                &SetHumidifierMode {
                    mode: 1,
                    param: 0x20
                }
            )
            .unwrap(),
            vec![0x33, 0x05, 0x01, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 23]
        );
    }

    fn round_trip<T: 'static + std::fmt::Debug>(sku: &str, value: &T, expect: GoveeBlePacket) {
        let bytes = Base64HexBytes::encode_for_sku(sku, value).unwrap();
        let decoded = bytes.decode_for_sku(sku);
        assert_eq!(decoded, expect);
    }

    #[test]
    fn basic_round_trip() {
        round_trip(
            "Generic:Light",
            &SetDevicePower { on: true },
            GoveeBlePacket::SetDevicePower(SetDevicePower { on: true }),
        );
        round_trip(
            "H7160",
            &SetHumidifierNightlightParams {
                on: true,
                r: 255,
                g: 69,
                b: 42,
                brightness: 100,
            },
            GoveeBlePacket::SetHumidifierNightlight(SetHumidifierNightlightParams {
                on: true,
                r: 255,
                g: 69,
                b: 42,
                brightness: 100,
            }),
        );
    }

    #[test]
    fn decode_h5086_power_reading() {
        let packet = vec![
            0xee, 0x19, 0x00, 0x19, 0x8b, 0x00, 0x00, 0x26, 0x2f, 0x3d, 0x00, 0x91, 0x00, 0x44,
            0x65, 0x64, 0x00, 0x00, 0x00, 0x85,
        ];

        assert_eq!(
            MGR.decode_for_sku("H5086", &packet),
            GoveeBlePacket::H5086PowerReading(H5086PowerReading {
                powered_on_seconds: 6539,
                energy_deci_watt_hours: 38,
                voltage_centivolts: 12093,
                current_centi_amps: 145,
                power_centi_watts: 17509,
                power_factor_percent: 100,
            })
        );
        assert_eq!(
            H5086PowerReading {
                power_factor_percent: 101,
                ..Default::default()
            }
            .power_factor_percent(),
            100
        );
    }

    #[test]
    fn h7105_commands_and_notifications() {
        round_trip(
            "H7105",
            &SetDevicePower { on: true },
            GoveeBlePacket::SetDevicePower(SetDevicePower { on: true }),
        );
        round_trip(
            "H7105",
            &SetH7105FanSpeed { speed: 12 },
            GoveeBlePacket::SetH7105FanSpeed(SetH7105FanSpeed { speed: 12 }),
        );
        for mode in H7105FanMode::ALL {
            round_trip(
                "H7105",
                &SetH7105FanMode { mode },
                GoveeBlePacket::SetH7105FanMode(SetH7105FanMode { mode }),
            );
        }
        assert_eq!(
            MGR.encode_for_sku(
                "H7105",
                &SetH7105FanMode {
                    mode: H7105FanMode::Auto,
                },
            )
            .unwrap(),
            vec![0x33, 0x05, 0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x34,]
        );
        round_trip(
            "H7105",
            &SetH7105NightlightPower { on: true },
            GoveeBlePacket::SetH7105NightlightPower(SetH7105NightlightPower { on: true }),
        );
        round_trip(
            "H7105",
            &SetH7105NightlightBrightness { brightness: 42 },
            GoveeBlePacket::SetH7105NightlightBrightness(SetH7105NightlightBrightness {
                brightness: 42,
            }),
        );
        round_trip(
            "H7105",
            &SetH7105NightlightColor { r: 1, g: 2, b: 3 },
            GoveeBlePacket::SetH7105NightlightColor(SetH7105NightlightColor { r: 1, g: 2, b: 3 }),
        );

        assert_eq!(
            MGR.decode_for_sku(
                "H7105",
                &[0xaa, 0x05, 0x01, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xa9]
            ),
            GoveeBlePacket::NotifyH7105FanSpeed(NotifyH7105FanSpeed { speed: 7 })
        );
        assert_eq!(
            MGR.decode_for_sku(
                "H7105",
                &[
                    0xaa, 0x05, 0x01, 0x0c, 0x00, 0x03, 0x5a, 0x04, 0xb0, 0x01, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e,
                ]
            ),
            GoveeBlePacket::NotifyH7105FanSpeed(NotifyH7105FanSpeed { speed: 12 })
        );
        assert_eq!(
            MGR.decode_for_sku(
                "H7105",
                &[0xaa, 0x05, 0x00, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xac]
            ),
            GoveeBlePacket::NotifyH7105FanMode(NotifyH7105FanMode {
                mode: H7105FanMode::Nature,
            })
        );
        round_trip(
            "H7105",
            &SetH7105Oscillation {
                oscillating: true,
                params: [0x03, 0x5a, 0x04, 0xb0, 0x01],
            },
            GoveeBlePacket::SetH7105Oscillation(SetH7105Oscillation {
                oscillating: true,
                params: [0x03, 0x5a, 0x04, 0xb0, 0x01],
            }),
        );
        assert_eq!(
            MGR.decode_for_sku(
                "H7105",
                &[
                    0xaa, 0x1d, 0x00, 0x03, 0x5a, 0x04, 0xb0, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0x5b,
                ]
            ),
            GoveeBlePacket::NotifyH7105Oscillation(NotifyH7105Oscillation {
                oscillating: false,
                params: [0x03, 0x5a, 0x04, 0xb0, 0x01],
            })
        );
        let old_range = H7105OscillationConfig::from_params([3, 90, 4, 176, 1]).unwrap();
        assert_eq!(old_range.start_degrees, -75);
        assert_eq!(old_range.end_degrees, 15);
        assert_eq!(old_range.to_params().unwrap(), [3, 90, 4, 176, 1]);

        let new_range = H7105OscillationConfig::from_params([3, 75, 3, 57, 1]).unwrap();
        assert_eq!(new_range.start_degrees, -30);
        assert_eq!(new_range.end_degrees, 45);
        assert_eq!(new_range.to_params().unwrap(), [3, 75, 3, 57, 1]);

        let symmetric_low = H7105OscillationConfig::from_params([1, 50, 3, 132, 0]).unwrap();
        assert_eq!(symmetric_low.start_degrees, -25);
        assert_eq!(symmetric_low.end_degrees, 25);
        assert_eq!(symmetric_low.to_params().unwrap(), [1, 50, 3, 132, 0]);

        let right_only = H7105OscillationConfig::from_params([1, 30, 1, 194, 1]).unwrap();
        assert_eq!(right_only.start_degrees, 30);
        assert_eq!(right_only.end_degrees, 60);
        assert_eq!(right_only.to_params().unwrap(), [1, 30, 1, 194, 1]);
    }

    #[test]
    fn scene_command() {
        const FOREST_SCENCE_PARAM: &str = "AyYAAQAKAgH/GQG0CgoCyBQF//8AAP//////AP//lP8AFAGWAAAAACMAAg8FAgH/FAH7AAAB+goEBP8AtP8AR///4/8AAAAAAAAAABoAAAABAgH/BQHIFBQC7hQBAP8AAAAAAAAAAA==";
        const FOREST_SCENE_CODE: u16 = 212;

        let command = SetSceneCode::new(FOREST_SCENE_CODE, FOREST_SCENCE_PARAM.to_string());

        let padded = command.encode().unwrap();

        println!("data is:");
        let mut hex = String::new();
        for (idx, b) in padded.iter().enumerate() {
            if idx % 20 == 0 && !hex.is_empty() {
                hex.push('\n');
            } else if !hex.is_empty() {
                hex.push(' ');
            }
            hex.push_str(&format!("{b:02x}"));
        }
        println!("{hex}");

        k9::snapshot!(
            hex,
            "
a3 00 01 07 02 03 26 00 01 00 0a 02 01 ff 19 01 b4 0a 0a d9
a3 01 02 c8 14 05 ff ff 00 00 ff ff ff ff ff 00 ff ff 94 12
a3 02 ff 00 14 01 96 00 00 00 00 23 00 02 0f 05 02 01 ff 0a
a3 03 14 01 fb 00 00 01 fa 0a 04 04 ff 00 b4 ff 00 47 ff b3
a3 04 ff e3 ff 00 00 00 00 00 00 00 00 1a 00 00 00 01 02 5d
a3 05 01 ff 05 01 c8 14 14 02 ee 14 01 00 ff 00 00 00 00 92
a3 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 5c
33 05 04 d4 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 e6
"
        );
    }
}
