use std::{
    io::{self, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use uw_frame_channel::*;
use uw_frame_proto::*;
fn receive(stream: &UnixStream) -> uw_frame_channel::Result<(Message, Vec<std::os::fd::OwnedFd>)> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recv_message(stream) {
            Err(FrameError::Busy) if Instant::now() < until => continue,
            result => return result,
        }
    }
}
fn marker(text: &str) {
    println!("ACTOR_{text}");
    io::stdout().flush().unwrap();
}
fn hold(mapping: &FrameMapping, partial_write: bool) {
    // SAFETY: this isolated actor uses our own correctly initialized mutex and
    // holds all mapping references. The parent never accesses bytes outside the
    // same protocol; termination deliberately exercises kernel robust recovery.
    let fds = unsafe { mapping.descriptors() };
    // SAFETY: control is a size-sealed 4096-byte file initialized by the channel.
    let control = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fds[0].as_raw_fd(),
            0,
        )
    };
    assert_ne!(control, libc::MAP_FAILED);
    // SAFETY: the native mutex is initialized, aligned and inside our live mapping.
    let mutex = unsafe {
        (control as *mut u8)
            .add(128)
            .cast::<libc::pthread_mutex_t>()
    };
    // SAFETY: this actor owns a live initialized process-shared mutex instance.
    assert_eq!(unsafe { libc::pthread_mutex_lock(mutex) }, 0);
    let mut pixels = std::ptr::null_mut();
    if partial_write {
        // SAFETY: the retained pixel descriptor covers the validated mapping length.
        pixels = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping.header().layout.pixel_len as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fds[1].as_raw_fd(),
                0,
            )
        };
        assert_ne!(pixels, libc::MAP_FAILED);
        // SAFETY: mutex ownership excludes every other writer/reader; write only
        // half the first 16-byte row before the parent terminates this actor.
        unsafe { std::ptr::write_bytes(pixels, 99, 8) };
    }
    marker("LOCKED");
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    // SAFETY: this thread still owns the mutex and both mappings remain live.
    unsafe {
        libc::pthread_mutex_unlock(mutex);
        if !pixels.is_null() {
            libc::munmap(pixels, mapping.header().layout.pixel_len as usize);
        }
        libc::munmap(control, 4096);
    }
}
pub fn run(
    role: &str,
    scenario: &str,
    path: &Path,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // SAFETY: restricts this test actor's own lifecycle to its parent; no pointers
    // or other process state are modified by PR_SET_PDEATHSIG.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let stream = UnixStream::connect(path)?;
    let peer = PeerIdentity::from_stream(&stream)?;
    let before = live_mapping_count();
    if role == "producer" {
        let key = AllocationKey {
            session: SessionId([9; 16]),
            widget: WidgetKey {
                id: 1,
                generation: 1,
            },
            generation: 1,
        };
        let mapping = FrameMapping::create(key, FrameLayout::new(4, 2, 4096)?, peer)?;
        mapping.try_publish(
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
        )?;
        // SAFETY: the trusted parent receiver uses the same channel implementation.
        send_message(&stream, &Message::Register(mapping.header()), &unsafe {
            mapping.descriptors()
        })?;
        hold(&mapping, true);
    } else {
        let rounds = if scenario == "churn" { 100 } else { 1 };
        let fds_before = std::fs::read_dir("/proc/self/fd")?.count();
        for _ in 0..rounds {
            let (message, fds) = receive(&stream)?;
            let Message::Register(header) = message else {
                return Err("expected registration".into());
            };
            // SAFETY: descriptors originate from our trusted parent producer,
            // which initializes the mutex and honors the publication contract.
            let mapping = Arc::new(unsafe {
                FrameMapping::open(
                    &header.encode()?,
                    fds.try_into().map_err(|_| "two descriptors required")?,
                    peer,
                )
            }?);
            if scenario == "hold-mutex" {
                hold(&mapping, false);
                continue;
            }
            let lease = mapping.try_claim()?.ok_or("missing ready pixels")?;
            if scenario != "churn" {
                marker("READING");
                let mut line = String::new();
                io::stdin().read_line(&mut line)?;
            }
            let pixels = lease.copy_pixels()?;
            assert_eq!(&pixels[..16], &[7; 16]);
            assert_eq!(&pixels[64..80], &[7; 16]);
            // SAFETY: this CPU-only ownership actor has submitted no GPU read.
            let completed = unsafe { mapping.try_complete_after_gpu(&lease) }?;
            assert_eq!(completed, CompleteResult::Completed);
            send_message(
                &stream,
                &Message::Consumed {
                    key: header.key,
                    consumed: lease.claim().seq,
                },
                &[],
            )?;
        }
        assert_eq!(live_mapping_count(), before);
        assert_eq!(std::fs::read_dir("/proc/self/fd")?.count(), fds_before);
    }
    marker("DONE");
    if scenario == "churn" {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
    }
    Ok(())
}
