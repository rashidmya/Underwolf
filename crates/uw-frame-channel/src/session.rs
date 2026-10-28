use crate::Result;
use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};
use uw_frame_proto::{FrameError, SessionId};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub uid: u32,
}
impl PeerIdentity {
    pub fn current() -> Result<Self> {
        Self::for_pid(std::process::id())
    }
    pub fn for_pid(pid: u32) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        if pid == 0 {
            return Err(FrameError::Invalid);
        }
        let root = PathBuf::from(format!("/proc/{pid}"));
        let stat = std::fs::read_to_string(root.join("stat")).map_err(|_| FrameError::Stale)?;
        let uid = std::fs::metadata(root)
            .map_err(|_| FrameError::Stale)?
            .uid();
        Ok(Self {
            pid,
            start_ticks: parse_start_ticks(&stat)?,
            uid,
        })
    }
    pub fn from_stream(stream: &UnixStream) -> Result<Self> {
        let cred = rustix::net::sockopt::socket_peercred(stream).map_err(|_| FrameError::Io)?;
        let actual = Self::for_pid(cred.pid.as_raw_nonzero().get() as u32)?;
        if actual.uid != cred.uid.as_raw() {
            return Err(FrameError::Invalid);
        }
        Ok(actual)
    }
}
pub fn parse_start_ticks(stat: &str) -> Result<u64> {
    let (_, tail) = stat.rsplit_once(')').ok_or(FrameError::Invalid)?;
    let ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or(FrameError::Invalid)?
        .parse::<u64>()
        .map_err(|_| FrameError::Invalid)?;
    if ticks == 0 {
        Err(FrameError::Invalid)
    } else {
        Ok(ticks)
    }
}
pub fn session_endpoint(peer: &PeerIdentity) -> Result<PathBuf> {
    let base = std::env::var_os("HOME").ok_or(FrameError::Invalid)?;
    Ok(PathBuf::from(base)
        .join(".cache/underwolf/render")
        .join(format!("{}-{}.sock", peer.pid, peer.start_ticks)))
}
fn shared_clock(peer: PeerIdentity) -> Result<()> {
    let current = std::fs::read_link("/proc/self/ns/time").map_err(|_| FrameError::Unsupported)?;
    let other =
        std::fs::read_link(format!("/proc/{}/ns/time", peer.pid)).map_err(|_| FrameError::Stale)?;
    if current == other {
        Ok(())
    } else {
        Err(FrameError::Unsupported)
    }
}
fn random_session() -> Result<SessionId> {
    let mut bytes = [0; 16];
    let mut at = 0;
    while at < bytes.len() {
        match rustix::rand::getrandom(&mut bytes[at..], rustix::rand::GetRandomFlags::empty()) {
            Ok(0) => return Err(FrameError::Io),
            Ok(n) => at += n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(FrameError::Io),
        }
    }
    if bytes == [0; 16] {
        return random_session();
    }
    Ok(SessionId(bytes))
}

pub struct SessionServer {
    listener: std::os::unix::net::UnixListener,
    endpoint: PathBuf,
    inode: (u64, u64),
    peer: PeerIdentity,
    active: std::sync::Weak<()>,
    _owner: std::fs::File,
}
pub struct ConsumerConnection {
    pub stream: UnixStream,
    pub session: SessionId,
    pub peer: PeerIdentity,
    pub alignment: u64,
    _active: std::sync::Arc<()>,
}
impl SessionServer {
    pub fn bind(peer: PeerIdentity) -> Result<Self> {
        Self::bind_path(&session_endpoint(&peer)?, peer)
    }
    /// An explicit private endpoint also supports isolated native probes and
    /// alternate container-visible paths without changing process environment.
    pub fn bind_path(path: &Path, peer: PeerIdentity) -> Result<Self> {
        use rustix::fs::{FlockOperation, flock};
        use std::{
            fs::{self, OpenOptions},
            os::unix::fs::{
                DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
            },
        };
        let current = PeerIdentity::current()?;
        if peer != PeerIdentity::for_pid(peer.pid)? || peer.uid != current.uid {
            return Err(FrameError::Stale);
        }
        shared_clock(peer)?;
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() >= 108 {
            return Err(FrameError::Invalid);
        }
        let parent = path.parent().ok_or(FrameError::Invalid)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|_| FrameError::Io)?;
        let m = fs::symlink_metadata(parent).map_err(|_| FrameError::Io)?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || m.uid() != current.uid
            || m.mode() & 0o077 != 0
        {
            return Err(FrameError::Invalid);
        }
        let lock_path = path.with_extension("lock");
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(lock_path)
            .map_err(|_| FrameError::Io)?;
        let m = owner.metadata().map_err(|_| FrameError::Io)?;
        if !m.is_file() || m.uid() != current.uid || m.mode() & 0o077 != 0 {
            return Err(FrameError::Invalid);
        }
        flock(&owner, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::WOULDBLOCK {
                FrameError::Busy
            } else {
                FrameError::Io
            }
        })?;
        match fs::symlink_metadata(path) {
            Ok(m) => {
                if !m.file_type().is_socket() || m.uid() != current.uid {
                    return Err(FrameError::Invalid);
                }
                // Only our advisory-lock owner may recover this dedicated stale
                // endpoint; an existing live socket is never unlinked or evicted.
                match UnixStream::connect(path) {
                    Ok(_) => return Err(FrameError::Busy),
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(path).map_err(|_| FrameError::Io)?
                    }
                    Err(_) => return Err(FrameError::Io),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(FrameError::Io),
        }
        let listener = std::os::unix::net::UnixListener::bind(path).map_err(|_| FrameError::Io)?;
        let m = fs::symlink_metadata(path).map_err(|_| FrameError::Io)?;
        let server = Self {
            listener,
            endpoint: path.to_owned(),
            inode: (m.dev(), m.ino()),
            peer,
            active: std::sync::Weak::new(),
            _owner: owner,
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| FrameError::Io)?;
        server
            .listener
            .set_nonblocking(true)
            .map_err(|_| FrameError::Io)?;
        Ok(server)
    }
    pub fn try_accept(&mut self) -> Result<ConsumerConnection> {
        use crate::{recv_message, send_message};
        use uw_frame_proto::{MAX_SIDE, MAX_WIDGETS, Message, SHARED_BUDGET};
        let (stream, _) = self.listener.accept().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                FrameError::Busy
            } else {
                FrameError::Io
            }
        })?;
        if self.active.upgrade().is_some() {
            return Err(FrameError::Busy);
        }
        stream.set_nonblocking(false).map_err(|_| FrameError::Io)?;
        let peer = PeerIdentity::from_stream(&stream)?;
        if peer != self.peer {
            return Err(FrameError::Stale);
        }
        shared_clock(peer)?;
        let (hello, fds) = recv_message(&stream)?;
        if !fds.is_empty() {
            return Err(FrameError::Invalid);
        }
        let Message::Hello {
            pid,
            start_ticks,
            alignment,
            capability,
            ..
        } = hello
        else {
            return Err(FrameError::Invalid);
        };
        if pid != peer.pid || start_ticks != peer.start_ticks {
            return Err(FrameError::Stale);
        }
        if capability != 1 {
            return Err(FrameError::Unsupported);
        }
        let session = random_session()?;
        send_message(
            &stream,
            &Message::Welcome {
                session,
                producer_pid: std::process::id(),
                max_widgets: MAX_WIDGETS,
                max_side: MAX_SIDE,
                shared_budget: SHARED_BUDGET,
            },
            &[],
        )?;
        let active = std::sync::Arc::new(());
        self.active = std::sync::Arc::downgrade(&active);
        Ok(ConsumerConnection {
            stream,
            session,
            peer,
            alignment,
            _active: active,
        })
    }
}
impl Drop for SessionServer {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::symlink_metadata(&self.endpoint)
            && (m.dev(), m.ino()) == self.inode
        {
            let _ = std::fs::remove_file(&self.endpoint);
        }
        // The persistent lock file is not unlinked: a waiter may already have
        // opened it. Closing our final owner FD releases its kernel lock safely.
    }
}
impl Drop for ConsumerConnection {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
