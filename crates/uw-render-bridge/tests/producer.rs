#![cfg(target_os = "linux")]
use std::{
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uw_frame_channel::*;
use uw_frame_proto::*;
use uw_render_bridge::*;
struct Fixture {
    producer: ProducerSession,
    client: UnixStream,
    root: PathBuf,
    session: SessionId,
}
fn receive(client: &UnixStream) -> (Message, Vec<std::os::fd::OwnedFd>) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recv_message(client) {
            Err(FrameError::Busy) if Instant::now() < until => continue,
            result => return result.unwrap(),
        }
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "uw-producer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let peer = PeerIdentity::current().unwrap();
        let producer = ProducerSession::bind_path(&root.join("render.sock"), peer).unwrap();
        let client = UnixStream::connect(root.join("render.sock")).unwrap();
        send_message(
            &client,
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
        let (Message::Welcome { session, .. }, _) = receive(&client) else {
            panic!("welcome")
        };
        let fixture = Self {
            producer,
            client,
            root,
            session,
        };
        fixture.event(|e| matches!(e,BridgeEvent::Ready{session:s} if *s==session));
        fixture
    }
    fn event(&self, predicate: impl Fn(&BridgeEvent) -> bool) -> BridgeEvent {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            for event in self.producer.drain_events() {
                if predicate(&event) {
                    return event;
                }
            }
            assert!(Instant::now() < until, "bridge event timeout");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn create(&self) -> (AllocationKey, Arc<FrameMapping>) {
        let widget = WidgetKey {
            id: 1,
            generation: 1,
        };
        let key = self.producer.create_widget(widget, 4, 2).unwrap();
        let header = loop {
            let (m, fds) = receive(&self.client);
            if let Message::Register(h) = m {
                break (h, fds);
            }
        };
        assert_eq!(header.0.key, key);
        // SAFETY: this consumer receives native metadata initialized by our producer.
        let mapping = Arc::new(
            unsafe {
                FrameMapping::open(
                    &header.0.encode().unwrap(),
                    header.1.try_into().unwrap(),
                    PeerIdentity::current().unwrap(),
                )
            }
            .unwrap(),
        );
        send_message(&self.client, &Message::Registered { key }, &[]).unwrap();
        (key, mapping)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.producer.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn full() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 4,
        height: 2,
    }
}
#[test]
fn publication_is_immutable_until_consumption_and_a_missing_hint_is_repaired() {
    let f = Fixture::new();
    let (key, m) = f.create();
    let seq = 9007199254740993;
    assert_eq!(key.session, f.session);
    assert_eq!(
        f.producer
            .publish_patch(
                key,
                seq,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &[7; 32]
                }
            )
            .unwrap(),
        PublishResult::Published { seq, bytes: 32 }
    );
    let lease = m.try_claim().unwrap().unwrap();
    assert!(!f.producer.can_publish(key));
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        f.producer
            .publish_patch(
                key,
                seq + 1,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &[9; 32]
                }
            )
            .unwrap(),
        PublishResult::Busy
    );
    assert_eq!(&lease.copy_pixels().unwrap()[..16], &[7; 16]);
    // SAFETY: this real shared-memory consumer did not submit GPU work.
    unsafe { m.try_complete_after_gpu(&lease) }.unwrap();
    // Deliberately send no Consumed hint: authoritative shared metadata must win.
    assert_eq!(
        f.event(|e| matches!(e,BridgeEvent::Consumed{key:k,seq:s} if *k==key && *s==seq)),
        BridgeEvent::Consumed { key, seq }
    );
    assert!(f.producer.can_publish(key));
    f.producer
        .publish_patch(
            key,
            seq + 1,
            Patch {
                rect: Rect {
                    x: 1,
                    y: 1,
                    width: 1,
                    height: 1,
                },
                stride: 4,
                data: &[9; 4],
            },
        )
        .unwrap();
    assert_eq!(f.producer.stats().cpu_bytes, 36);
    assert_eq!(f.producer.stats().publications, 2);
}
#[test]
fn same_size_replacement_retains_budget_until_the_old_allocation_is_retired() {
    let f = Fixture::new();
    let (old, m) = f.create();
    drop(m);
    let (new, _) = f.create();
    assert_ne!(old, new);
    assert!(new.generation > old.generation);
    assert_eq!(f.producer.stats().live_pixel_bytes, 4096);
    assert_eq!(f.producer.stats().retired_pixel_bytes, 4096);
    assert_eq!(
        f.producer
            .publish_patch(
                old,
                1,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &[7; 32]
                }
            )
            .unwrap(),
        PublishResult::Stale
    );
    send_message(&f.client, &Message::Retired { key: old }, &[]).unwrap();
    f.event(|e| matches!(e,BridgeEvent::Retired{key:k} if *k==old));
    assert_eq!(f.producer.stats().retired_pixel_bytes, 0);
}
#[test]
fn control_requests_validate_direction_and_layout_revisions() {
    let f = Fixture::new();
    let (key, _) = f.create();
    let layout = Message::Layout {
        widget: key.widget,
        revision: 1,
        x: 3,
        y: 4,
        width: 4,
        height: 2,
        z: 1,
        visible: true,
    };
    f.producer.control(layout).unwrap();
    assert!(matches!(f.producer.control(layout), Err(FrameError::Stale)));
    assert!(
        f.producer
            .control(Message::Consumed { key, consumed: 1 })
            .is_err()
    );
}
#[test]
fn retained_allocation_and_control_counts_stay_bounded() {
    let f = Fixture::new();
    let mut accepted = 0;
    for _ in 0..300 {
        match f.producer.create_widget(
            WidgetKey {
                id: 1,
                generation: 1,
            },
            4,
            2,
        ) {
            Ok(_) => accepted += 1,
            Err(FrameError::Capacity | FrameError::Unsupported | FrameError::Stale) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert!(accepted <= 128);
    let stats = f.producer.stats();
    assert!(stats.live_pixel_bytes + stats.retired_pixel_bytes <= 128 * 4096);
}
