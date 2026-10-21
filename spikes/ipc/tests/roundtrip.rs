use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;

use uw_spike_ipc::{Msg, Plane, fourcc_for, parse_modifier, recv, send};

fn all_variants() -> Vec<Msg> {
    vec![
        Msg::Hello { protocol: 1, pid: 4242 },
        Msg::Swapchain { width: 2560, height: 1440 },
        Msg::Frame {
            id: u64::MAX - 1,
            widget: 3,
            x: -10,
            y: 20,
            width: 400,
            height: 300,
            drm_fourcc: 0x3432_5241,
            modifier: 0x00ff_ffff_ffff_ffff,
            planes: vec![Plane { offset: 0, stride: 1792 }, Plane { offset: 64, stride: 896 }],
        },
        Msg::Ack { id: 7 },
        Msg::Layout { widget: 3, x: 5, y: 6, visible: false },
        Msg::Slot { widget: 2, slot: 1, width: 400, height: 300, stride: 1600, drm_fourcc: 0x3432_5241 },
        Msg::FrameShm { id: 99, widget: 2, slot: 1, x: 100, y: 450 },
        Msg::ModuleConfig { input_hooks: true },
        Msg::ModuleConfig { input_hooks: false },
        Msg::LoadPlugin {
            path: r"Z:\home\u\.local\share\underwolf-spike\uw_spike_dlcollector.dll".into(),
            channel: 1,
            args: "discover=10 corrupt=C_Team.m_iTeamNum".into(),
        },
        Msg::PluginLoaded { path: r"Z:\p\c.dll".into(), ok: true, error: None },
        Msg::PluginLoaded { path: r"Z:\p\c.dll".into(), ok: false, error: Some("LoadLibraryW failed: 126".into()) },
    ]
}

#[test]
fn tcp_framing_roundtrip() {
    let mut buf = Vec::new();
    for m in all_variants() {
        uw_spike_ipc::write_msg(&mut buf, &m).unwrap();
    }
    let mut r = &buf[..];
    for m in all_variants() {
        assert_eq!(uw_spike_ipc::read_msg(&mut r).unwrap(), m);
    }
    assert!(r.is_empty());
}

#[test]
fn msg_roundtrip() {
    let (a, b) = UnixStream::pair().unwrap();
    for m in all_variants() {
        send(&a, &m, &[]).unwrap();
        let (got, fds) = recv(&b).unwrap();
        assert_eq!(got, m);
        assert!(fds.is_empty());
    }
}

#[test]
fn messages_do_not_merge() {
    let (a, b) = UnixStream::pair().unwrap();
    let msgs = all_variants();
    for m in &msgs {
        send(&a, m, &[]).unwrap();
    }
    for m in &msgs {
        assert_eq!(&recv(&b).unwrap().0, m);
    }
}

#[test]
fn fds_cross_socket() {
    let dir = std::env::temp_dir().join(format!("uw-ipc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = std::fs::File::create(dir.join("one")).unwrap();
    let f2 = std::fs::File::create(dir.join("two")).unwrap();
    let (a, b) = UnixStream::pair().unwrap();
    let m = Msg::Ack { id: 1 };
    send(&a, &m, &[f1.as_fd(), f2.as_fd()]).unwrap();
    send(&a, &Msg::Ack { id: 2 }, &[]).unwrap();

    let (got, fds) = recv(&b).unwrap();
    assert_eq!(got, m);
    assert_eq!(fds.len(), 2);
    let ino = |fd: &std::os::fd::OwnedFd| std::fs::File::from(fd.try_clone().unwrap()).metadata().unwrap().ino();
    assert_eq!(ino(&fds[0]), f1.metadata().unwrap().ino());
    assert_eq!(ino(&fds[1]), f2.metadata().unwrap().ino());

    let (next, fds) = recv(&b).unwrap();
    assert_eq!(next, Msg::Ack { id: 2 });
    assert!(fds.is_empty());
}

#[test]
fn recv_reports_eof() {
    let (a, b) = UnixStream::pair().unwrap();
    drop(a);
    assert_eq!(recv(&b).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn fourcc_mapping() {
    assert_eq!(fourcc_for("bgra"), Some(0x3432_5241));
    assert_eq!(fourcc_for("rgba"), Some(0x3432_4241));
    assert_eq!(fourcc_for("rgbaf16"), None);
}

#[test]
fn modifier_parsing() {
    assert_eq!(parse_modifier("72057594037927935"), Some(0x00ff_ffff_ffff_ffff));
    assert_eq!(parse_modifier("0"), Some(0));
    assert_eq!(parse_modifier("abc"), None);
}
