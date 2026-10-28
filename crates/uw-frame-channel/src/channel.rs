//! A message owns its descriptor rights only on its first byte. All operations
//! run on socket workers and have a bounded total deadline, including short I/O.
use crate::Result;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use std::{
    io::{IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{
        fd::{BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
    time::{Duration, Instant},
};
use uw_frame_proto::{
    FrameError, MAX_FDS, Message, WIRE_HEADER_BYTES, decode_envelope, decode_message,
    encode_message,
};
const DEADLINE: Duration = Duration::from_millis(100);
fn remaining(deadline: Instant, idle: bool) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(if idle {
            FrameError::Busy
        } else {
            FrameError::Io
        })
}
/// Concurrent writers must serialize whole messages; the session has one writer.
pub fn send_message(stream: &UnixStream, message: &Message, fds: &[BorrowedFd<'_>]) -> Result<()> {
    if fds.len() != message.expected_fds() || fds.len() > MAX_FDS {
        return Err(FrameError::Invalid);
    }
    let bytes = encode_message(message)?;
    let deadline = Instant::now() + DEADLINE;
    let mut sent = 0;
    while sent < bytes.len() {
        stream
            .set_write_timeout(Some(remaining(deadline, false)?))
            .map_err(|_| FrameError::Io)?;
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        if sent == 0 && !fds.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(FrameError::Invalid);
        }
        match sendmsg(
            stream,
            &[IoSlice::new(&bytes[sent..])],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        ) {
            Ok(0) => return Err(FrameError::Io),
            Ok(n) => sent += n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(FrameError::Io),
        }
    }
    Ok(())
}
fn receive_part(
    stream: &UnixStream,
    bytes: &mut [u8],
    deadline: Instant,
    idle: bool,
) -> Result<(usize, Vec<OwnedFd>)> {
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline, idle)?))
            .map_err(|_| FrameError::Io)?;
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let result = match recvmsg(
            stream,
            &mut [IoSliceMut::new(bytes)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Ok(result) => result,
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) if idle && error == rustix::io::Errno::AGAIN => {
                return Err(FrameError::Busy);
            }
            Err(_) => return Err(FrameError::Io),
        };
        let mut fds = Vec::new();
        let mut unknown = false;
        for item in ancillary.drain() {
            match item {
                RecvAncillaryMessage::ScmRights(rights) => fds.extend(rights),
                _ => unknown = true,
            }
        }
        if result.bytes == 0 {
            return Err(FrameError::Io);
        }
        if result.bytes > bytes.len()
            || result
                .flags
                .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
            || unknown
            || fds.len() > MAX_FDS
        {
            return Err(FrameError::Invalid);
        }
        return Ok((result.bytes, fds));
    }
}
fn read_remainder(stream: &UnixStream, bytes: &mut [u8], deadline: Instant) -> Result<()> {
    let mut at = 0;
    while at < bytes.len() {
        let (count, fds) = receive_part(stream, &mut bytes[at..], deadline, false)?;
        if !fds.is_empty() {
            return Err(FrameError::Invalid);
        }
        at += count;
    }
    Ok(())
}
/// Busy means no byte was consumed. Every other error invalidates this stream;
/// callers must close it rather than retry from a partially consumed header.
pub fn recv_message(stream: &UnixStream) -> Result<(Message, Vec<OwnedFd>)> {
    let deadline = Instant::now() + DEADLINE;
    let mut header = [0; WIRE_HEADER_BYTES];
    let (_, fds) = receive_part(stream, &mut header[..1], deadline, true)?;
    read_remainder(stream, &mut header[1..], deadline)?;
    let envelope = decode_envelope(&header)?;
    if fds.len() != envelope.fd_count {
        return Err(FrameError::Invalid);
    }
    let mut bytes = vec![0; WIRE_HEADER_BYTES + envelope.payload_len];
    bytes[..WIRE_HEADER_BYTES].copy_from_slice(&header);
    read_remainder(stream, &mut bytes[WIRE_HEADER_BYTES..], deadline)?;
    Ok((decode_message(&bytes)?, fds))
}
