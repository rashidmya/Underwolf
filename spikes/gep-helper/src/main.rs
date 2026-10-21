//! Wine-side GEP helper: `uw-gep-helper <channel>`, run inside the game's Wine session by `uw-gep`.
//!
//! Connects to `uw-gep` on `127.0.0.1:$UW_SPIKE_GEP_PORT` (default 48611), retrying every second.
//! Waits for the collector to create data channel `<channel>`, attaches, sends `GepConnected` and
//! the full info snapshot plus every event still in the ring, then relays changes and events as
//! the collector signals them. When the collector goes away it sends `GepDisconnected` and waits
//! for the channel again. After a lost connection it reconnects and starts over with a fresh
//! snapshot, so a restarted `uw-gep` (or helper) always gets the whole state.

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-gep-helper runs inside Wine: build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::net::TcpStream;
    use std::time::Duration;

    use uw_spike_gep_channel::win::{Consumer, Wake};
    use uw_spike_gep_channel::{InfoReader, RingReader};
    use uw_spike_gep_helper::{relay, relay_events};
    use uw_spike_ipc::{Msg, write_msg};

    const DEFAULT_PORT: u16 = 48611;
    const RETRY: Duration = Duration::from_secs(1);
    /// How often a missing channel is looked for again.
    const PRODUCER_POLL: Duration = Duration::from_millis(250);
    /// Longest wait for a signal; on a timeout the helper polls anyway and checks the connection.
    const WAIT_MS: u32 = 1000;

    pub fn main() {
        let Some(channel) = std::env::args().nth(1).and_then(|c| c.parse::<u32>().ok()) else {
            eprintln!("usage: uw-gep-helper <channel>");
            std::process::exit(2);
        };
        let port = std::env::var("UW_SPIKE_GEP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
        loop {
            let mut stream = connect(port);
            eprintln!("[uw-gep-helper] connected to uw-gep on port {port}; channel {channel}");
            let err = serve(&mut stream, channel);
            eprintln!("[uw-gep-helper] connection to uw-gep lost ({err}); reconnecting");
            std::thread::sleep(RETRY);
        }
    }

    fn connect(port: u16) -> TcpStream {
        loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.set_nodelay(true);
                return s;
            }
            std::thread::sleep(RETRY);
        }
    }

    fn send(stream: &mut TcpStream, msgs: impl IntoIterator<Item = Msg>) -> io::Result<()> {
        msgs.into_iter().try_for_each(|m| write_msg(stream, &m))
    }

    /// `uw-gep` never writes to us, so readable-with-zero-bytes means it closed the connection.
    fn peer_alive(stream: &TcpStream) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        let mut b = [0u8; 1];
        let r = stream.peek(&mut b);
        stream.set_nonblocking(false)?;
        match r {
            Ok(0) => Err(io::Error::new(io::ErrorKind::ConnectionAborted, "uw-gep closed the connection")),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Relays producer after producer until the TCP connection fails.
    fn serve(stream: &mut TcpStream, channel: u32) -> io::Error {
        loop {
            let consumer = match wait_for_producer(stream, channel) {
                Ok(c) => c,
                Err(e) => return e,
            };
            if let Err(e) = relay_producer(stream, channel, consumer) {
                return e;
            }
        }
    }

    fn wait_for_producer(stream: &TcpStream, channel: u32) -> io::Result<Consumer> {
        let mut last = String::new();
        loop {
            match Consumer::open(channel) {
                Ok(c) => return Ok(c),
                Err(e) => {
                    let why = e.to_string();
                    if why != last {
                        eprintln!("[uw-gep-helper] waiting for channel {channel}: {why}");
                        last = why;
                    }
                }
            }
            peer_alive(stream)?;
            std::thread::sleep(PRODUCER_POLL);
        }
    }

    /// Attaches and relays until the producer disconnects (`Ok`) or the TCP connection fails.
    /// The consumer is dropped on return, so a restarted producer can create the channel again.
    fn relay_producer(stream: &mut TcpStream, channel: u32, consumer: Consumer) -> io::Result<()> {
        let (version, pid) = consumer.hello();
        // Attach and snapshot under one lock, so later `changed` batches continue exactly from it.
        let (mut info, snapshot) = loop {
            match consumer.with_info(|r| InfoReader::attach(r).map(|rd| {
                let snapshot = rd.snapshot(r);
                (rd, snapshot)
            })) {
                Some(Ok(attached)) => break attached,
                Some(Err(e)) => {
                    eprintln!("[uw-gep-helper] channel {channel}: info store unreadable ({e})");
                    drop(consumer);
                    std::thread::sleep(RETRY);
                    return Ok(());
                }
                // The collector held the mutex for 2 s; try again unless it went away.
                None if consumer.wait(0) == Wake::Disconnect => return send(stream, [Msg::GepDisconnected]),
                None => peer_alive(stream)?,
            }
        };
        eprintln!("[uw-gep-helper] attached to channel {channel} (pid {pid}, format {version})");
        send(stream, [Msg::GepConnected { channel, pid, version }])?;
        send(stream, relay(snapshot))?;
        // Replays the events still retained (e.g. ones pushed before this helper attached).
        let mut ring = RingReader::new();
        poll_events(stream, &consumer, &mut ring)?;
        loop {
            match consumer.wait(WAIT_MS) {
                Wake::Disconnect => {
                    drop(consumer);
                    eprintln!("[uw-gep-helper] channel {channel} disconnected");
                    return send(stream, [Msg::GepDisconnected]);
                }
                Wake::Info => poll_info(stream, &consumer, &mut info)?,
                Wake::Events => poll_events(stream, &consumer, &mut ring)?,
                // A signal can be missed when a lock attempt timed out: poll both anyway.
                Wake::Timeout => {
                    peer_alive(stream)?;
                    poll_info(stream, &consumer, &mut info)?;
                    poll_events(stream, &consumer, &mut ring)?;
                }
            }
        }
    }

    fn poll_info(stream: &mut TcpStream, consumer: &Consumer, info: &mut InfoReader) -> io::Result<()> {
        match consumer.with_info(|r| info.changed(r)) {
            Some(changes) => send(stream, relay(changes)),
            None => Ok(()),
        }
    }

    fn poll_events(stream: &mut TcpStream, consumer: &Consumer, ring: &mut RingReader) -> io::Result<()> {
        match consumer.with_events(|r| ring.drain(r)) {
            Some((records, lost)) => send(stream, relay_events(records, lost)),
            None => Ok(()),
        }
    }
}
