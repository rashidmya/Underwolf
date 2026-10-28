#![cfg(target_os = "linux")]
use std::{io::Write, os::unix::net::UnixStream};
use uw_frame_channel::*;
use uw_frame_proto::*;
fn message() -> Message {
    Message::Hello {
        pid: 1,
        start_ticks: 1,
        alignment: 4096,
        mutex_size: 40,
        mutex_align: 8,
        abi: 1,
        capability: 1,
    }
}
#[test]
fn a_message_survives_every_byte_arriving_separately() {
    let (mut a, b) = UnixStream::pair().unwrap();
    let expected = message();
    let bytes = encode_message(&expected).unwrap();
    let worker = std::thread::spawn(move || {
        for byte in bytes {
            a.write_all(&[byte]).unwrap();
        }
    });
    let (actual, fds) = recv_message(&b).unwrap();
    assert_eq!(actual, expected);
    assert!(fds.is_empty());
    worker.join().unwrap();
}
#[test]
fn sending_and_receiving_reject_wrong_rights_and_truncated_payloads() {
    let (a, b) = UnixStream::pair().unwrap();
    send_message(&a, &message(), &[]).unwrap();
    assert_eq!(recv_message(&b).unwrap().0, message());
    let mut raw = encode_message(&message()).unwrap();
    raw.truncate(20);
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(&raw).unwrap();
    a.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(recv_message(&b).is_err());
    let mut raw = encode_message(&message()).unwrap();
    raw[12] = 2;
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(&raw).unwrap();
    assert!(recv_message(&b).is_err());
    let mut raw = encode_message(&message()).unwrap();
    raw[8..12].copy_from_slice(&4097u32.to_le_bytes());
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(&raw[..16]).unwrap();
    assert!(recv_message(&b).is_err());
}

#[test]
fn malformed_send_does_not_poison_the_stream_and_rights_outlive_the_sender_mapping() {
    use std::os::fd::AsFd;
    let (a, b) = UnixStream::pair().unwrap();
    let null = std::fs::File::open("/dev/null").unwrap();
    assert!(send_message(&a, &message(), &[null.as_fd()]).is_err());
    send_message(&a, &message(), &[]).unwrap();
    assert_eq!(recv_message(&b).unwrap().0, message());
    let key = AllocationKey {
        session: SessionId([2; 16]),
        widget: WidgetKey {
            id: 2,
            generation: 1,
        },
        generation: 1,
    };
    let map = FrameMapping::create(
        key,
        FrameLayout::new(4, 2, 4096).unwrap(),
        PeerIdentity::current().unwrap(),
    )
    .unwrap();
    map.try_publish(
        1,
        Patch {
            rect: Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
            stride: 16,
            data: &[7; 32],
        },
    )
    .unwrap();
    let header = map.header();
    // SAFETY: the receiver follows the same native initialization and ownership protocol.
    send_message(&a, &Message::Register(header), &unsafe {
        map.descriptors()
    })
    .unwrap();
    drop(map);
    let (received, fds) = recv_message(&b).unwrap();
    assert_eq!(received, Message::Register(header));
    assert_eq!(fds.len(), 2);
    // SAFETY: the queued descriptors retain our producer's correctly initialized mutex.
    let restored = std::sync::Arc::new(
        unsafe {
            FrameMapping::open(
                &header.encode().unwrap(),
                fds.try_into().unwrap(),
                PeerIdentity::current().unwrap(),
            )
        }
        .unwrap(),
    );
    let lease = restored.try_claim().unwrap().unwrap();
    assert_eq!(&lease.copy_pixels().unwrap()[..16], &[7; 16]);
    restored.try_cancel_unsubmitted(&lease).unwrap();
}

fn send_rights(stream: &UnixStream, bytes: &[u8], fds: &[std::os::fd::BorrowedFd<'_>]) {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let mut storage = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut ancillary = SendAncillaryBuffer::new(&mut storage);
    assert!(ancillary.push(SendAncillaryMessage::ScmRights(fds)));
    assert_eq!(
        sendmsg(
            stream,
            &[std::io::IoSlice::new(bytes)],
            &mut ancillary,
            SendFlags::NOSIGNAL
        )
        .unwrap(),
        bytes.len()
    );
}

#[test]
fn extra_truncated_or_late_descriptor_rights_are_closed_and_rejected() {
    use std::os::fd::AsFd;
    let null = std::fs::File::open("/dev/null").unwrap();
    let bytes = encode_message(&message()).unwrap();
    for count in [1, 3] {
        let (a, b) = UnixStream::pair().unwrap();
        send_rights(&a, &bytes, &vec![null.as_fd(); count]);
        assert!(recv_message(&b).is_err());
    }
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(&bytes[..1]).unwrap();
    send_rights(&a, &bytes[1..], &[null.as_fd()]);
    assert!(recv_message(&b).is_err());
}

#[test]
fn timeout_after_a_partial_header_is_not_an_idle_retry() {
    let (mut a, b) = UnixStream::pair().unwrap();
    a.write_all(b"U").unwrap();
    assert_eq!(recv_message(&b).unwrap_err(), FrameError::Io);
    let (_a, b) = UnixStream::pair().unwrap();
    assert_eq!(recv_message(&b).unwrap_err(), FrameError::Busy);
}

#[test]
fn a_hundred_rejected_rights_messages_do_not_leak_descriptors() {
    use std::os::fd::AsFd;
    // Count in an isolated subprocess so other parallel tests cannot affect it.
    if std::env::var_os("UW_RIGHTS_COUNT_ACTOR").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_hundred_rejected_rights_messages_do_not_leak_descriptors",
                "--test-threads=1",
            ])
            .env("UW_RIGHTS_COUNT_ACTOR", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let null = std::fs::File::open("/dev/null").unwrap();
    let bytes = encode_message(&message()).unwrap();
    let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
    for _ in 0..100 {
        let (a, b) = UnixStream::pair().unwrap();
        send_rights(&a, &bytes, &[null.as_fd(); 3]);
        assert!(recv_message(&b).is_err());
    }
    assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), before);
}
