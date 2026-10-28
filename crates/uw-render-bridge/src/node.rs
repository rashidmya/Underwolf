//! All JS access stays on the calling thread. The reactor owns native data only.
use crate::*;
use napi::{Error, Result, bindgen_prelude::Buffer};
use napi_derive::napi;
use std::sync::Mutex;
use uw_frame_channel::{Patch, PeerIdentity, PublishResult};
use uw_frame_proto::{AllocationKey, FrameError, Message, Rect, WidgetKey};
static SERVER: Mutex<Option<ProducerSession>> = Mutex::new(None);
fn error(e: FrameError) -> Error {
    Error::from_reason(format!("UW_RENDER_{e:?}"))
}
fn value<T>(v: uw_frame_proto::Result<T>) -> Result<T> {
    v.map_err(error)
}
fn with<T>(f: impl FnOnce(&ProducerSession) -> Result<T>) -> Result<T> {
    let server = SERVER
        .lock()
        .map_err(|_| Error::from_reason("UW_RENDER_POISONED"))?;
    f(server
        .as_ref()
        .ok_or_else(|| Error::from_reason("UW_RENDER_STOPPED"))?)
}
#[napi(object)]
pub struct WidgetJs {
    pub id: f64,
    pub generation: f64,
}
impl WidgetJs {
    fn key(&self) -> Result<WidgetKey> {
        let w = WidgetKey {
            id: value(uint(self.id))?,
            generation: value(uint(self.generation))?,
        };
        value(w.validate())?;
        Ok(w)
    }
}
impl From<WidgetKey> for WidgetJs {
    fn from(w: WidgetKey) -> Self {
        Self {
            id: w.id as f64,
            generation: w.generation as f64,
        }
    }
}
#[napi(object)]
pub struct FrameKeyJs {
    pub session: String,
    pub widget: WidgetJs,
    pub allocation: String,
}
impl FrameKeyJs {
    fn key(&self) -> Result<AllocationKey> {
        Ok(AllocationKey {
            session: value(parse_session(&self.session))?,
            widget: self.widget.key()?,
            generation: value(parse_counter(&self.allocation, false))?,
        })
    }
}
impl From<AllocationKey> for FrameKeyJs {
    fn from(k: AllocationKey) -> Self {
        Self {
            session: format_session(k.session),
            widget: k.widget.into(),
            allocation: k.generation.to_string(),
        }
    }
}
#[napi(object)]
pub struct PeerJs {
    pub pid: f64,
    pub start_ticks: String,
}
#[napi(object)]
pub struct CreateJs {
    pub id: f64,
    pub generation: f64,
    pub width: f64,
    pub height: f64,
}
#[napi(object)]
pub struct PatchJs {
    pub key: FrameKeyJs,
    pub seq: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub stride: f64,
    pub data: Buffer,
}
#[napi(object)]
pub struct PublishJs {
    pub status: String,
    pub seq: String,
    pub bytes: String,
}
#[napi(object)]
pub struct LayoutJs {
    pub widget: WidgetJs,
    pub revision: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub z: f64,
    pub visible: bool,
}
#[napi(object)]
pub struct DimmingJs {
    pub revision: String,
    pub active: bool,
}
#[napi(object)]
#[derive(Default)]
pub struct EventJs {
    pub kind: String,
    pub session: Option<String>,
    pub key: Option<FrameKeyJs>,
    pub widget: Option<WidgetJs>,
    pub seq: Option<String>,
    pub generation: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub ready: Option<bool>,
    pub swapchain_generation: Option<String>,
    pub completed_draw: Option<String>,
    pub monotonic_ns: Option<String>,
    pub reason: Option<String>,
}
impl From<BridgeEvent> for EventJs {
    fn from(e: BridgeEvent) -> Self {
        let mut out = Self::default();
        match e {
            BridgeEvent::Ready { session } => {
                out.kind = "ready".into();
                out.session = Some(format_session(session));
            }
            BridgeEvent::Registered { key } => {
                out.kind = "registered".into();
                out.key = Some(key.into());
            }
            BridgeEvent::Consumed { key, seq } => {
                out.kind = "consumed".into();
                out.key = Some(key.into());
                out.seq = Some(seq.to_string());
            }
            BridgeEvent::Retired { key } => {
                out.kind = "retired".into();
                out.key = Some(key.into());
            }
            BridgeEvent::NeedBaseline { widget } => {
                out.kind = "need_baseline".into();
                out.widget = Some(widget.into());
            }
            BridgeEvent::Swapchain {
                generation,
                width,
                height,
                ready,
            } => {
                out.kind = "swapchain".into();
                out.generation = Some(generation.to_string());
                out.width = Some(width as f64);
                out.height = Some(height as f64);
                out.ready = Some(ready);
            }
            BridgeEvent::Progress {
                swapchain_generation,
                completed_draw,
                monotonic_ns,
            } => {
                out.kind = "progress".into();
                out.swapchain_generation = Some(swapchain_generation.to_string());
                out.completed_draw = Some(completed_draw.to_string());
                out.monotonic_ns = Some(monotonic_ns.to_string());
            }
            BridgeEvent::Disconnected { reason } => {
                out.kind = "disconnected".into();
                out.reason = Some(reason);
            }
            BridgeEvent::Unavailable { reason } => {
                out.kind = "unavailable".into();
                out.reason = Some(reason);
            }
        }
        out
    }
}
#[napi(object)]
pub struct StatsJs {
    pub cpu_bytes: String,
    pub publications: String,
    pub coalesced_hints: String,
    pub live_pixel_bytes: String,
    pub retired_pixel_bytes: String,
}
#[napi]
pub fn start_server(peer: PeerJs) -> Result<()> {
    let pid = value(uint(peer.pid))?;
    let expected = value(parse_counter(&peer.start_ticks, false))?;
    let identity = value(PeerIdentity::for_pid(pid))?;
    if identity.start_ticks != expected {
        return Err(error(FrameError::Stale));
    }
    let mut server = SERVER
        .lock()
        .map_err(|_| Error::from_reason("UW_RENDER_POISONED"))?;
    if server.is_some() {
        return Err(error(FrameError::Busy));
    }
    *server = Some(value(ProducerSession::bind(identity))?);
    Ok(())
}
#[napi]
pub fn create_widget(spec: CreateJs) -> Result<FrameKeyJs> {
    with(|s| {
        let widget = WidgetJs {
            id: spec.id,
            generation: spec.generation,
        }
        .key()?;
        Ok(
            value(s.create_widget(widget, value(uint(spec.width))?, value(uint(spec.height))?))?
                .into(),
        )
    })
}
#[napi]
pub fn can_publish(key: FrameKeyJs) -> Result<bool> {
    let key = key.key()?;
    with(|s| Ok(s.can_publish(key)))
}
#[napi]
pub fn publish_patch(p: PatchJs) -> Result<PublishJs> {
    let key = p.key.key()?;
    let seq = value(parse_counter(&p.seq, false))?;
    let patch = Patch {
        rect: Rect {
            x: value(uint(p.x))?,
            y: value(uint(p.y))?,
            width: value(uint(p.width))?,
            height: value(uint(p.height))?,
        },
        stride: value(uint(p.stride))?,
        data: &p.data,
    };
    // The borrowed JS bytes are copied synchronously under the mapping mutex;
    // no Buffer, Env, reference or pixel slice enters the native worker.
    with(|s| {
        Ok(match value(s.publish_patch(key, seq, patch))? {
            PublishResult::Published { seq, bytes } => PublishJs {
                status: "published".into(),
                seq: seq.to_string(),
                bytes: bytes.to_string(),
            },
            PublishResult::Busy => PublishJs {
                status: "busy".into(),
                seq: p.seq.clone(),
                bytes: "0".into(),
            },
            PublishResult::Stale => PublishJs {
                status: "stale".into(),
                seq: p.seq.clone(),
                bytes: "0".into(),
            },
        })
    })
}
#[napi]
pub fn set_layout(p: LayoutJs) -> Result<()> {
    let message = Message::Layout {
        widget: p.widget.key()?,
        revision: value(parse_counter(&p.revision, false))?,
        x: value(int(p.x))?,
        y: value(int(p.y))?,
        width: value(uint(p.width))?,
        height: value(uint(p.height))?,
        z: value(int(p.z))?,
        visible: p.visible,
    };
    with(|s| value(s.control(message)))
}
#[napi]
pub fn set_dimming(p: DimmingJs) -> Result<()> {
    let message = Message::Dimming {
        revision: value(parse_counter(&p.revision, false))?,
        active: p.active,
    };
    with(|s| value(s.control(message)))
}
#[napi]
pub fn remove_widget(widget: WidgetJs, revision: String) -> Result<()> {
    let message = Message::Remove {
        widget: widget.key()?,
        revision: value(parse_counter(&revision, false))?,
    };
    with(|s| value(s.control(message)))
}
#[napi]
pub fn drain_events() -> Result<Vec<EventJs>> {
    with(|s| Ok(s.drain_events().into_iter().map(Into::into).collect()))
}
#[napi]
pub fn stats() -> Result<StatsJs> {
    with(|s| {
        let s = s.stats();
        Ok(StatsJs {
            cpu_bytes: s.cpu_bytes.to_string(),
            publications: s.publications.to_string(),
            coalesced_hints: s.coalesced_hints.to_string(),
            live_pixel_bytes: s.live_pixel_bytes.to_string(),
            retired_pixel_bytes: s.retired_pixel_bytes.to_string(),
        })
    })
}
#[napi]
pub fn stop() -> Result<()> {
    let mut session = SERVER
        .lock()
        .map_err(|_| Error::from_reason("UW_RENDER_POISONED"))?
        .take();
    if let Some(s) = session.as_mut() {
        s.stop();
    }
    Ok(())
}
#[napi]
pub fn monotonic_now_ns() -> Result<String> {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one valid timespec, and CLOCK_MONOTONIC is
    // verified to share the consumer's time namespace by the handshake.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0 {
        return Err(error(FrameError::Io));
    }
    Ok((t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64).to_string())
}
