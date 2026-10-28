//! CPU-only channel peer for the addon boundary smoke test. Never a GPU proof.
#[cfg(target_os = "linux")]
fn main() {
    use std::{
        io::{self, BufRead},
        os::unix::net::UnixStream,
        sync::Arc,
        time::{Duration, Instant},
    };
    use uw_frame_channel::*;
    use uw_frame_proto::*;
    fn retry_busy<T>(mut operation: impl FnMut() -> uw_frame_proto::Result<T>) -> T {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            match operation() {
                Ok(value) => return value,
                Err(FrameError::Busy) => {
                    assert!(Instant::now() < until, "metadata stayed busy");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("channel operation: {error:?}"),
            }
        }
    }
    fn receive(s: &UnixStream) -> (Message, Vec<std::os::fd::OwnedFd>) {
        loop {
            match recv_message(s) {
                Err(FrameError::Busy) => {}
                r => return r.unwrap(),
            }
        }
    }
    let peer = PeerIdentity::current().unwrap();
    println!("IDENTITY {} {}", peer.pid, peer.start_ticks);
    let path = session_endpoint(&peer).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match UnixStream::connect(&path) {
            Ok(s) => break s,
            Err(_) => {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    send_message(
        &stream,
        &Message::Hello {
            pid: peer.pid,
            start_ticks: peer.start_ticks,
            alignment: 4096,
            mutex_size: 40,
            mutex_align: 8,
            abi: 1,
            capability: 1,
        },
        &[],
    )
    .unwrap();
    let (Message::Welcome { .. }, _) = receive(&stream) else {
        panic!("welcome")
    };
    println!("CONNECTED");
    let initial_fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
    let mut mapping: Option<Arc<FrameMapping>> = None;
    let mut lease: Option<ReadLease> = None;
    for line in io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "register" => {
                loop {
                    let (m, fds) = receive(&stream);
                    if let Message::Register(h) = m {
                        if let Some(old) = mapping.take() {
                            let key = old.header().key;
                            drop(old);
                            send_message(&stream, &Message::Retired { key }, &[]).unwrap();
                        }
                        // SAFETY: authenticated addon initializes native metadata and mutex.
                        mapping = Some(Arc::new(
                            unsafe {
                                FrameMapping::open(
                                    &h.encode().unwrap(),
                                    fds.try_into().unwrap(),
                                    PeerIdentity::from_stream(&stream).unwrap(),
                                )
                            }
                            .unwrap(),
                        ));
                        send_message(&stream, &Message::Registered { key: h.key }, &[]).unwrap();
                        println!("REGISTERED {}", h.key.generation);
                        break;
                    }
                }
            }
            "claim" => {
                let m = mapping.as_ref().unwrap();
                let until = Instant::now() + Duration::from_secs(3);
                let l = loop {
                    if let Some(l) = retry_busy(|| m.try_claim()) {
                        break l;
                    }
                    assert!(Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(2));
                };
                let pixels = retry_busy(|| l.copy_pixels());
                println!(
                    "CLAIMED {} {}",
                    l.claim().seq,
                    pixels[..16].iter().map(|b| u32::from(*b)).sum::<u32>()
                );
                lease = Some(l);
            }
            "complete" => {
                let l = lease.take().unwrap();
                retry_busy(|| {
                    // SAFETY: this CPU-only fixture submitted no GPU work.
                    unsafe { l.mapping().try_complete_after_gpu(&l) }
                });
                send_message(
                    &stream,
                    &Message::Consumed {
                        key: l.claim().key,
                        consumed: l.claim().seq,
                    },
                    &[],
                )
                .unwrap();
                println!("COMPLETED");
            }
            "progress" => {
                send_message(
                    &stream,
                    &Message::Progress {
                        swapchain_generation: 1,
                        completed_draw: 9007199254740993,
                        monotonic_ns: 9007199254740993,
                    },
                    &[],
                )
                .unwrap();
                println!("PROGRESS");
            }
            "remove" => {
                loop {
                    let (m, _) = receive(&stream);
                    if matches!(m, Message::Remove { .. }) {
                        break;
                    }
                }
                let key = mapping.take().unwrap().header().key;
                send_message(&stream, &Message::Retired { key }, &[]).unwrap();
                let fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
                println!(
                    "CLEAN {} {}",
                    live_mapping_count(),
                    fds as i64 - initial_fds as i64
                );
            }
            "exit" => break,
            _ => panic!("command"),
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {}
