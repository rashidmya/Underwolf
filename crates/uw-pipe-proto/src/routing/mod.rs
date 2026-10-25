//! Input routing v2. New layouts have separate tags; legacy input and state layouts are unchanged.
mod codec;
pub mod json;
use crate::{
    input::{InputAck, InputChallenge, InputReason, InputRecord},
    pipe::ProtoError,
};
use serde::{Deserialize, Serialize};
pub const ROUTING_VERSION: u16 = 2;
pub const MAX_WIDGETS: usize = 32;
pub const MAX_HOTKEYS: usize = 64;
pub const ROUTED_RECORD_BYTES: usize = 48;

macro_rules! codes {
    ($name:ident { $($variant:ident = $code:literal),* $(,)? }) => {
        #[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
        #[serde(rename_all="snake_case")]
        #[repr(u16)]
        pub enum $name { $($variant=$code),* }
        impl $name {
            pub fn from_code(code:u16)->Result<Self,ProtoError> {
                match code { $($code=>Ok(Self::$variant),)* _=>Err(ProtoError::BadInput) }
            }
        }
    }
}
codes!(InputMode { Block=0,Pass=1,Notify=2 });
codes!(HotkeyAction { ToggleEdit=1,Notify=2,Takeover=3,ReleaseTakeover=4 });
codes!(EventKind { Input=1,Hotkey=2,Cancel=3,Focus=4,Leave=5 });
codes!(RoutingAction { EnterEdit=1,LeaveEdit=2,Takeover=3,ReleaseTakeover=4 });
codes!(RoutingCode { Applied=0,StaleEpoch=1,StaleRevision=2,Invalid=3,Unavailable=4,SetupFailed=5,Superseded=6,NotOwner=7 });
codes!(NativeStatus { Absent=0,Installing=1,Supported=2,Unsupported=3,Failed=4 });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetRef {
    pub id: u32,
    pub generation: u32,
}
impl WidgetRef {
    pub fn validate(self) -> Result<(), ProtoError> {
        check(self.id != 0 && self.generation != 0)
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    pub keyboard: bool,
    pub mouse: bool,
}
impl Domains {
    pub fn bits(self) -> u8 {
        self.keyboard as u8 | (self.mouse as u8) << 1
    }
    pub fn from_bits(bits: u8) -> Result<Self, ProtoError> {
        check(bits & !3 == 0)?;
        Ok(Self { keyboard: bits & 1 != 0, mouse: bits & 2 != 0 })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetSpec {
    pub instance: WidgetRef,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub z: i32,
    pub mode: InputMode,
    pub visible: bool,
    pub keyboard: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeySpec {
    pub id: u32,
    pub vk: u16,
    pub modifiers: u16,
    pub flags: u16,
    pub action: HotkeyAction,
    pub target: Option<WidgetRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub revision: u64,
    pub request_id: u32,
    pub width: u32,
    pub height: u32,
    pub widgets: Vec<WidgetSpec>,
    pub hotkeys: Vec<HotkeySpec>,
}
pub fn valid_trigger(vk: u16) -> bool {
    (1..=255).contains(&vk) && !matches!(vk, 1 | 2 | 4 | 5 | 6 | 16 | 17 | 18 | 91 | 92 | 160..=165)
}
/// Physical modifier bits are left/right Shift, Ctrl, Alt and Meta (eight bits).
pub fn modifiers_match(required: u16, physical: u8, extra: bool) -> bool {
    for family in 0..4 {
        let want = (required >> (family * 3)) & 7;
        let have = (physical >> (family * 2)) & 3;
        let matches = match want {
            0 => extra || have == 0,
            1 => have != 0,
            2 => {
                if extra {
                    have & 1 != 0
                } else {
                    have == 1
                }
            }
            4 => {
                if extra {
                    have & 2 != 0
                } else {
                    have == 2
                }
            }
            6 => have == 3,
            _ => false,
        };
        if !matches {
            return false;
        }
    }
    true
}
impl RoutingConfig {
    pub fn validate(&self) -> Result<(), ProtoError> {
        check(self.epoch != 0 && self.revision != 0 && self.request_id != 0)?;
        check(self.width > 0 && self.height > 0 && self.width <= i32::MAX as u32 && self.height <= i32::MAX as u32)?;
        check(self.widgets.len() <= MAX_WIDGETS && self.hotkeys.len() <= MAX_HOTKEYS)?;
        for (i, w) in self.widgets.iter().enumerate() {
            w.instance.validate()?;
            check(w.width >= 0 && w.height >= 0)?;
            check(!self.widgets[..i].iter().any(|old| old.instance.id == w.instance.id || old.z == w.z))?;
        }
        check(self.hotkeys.iter().filter(|h| h.action == HotkeyAction::ToggleEdit).count() == 1)?;
        for (i, h) in self.hotkeys.iter().enumerate() {
            check(h.id != 0 && valid_trigger(h.vk) && h.modifiers & !0xfff == 0 && h.flags & !3 == 0)?;
            for family in 0..4 {
                let bits = (h.modifiers >> (family * 3)) & 7;
                check(bits & 1 == 0 || bits == 1)?;
            }
            match h.action {
                HotkeyAction::ToggleEdit | HotkeyAction::Notify => check(h.target.is_none())?,
                HotkeyAction::Takeover | HotkeyAction::ReleaseTakeover => {
                    let target = h.target.ok_or(ProtoError::BadInput)?;
                    target.validate()?;
                    check(self.widgets.iter().any(|w| w.instance == target))?;
                }
            }
            for old in &self.hotkeys[..i] {
                check(old.id != h.id)?;
                if old.vk == h.vk {
                    check(!(0..=255).any(|physical| {
                        modifiers_match(old.modifiers, physical, old.flags & 2 != 0)
                            && modifiers_match(h.modifiers, physical, h.flags & 2 != 0)
                    }))?;
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedEvent {
    #[serde(with = "json::counter")]
    pub revision: u64,
    pub target: Option<WidgetRef>,
    pub kind: EventKind,
    pub argument: u32,
    pub input: Option<InputRecord>,
}
impl RoutedEvent {
    pub fn validate(&self) -> Result<(), ProtoError> {
        check(self.revision != 0)?;
        if let Some(target) = self.target {
            target.validate()?;
        }
        match self.kind {
            EventKind::Input => {
                check(self.target.is_some() && self.argument == 0)?;
                self.input.ok_or(ProtoError::BadInput)?.validate()?;
            }
            EventKind::Hotkey => check(self.target.is_none() && self.argument != 0 && self.input.is_none())?,
            EventKind::Cancel => {
                check(self.target.is_some() && self.input.is_none() && self.argument <= u8::MAX as u32)?;
                InputReason::from_code(self.argument as u8)?;
            }
            EventKind::Focus => check(self.target.is_some() && self.input.is_none() && self.argument <= 1)?,
            EventKind::Leave => check(self.target.is_some() && self.input.is_none() && self.argument == 0)?,
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedBatch {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub first_seq: u64,
    pub events: Vec<RoutedEvent>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingState {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub revision: u64,
    #[serde(default, with = "json::optional_counter")]
    pub state_seq: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub pointer: Option<WidgetRef>,
    pub focus: Option<WidgetRef>,
    pub takeover: Option<WidgetRef>,
    pub exclusive: bool,
    pub domains: Domains,
    pub reason: InputReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingRequest {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub revision: u64,
    pub request_id: u32,
    pub target: Option<WidgetRef>,
    pub action: RoutingAction,
    #[serde(default, with = "json::optional_counter")]
    pub state_seq: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingResult {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub revision: u64,
    pub request_id: u32,
    pub code: RoutingCode,
    #[serde(with = "json::counter")]
    pub through_seq: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiCoverage {
    pub status: NativeStatus,
    pub domains: Domains,
    pub abi_mask: u16,
    pub detail: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCoverage {
    #[serde(with = "json::counter")]
    pub epoch: u64,
    #[serde(with = "json::counter")]
    pub revision: u64,
    pub directinput: ApiCoverage,
    pub gameinput: ApiCoverage,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingMessage {
    Hello { module_version: u32, state_layout: u16, input_version: u16 },
    Welcome { input_version: u16 },
    State(RoutingState),
    Configure(RoutingConfig),
    Configured(RoutingResult),
    Events(RoutedBatch),
    Challenge(InputChallenge),
    Ack(InputAck),
    Request(RoutingRequest),
    Result(RoutingResult),
    Coverage(NativeCoverage),
    Visibility { state_seq: u64, on: bool },
}
pub(crate) fn check(value: bool) -> Result<(), ProtoError> {
    if value { Ok(()) } else { Err(ProtoError::BadInput) }
}
impl RoutingMessage {
    pub fn validate(&self) -> Result<(), ProtoError> {
        match self {
            Self::Hello { .. } | Self::Welcome { .. } => Ok(()),
            Self::Configure(config) => config.validate(),
            Self::Events(batch) => {
                check(
                    batch.epoch != 0 && batch.first_seq != 0 && !batch.events.is_empty() && batch.events.len() <= 64,
                )?;
                check(batch.first_seq.checked_add(batch.events.len() as u64 - 1).is_some())?;
                batch.events.iter().try_for_each(RoutedEvent::validate)
            }
            Self::Configured(r) | Self::Result(r) => check(r.epoch != 0 && r.revision != 0 && r.request_id != 0),
            Self::Request(r) => {
                check(r.epoch != 0 && r.revision != 0 && r.request_id != 0 && r.state_seq != Some(0))?;
                match r.action {
                    RoutingAction::EnterEdit | RoutingAction::LeaveEdit => check(r.target.is_none()),
                    RoutingAction::Takeover | RoutingAction::ReleaseTakeover => {
                        r.target.ok_or(ProtoError::BadInput)?.validate()
                    }
                }
            }
            Self::State(s) => {
                check(
                    s.epoch != 0 && s.state_seq != Some(0) && s.width <= i32::MAX as u32 && s.height <= i32::MAX as u32,
                )?;
                for target in [s.pointer, s.focus, s.takeover].into_iter().flatten() {
                    target.validate()?;
                }
                check(!s.exclusive || (s.domains.keyboard && s.domains.mouse))?;
                check(s.takeover.is_none() || s.exclusive)?;
                check(
                    s.revision != 0
                        || (!s.exclusive
                            && s.domains == Domains::default()
                            && s.pointer.is_none()
                            && s.focus.is_none()
                            && s.takeover.is_none()),
                )
            }
            Self::Challenge(c) => check(c.epoch != 0 && c.nonce != 0),
            Self::Ack(c) => check(c.epoch != 0 && c.nonce != 0),
            Self::Visibility { .. } => Ok(()),
            Self::Coverage(c) => {
                check(c.epoch != 0 && c.revision != 0)?;
                for (api, mask) in [(c.directinput, 3), (c.gameinput, 15)] {
                    check(api.abi_mask & !mask == 0)?;
                    if api.status == NativeStatus::Supported {
                        check(api.abi_mask != 0 && api.domains.bits() != 0)?;
                    }
                    if api.status != NativeStatus::Supported {
                        check(api.domains.bits() == 0)?;
                    }
                    if api.status == NativeStatus::Absent {
                        check(api.abi_mask == 0 && api.domains.bits() == 0)?;
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingFirst {
    Hello { module_version: u32 },
    VersionMismatch,
    Bad,
}
pub fn classify_hello(message: &crate::pipe::OverlayMessage) -> RoutingFirst {
    use crate::pipe::OverlayMessage;
    match message {
        OverlayMessage::Routing(RoutingMessage::Hello { module_version, state_layout: 1, input_version: 2 }) => {
            RoutingFirst::Hello { module_version: *module_version }
        }
        OverlayMessage::Routing(RoutingMessage::Hello { .. }) | OverlayMessage::Input(_) => {
            RoutingFirst::VersionMismatch
        }
        _ => RoutingFirst::Bad,
    }
}
