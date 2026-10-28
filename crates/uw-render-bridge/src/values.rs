use uw_frame_proto::{FrameError, Result, SessionId};
pub fn parse_counter(text: &str, zero: bool) -> Result<u64> {
    if text.is_empty()
        || !text.bytes().all(|b| b.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return Err(FrameError::Invalid);
    }
    let n = text.parse().map_err(|_| FrameError::Overflow)?;
    if n == 0 && !zero {
        return Err(FrameError::Invalid);
    }
    Ok(n)
}
pub fn parse_session(text: &str) -> Result<SessionId> {
    if text.len() != 32
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(FrameError::Invalid);
    }
    let mut bytes = [0; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| FrameError::Invalid)?;
    }
    if bytes == [0; 16] {
        return Err(FrameError::Invalid);
    }
    Ok(SessionId(bytes))
}
pub fn format_session(id: SessionId) -> String {
    id.0.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn uint(n: f64) -> Result<u32> {
    if !n.is_finite() || n.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&n) {
        return Err(FrameError::Invalid);
    }
    Ok(n as u32)
}
pub fn int(n: f64) -> Result<i32> {
    if !n.is_finite() || n.fract() != 0.0 || !(i32::MIN as f64..=i32::MAX as f64).contains(&n) {
        return Err(FrameError::Invalid);
    }
    Ok(n as i32)
}
