use super::*;
fn u16w(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u32w(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u64w(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn widget(out: &mut Vec<u8>, value: Option<WidgetRef>) {
    let value = value.unwrap_or(WidgetRef { id: 0, generation: 0 });
    u32w(out, value.id);
    u32w(out, value.generation);
}
fn result(out: &mut Vec<u8>, value: &RoutingResult) {
    u64w(out, value.epoch);
    u64w(out, value.revision);
    u32w(out, value.request_id);
    u16w(out, value.code as u16);
    u16w(out, 0);
    u64w(out, value.through_seq);
}
impl RoutingMessage {
    pub fn encode_payload(&self) -> Result<(u16, Vec<u8>), ProtoError> {
        self.validate()?;
        let mut out = Vec::new();
        let tag = match self {
            Self::Hello { module_version, state_layout, input_version } => {
                u32w(&mut out, *module_version);
                u16w(&mut out, *state_layout);
                u16w(&mut out, *input_version);
                0x120
            }
            Self::Welcome { input_version } => {
                u16w(&mut out, *input_version);
                u16w(&mut out, 0);
                0x121
            }
            Self::State(s) => {
                u64w(&mut out, s.epoch);
                u64w(&mut out, s.revision);
                u64w(&mut out, s.state_seq.unwrap_or(0));
                u32w(&mut out, s.width);
                u32w(&mut out, s.height);
                for target in [s.pointer, s.focus, s.takeover] {
                    widget(&mut out, target);
                }
                out.extend_from_slice(&[
                    s.exclusive as u8 | (s.state_seq.is_some() as u8) << 1,
                    s.domains.bits(),
                    s.reason as u8,
                    0,
                ]);
                0x122
            }
            Self::Configure(c) => {
                u64w(&mut out, c.epoch);
                u64w(&mut out, c.revision);
                u32w(&mut out, c.request_id);
                u32w(&mut out, c.width);
                u32w(&mut out, c.height);
                u16w(&mut out, c.widgets.len() as u16);
                u16w(&mut out, c.hotkeys.len() as u16);
                for w in &c.widgets {
                    widget(&mut out, Some(w.instance));
                    for value in [w.x, w.y, w.width, w.height, w.z] {
                        u32w(&mut out, value as u32);
                    }
                    out.extend_from_slice(&[w.mode as u8, w.visible as u8 | (w.keyboard as u8) << 1, 0, 0]);
                }
                for h in &c.hotkeys {
                    u32w(&mut out, h.id);
                    u16w(&mut out, h.vk);
                    u16w(&mut out, h.modifiers);
                    u16w(&mut out, h.flags);
                    u16w(&mut out, h.action as u16);
                    widget(&mut out, h.target);
                    u32w(&mut out, 0);
                }
                0x123
            }
            Self::Configured(r) => {
                result(&mut out, r);
                0x124
            }
            Self::Events(batch) => {
                u64w(&mut out, batch.epoch);
                u64w(&mut out, batch.first_seq);
                u16w(&mut out, batch.events.len() as u16);
                u16w(&mut out, 0);
                for e in &batch.events {
                    u64w(&mut out, e.revision);
                    widget(&mut out, e.target);
                    out.extend_from_slice(&[e.kind as u8, 0, 0, 0]);
                    u32w(&mut out, e.argument);
                    if let Some(input) = e.input {
                        out.extend_from_slice(&[input.kind, input.flags, 0, 0]);
                        for value in [input.a, input.b, input.c, input.d] {
                            u32w(&mut out, value as u32);
                        }
                        u32w(&mut out, input.modifiers);
                    } else {
                        out.extend_from_slice(&[0; 24]);
                    }
                }
                0x125
            }
            Self::Challenge(c) => {
                for value in [c.epoch, c.nonce, c.through_seq] {
                    u64w(&mut out, value);
                }
                0x126
            }
            Self::Ack(c) => {
                for value in [c.epoch, c.nonce, c.through_seq] {
                    u64w(&mut out, value);
                }
                0x127
            }
            Self::Request(r) => {
                u64w(&mut out, r.epoch);
                u64w(&mut out, r.revision);
                u32w(&mut out, r.request_id);
                widget(&mut out, r.target);
                out.extend_from_slice(&[r.action as u8, r.state_seq.is_some() as u8, 0, 0]);
                u64w(&mut out, r.state_seq.unwrap_or(0));
                0x128
            }
            Self::Result(r) => {
                result(&mut out, r);
                0x129
            }
            Self::Coverage(c) => {
                u64w(&mut out, c.epoch);
                u64w(&mut out, c.revision);
                for api in [c.directinput, c.gameinput] {
                    out.extend_from_slice(&[api.status as u8, api.domains.bits()]);
                    u16w(&mut out, api.abi_mask);
                    u32w(&mut out, api.detail);
                }
                0x12a
            }
            Self::Visibility { state_seq, on } => {
                u64w(&mut out, *state_seq);
                out.push(*on as u8);
                out.extend_from_slice(&[0; 7]);
                0x12b
            }
        };
        Ok((tag, out))
    }
    pub fn decode(tag: u16, bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader { bytes, at: 0 };
        let message = match tag {
            0x120 => Self::Hello { module_version: r.u32()?, state_layout: r.u16()?, input_version: r.u16()? },
            0x121 => {
                let input_version = r.u16()?;
                r.zero(2)?;
                Self::Welcome { input_version }
            }
            0x122 => {
                let epoch = r.u64()?;
                let revision = r.u64()?;
                let seq = r.u64()?;
                let width = r.u32()?;
                let height = r.u32()?;
                let pointer = r.widget()?;
                let focus = r.widget()?;
                let takeover = r.widget()?;
                let flags = r.u8()?;
                check(flags & !3 == 0)?;
                let domains = Domains::from_bits(r.u8()?)?;
                let reason = InputReason::from_code(r.u8()?)?;
                r.zero(1)?;
                Self::State(RoutingState {
                    epoch,
                    revision,
                    state_seq: sequence(flags & 2 != 0, seq)?,
                    width,
                    height,
                    pointer,
                    focus,
                    takeover,
                    exclusive: flags & 1 != 0,
                    domains,
                    reason,
                })
            }
            0x123 => {
                let epoch = r.u64()?;
                let revision = r.u64()?;
                let request_id = r.u32()?;
                let width = r.u32()?;
                let height = r.u32()?;
                let wc = r.u16()? as usize;
                let hc = r.u16()? as usize;
                check(wc <= MAX_WIDGETS && hc <= MAX_HOTKEYS && r.remaining() == wc * 32 + hc * 24)?;
                let mut widgets = Vec::with_capacity(wc);
                let mut hotkeys = Vec::with_capacity(hc);
                for _ in 0..wc {
                    let instance = r.widget()?.ok_or(ProtoError::BadInput)?;
                    let x = r.u32()? as i32;
                    let y = r.u32()? as i32;
                    let width = r.u32()? as i32;
                    let height = r.u32()? as i32;
                    let z = r.u32()? as i32;
                    let mode = InputMode::from_code(r.u8()? as u16)?;
                    let flags = r.u8()?;
                    check(flags & !3 == 0)?;
                    r.zero(2)?;
                    widgets.push(WidgetSpec {
                        instance,
                        x,
                        y,
                        width,
                        height,
                        z,
                        mode,
                        visible: flags & 1 != 0,
                        keyboard: flags & 2 != 0,
                    });
                }
                for _ in 0..hc {
                    let id = r.u32()?;
                    let vk = r.u16()?;
                    let modifiers = r.u16()?;
                    let flags = r.u16()?;
                    let action = HotkeyAction::from_code(r.u16()?)?;
                    let target = r.widget()?;
                    r.zero(4)?;
                    hotkeys.push(HotkeySpec { id, vk, modifiers, flags, action, target });
                }
                Self::Configure(RoutingConfig { epoch, revision, request_id, width, height, widgets, hotkeys })
            }
            0x124 => Self::Configured(r.result()?),
            0x125 => {
                let epoch = r.u64()?;
                let first_seq = r.u64()?;
                let count = r.u16()? as usize;
                r.zero(2)?;
                check((1..=64).contains(&count) && r.remaining() == count * ROUTED_RECORD_BYTES)?;
                let mut events = Vec::with_capacity(count);
                for _ in 0..count {
                    let revision = r.u64()?;
                    let target = r.widget()?;
                    let kind = EventKind::from_code(r.u8()? as u16)?;
                    r.zero(3)?;
                    let argument = r.u32()?;
                    let input = if kind == EventKind::Input {
                        let kind = r.u8()?;
                        let flags = r.u8()?;
                        r.zero(2)?;
                        Some(InputRecord {
                            kind,
                            flags,
                            a: r.u32()? as i32,
                            b: r.u32()? as i32,
                            c: r.u32()? as i32,
                            d: r.u32()? as i32,
                            modifiers: r.u32()?,
                        })
                    } else {
                        r.zero(24)?;
                        None
                    };
                    events.push(RoutedEvent { revision, target, kind, argument, input });
                }
                Self::Events(RoutedBatch { epoch, first_seq, events })
            }
            0x126 => Self::Challenge(InputChallenge { epoch: r.u64()?, nonce: r.u64()?, through_seq: r.u64()? }),
            0x127 => Self::Ack(InputAck { epoch: r.u64()?, nonce: r.u64()?, through_seq: r.u64()? }),
            0x128 => {
                let epoch = r.u64()?;
                let revision = r.u64()?;
                let request_id = r.u32()?;
                let target = r.widget()?;
                let action = RoutingAction::from_code(r.u8()? as u16)?;
                let has = r.boolean()?;
                r.zero(2)?;
                let seq = r.u64()?;
                Self::Request(RoutingRequest {
                    epoch,
                    revision,
                    request_id,
                    target,
                    action,
                    state_seq: sequence(has, seq)?,
                })
            }
            0x129 => Self::Result(r.result()?),
            0x12a => Self::Coverage(NativeCoverage {
                epoch: r.u64()?,
                revision: r.u64()?,
                directinput: r.coverage()?,
                gameinput: r.coverage()?,
            }),
            0x12b => {
                let state_seq = r.u64()?;
                let on = r.boolean()?;
                r.zero(7)?;
                Self::Visibility { state_seq, on }
            }
            _ => return Err(ProtoError::UnknownType(tag)),
        };
        check(r.remaining() == 0)?;
        message.validate()?;
        Ok(message)
    }
}
fn sequence(has: bool, seq: u64) -> Result<Option<u64>, ProtoError> {
    check(if has { seq != 0 } else { seq == 0 })?;
    Ok(has.then_some(seq))
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        let end = self.at.checked_add(n).ok_or(ProtoError::BadInput)?;
        let value = self.bytes.get(self.at..end).ok_or(ProtoError::BadInput)?;
        self.at = end;
        Ok(value)
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn zero(&mut self, n: usize) -> Result<(), ProtoError> {
        check(self.take(n)?.iter().all(|byte| *byte == 0))
    }
    fn boolean(&mut self) -> Result<bool, ProtoError> {
        let b = self.u8()?;
        check(b <= 1)?;
        Ok(b != 0)
    }
    fn widget(&mut self) -> Result<Option<WidgetRef>, ProtoError> {
        let id = self.u32()?;
        let generation = self.u32()?;
        if id == 0 && generation == 0 {
            Ok(None)
        } else {
            let value = WidgetRef { id, generation };
            value.validate()?;
            Ok(Some(value))
        }
    }
    fn result(&mut self) -> Result<RoutingResult, ProtoError> {
        let epoch = self.u64()?;
        let revision = self.u64()?;
        let request_id = self.u32()?;
        let code = RoutingCode::from_code(self.u16()?)?;
        self.zero(2)?;
        Ok(RoutingResult { epoch, revision, request_id, code, through_seq: self.u64()? })
    }
    fn coverage(&mut self) -> Result<ApiCoverage, ProtoError> {
        Ok(ApiCoverage {
            status: NativeStatus::from_code(self.u8()? as u16)?,
            domains: Domains::from_bits(self.u8()?)?,
            abi_mask: self.u16()?,
            detail: self.u32()?,
        })
    }
}
