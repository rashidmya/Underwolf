//! Data gate probe, consumer side (design 2b "Testing" 2). `data_consumer_probe <producer windows
//! pid>`, run under Wine in the prefix where `data_producer_probe` runs. It runs one command per
//! stdin line, as the helper's data thread would, and answers each with one line:
//! - `open`: `Consumer::open` every 100 ms until it opens (`opened`; 30 s at most);
//! - `open-once`: `Consumer::open` once: `opened`, or `not ready` (`connect` missing or not set);
//! - `attach [<producer_id>:<seq>]`: `attached producer_id=<id> from_seq=<n> schema=<c.k/max,…>
//!   snapshot=<c.k=value,…>` (null values print as `null`);
//! - `wait`: `wake disconnect|info|events|timeout` (a 5 s wait);
//! - `changes`: `changes <c.k=value> …`;
//! - `drain`: `drain lost=<n> <seq>:<name>:<data> …`;
//! - `close`: drop the consumer (`closed`).
//!
//! A read that fails answers `busy <ms>` (the lock was not acquired, after that many ms) or
//! `error <code> <reason>`. It exits when stdin closes.

#[cfg(not(windows))]
fn main() {
    eprintln!("data_consumer_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::{BufRead, Write};
    use std::time::{Duration, Instant};

    use uw_game_channel::data::{ConsumeError, Consumer, OpenError, Wake};
    use uw_pipe_proto::data::{InfoValue, ResumePoint};

    const CHANNEL: u32 = 1;

    fn say(s: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    }

    fn values(vs: &[InfoValue]) -> Vec<String> {
        vs.iter()
            .map(|v| {
                format!(
                    "{}.{}={}",
                    v.category,
                    v.key,
                    v.value.as_deref().unwrap_or("null")
                )
            })
            .collect()
    }

    fn failed(e: ConsumeError, started: Instant) -> String {
        match e {
            ConsumeError::Busy => format!("busy {}", started.elapsed().as_millis()),
            ConsumeError::Read(e) => format!("error {} {e}", e.code()),
        }
    }

    fn command(line: &str, pid: u32, consumer: &mut Option<Consumer>) -> String {
        let mut words = line.split_whitespace();
        let word = words.next().unwrap_or("");
        if word == "open" {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(30) {
                if let Ok(c) = Consumer::open(pid, CHANNEL) {
                    *consumer = Some(c);
                    return "opened".into();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return "error open timed out".into();
        }
        if word == "open-once" {
            return match Consumer::open(pid, CHANNEL) {
                Ok(c) => {
                    *consumer = Some(c);
                    "opened".into()
                }
                Err(OpenError::NotReady) => "not ready".into(),
                Err(e) => format!("error {e}"),
            };
        }
        let Some(c) = consumer.as_mut() else {
            return "error not open".into();
        };
        let started = Instant::now();
        match word {
            "attach" => {
                let resume = words.next().and_then(|w| w.parse::<ResumePoint>().ok());
                match c.attach(resume) {
                    Ok(a) => {
                        let schema: Vec<String> = a
                            .schema
                            .keys()
                            .map(|(c, k, max)| format!("{c}.{k}/{max}"))
                            .collect();
                        format!(
                            "attached producer_id={} from_seq={} schema={} snapshot={}",
                            a.producer_id,
                            a.from_seq,
                            schema.join(","),
                            values(&a.snapshot).join(",")
                        )
                    }
                    Err(e) => failed(e, started),
                }
            }
            "wait" => match c.wait(5000) {
                Wake::Disconnect => "wake disconnect".into(),
                Wake::Info => "wake info".into(),
                Wake::Events => "wake events".into(),
                Wake::Timeout => "wake timeout".into(),
            },
            "changes" => match c.changes() {
                Ok(vs) => ["changes".to_string()]
                    .into_iter()
                    .chain(values(&vs))
                    .collect::<Vec<_>>()
                    .join(" "),
                Err(e) => failed(e, started),
            },
            "drain" => match c.drain() {
                Ok(d) => {
                    let mut out = format!("drain lost={}", d.lost);
                    for e in &d.events {
                        out.push_str(&format!(" {}:{}:{}", e.seq, e.name, e.data));
                    }
                    out
                }
                Err(e) => failed(e, started),
            },
            "close" => {
                *consumer = None;
                "closed".into()
            }
            other => format!("error unknown command {other:?}"),
        }
    }

    pub fn main() -> i32 {
        let Some(pid) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
            eprintln!("usage: data_consumer_probe <producer windows pid>");
            return 2;
        };
        let mut consumer = None;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            say(&command(&line, pid, &mut consumer));
        }
        0
    }
}
