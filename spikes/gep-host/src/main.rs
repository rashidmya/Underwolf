//! `uw-gep`: the Linux client of the in-game Deadlock collector (Spike 3).
//!
//!   uw-gep watch --jsonl out.jsonl [--channel N] [--args S] [--no-inject]
//!                [--exe deadlock.exe] [--pid N] [--proton DIR] [--install-dir DIR]
//!
//! Without `--no-inject` it listens for the in-game module on 127.0.0.1:$UW_SPIKE_HOST_PORT
//! (48610), injects `uw_spike_input.dll` into the game and answers the module's `Hello` with
//! `ModuleConfig { input_hooks: false }` and `LoadPlugin { collector, channel, args }`. With
//! `--no-inject` the module is someone else's (osr-host `--capture win32 --gep ""` in the Hideout).
//! Either way it runs `uw-gep-helper.exe <channel>` in the game's Wine session (restarting it if it
//! exits), listens for it on 127.0.0.1:$UW_SPIKE_GEP_PORT (48611), passes what it relays through
//! the provider and writes one JSONL line per emission:
//!   {"t_ms":…,"kind":"info","feature":…,"category":…,"key":…,"value":…,"ts":…}
//!   {"t_ms":…,"kind":"event","name":…,"data":…,"ts":…}
//!   {"t_ms":…,"kind":"status","status":"connected"|"disconnected"|"lost"|"plugin_loaded",…}
//! `t_ms` is Unix time in ms when `uw-gep` received it; `ts` is the collector's own clock (ms).
//! On Ctrl+C it prints the feature-status table. Artifacts come from `--install-dir` (default
//! `~/.local/share/underwolf-spike`, see `spikes/inject/install.sh`).

#[cfg(unix)]
fn main() {
    match app::run() {
        Ok(()) => {}
        Err(msg) => {
            eprintln!("[uw-gep] {msg}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("uw-gep is the Linux client");
    std::process::exit(2);
}

#[cfg(unix)]
mod app {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Child;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};
    use uw_spike_gep_host::provider::{Emission, Provider};
    use uw_spike_gep_host::status::StatusTable;
    use uw_spike_inject::launcher::{GameSession, find_pids};
    use uw_spike_inject::to_wine_path;
    use uw_spike_ipc::{Msg, read_msg, write_msg};

    const MODULE_DLL: &str = "uw_spike_input.dll";
    const COLLECTOR_DLL: &str = "uw_spike_dlcollector.dll";
    const INJECTOR: &str = "uw-spike-inject-win.exe";
    const HELPER: &str = "uw-gep-helper.exe";
    const HELPER_RESTART: Duration = Duration::from_secs(1);

    static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    struct Options {
        jsonl: PathBuf,
        channel: u32,
        args: String,
        inject: bool,
        exe: String,
        pid: Option<i32>,
        proton: Option<PathBuf>,
        install_dir: PathBuf,
    }

    fn usage() -> String {
        "usage: uw-gep watch --jsonl out.jsonl [--channel N] [--args S] [--no-inject] [--exe deadlock.exe] \
         [--pid N] [--proton DIR] [--install-dir DIR]"
            .into()
    }

    fn parse() -> Result<Options, String> {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() != Some("watch") {
            return Err(usage());
        }
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
        let mut o = Options {
            jsonl: PathBuf::new(),
            channel: 1,
            args: String::new(),
            inject: true,
            exe: "deadlock.exe".into(),
            pid: None,
            proton: None,
            install_dir: home.join(".local/share/underwolf-spike"),
        };
        let mut jsonl = None;
        while let Some(flag) = args.next() {
            let mut value = || args.next().ok_or(format!("{flag} needs a value"));
            match flag.as_str() {
                "--jsonl" => jsonl = Some(PathBuf::from(value()?)),
                "--channel" => o.channel = value()?.parse().map_err(|e| format!("--channel: {e}"))?,
                "--args" => o.args = value()?,
                "--no-inject" => o.inject = false,
                "--exe" => o.exe = value()?,
                "--pid" => o.pid = Some(value()?.parse().map_err(|e| format!("--pid: {e}"))?),
                "--proton" => o.proton = Some(value()?.into()),
                "--install-dir" => o.install_dir = value()?.into(),
                _ => return Err(format!("unknown argument {flag}\n{}", usage())),
            }
        }
        o.jsonl = jsonl.ok_or_else(usage)?;
        // The collector's path goes to Wine as a Z: path, which must be absolute.
        o.install_dir =
            std::fs::canonicalize(&o.install_dir).map_err(|e| format!("{}: {e}", o.install_dir.display()))?;
        Ok(o)
    }

    fn port(var: &str, default: u16) -> u16 {
        std::env::var(var).ok().and_then(|p| p.parse().ok()).unwrap_or(default)
    }

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    enum Ev {
        Helper(Msg),
        HelperClosed,
        PluginLoaded { path: String, ok: bool, error: Option<String> },
    }

    pub fn run() -> Result<(), String> {
        let o = parse()?;
        let helper_exe = o.install_dir.join(HELPER);
        let mut needed = vec![helper_exe.clone()];
        if o.inject {
            needed.extend([INJECTOR, MODULE_DLL, COLLECTOR_DLL].map(|f| o.install_dir.join(f)));
        }
        if let Some(missing) = needed.iter().find(|p| !p.exists()) {
            return Err(format!("missing {} (run spikes/inject/install.sh)", missing.display()));
        }
        let session = GameSession::find_with_proton(&o.exe, o.pid, o.proton.clone())?;
        eprintln!(
            "[uw-gep] {} pid {}; prefix {}; wine {}",
            o.exe,
            session.pid,
            session.prefix.display(),
            session.proton.join("bin/wine").display()
        );
        kill_stale_helpers();
        let mut out = BufWriter::new(File::create(&o.jsonl).map_err(|e| format!("{}: {e}", o.jsonl.display()))?);

        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }

        let (tx, rx) = mpsc::channel();
        let gep_port = port("UW_SPIKE_GEP_PORT", 48611);
        let helpers = TcpListener::bind(("127.0.0.1", gep_port)).map_err(|e| format!("port {gep_port}: {e}"))?;
        spawn_accept(helpers, tx.clone(), helper_conn);

        if o.inject {
            let host_port = port("UW_SPIKE_HOST_PORT", 48610);
            let modules =
                TcpListener::bind(("127.0.0.1", host_port)).map_err(|e| format!("port {host_port}: {e}"))?;
            let replies = vec![
                Msg::ModuleConfig { input_hooks: false },
                Msg::LoadPlugin {
                    path: to_wine_path(&o.install_dir.join(COLLECTOR_DLL)),
                    channel: o.channel,
                    args: o.args.clone(),
                },
            ];
            spawn_accept(modules, tx.clone(), move |s, tx| module_conn(s, tx, &replies));
            inject(&session, &o)?;
        }

        let mut helper = Supervisor { session, exe: helper_exe, channel: o.channel, port: gep_port, child: None };
        if !helper.ensure() {
            return Err(format!("{} (pid {}) exited", o.exe, helper.session.pid));
        }
        let result = pump(&rx, &mut out, &mut helper);
        helper.stop();
        let _ = out.flush();
        print!("{}", result?.render());
        Ok(())
    }

    fn spawn_accept<F>(listener: TcpListener, tx: Sender<Ev>, conn: F)
    where
        F: Fn(TcpStream, &Sender<Ev>) + Send + Sync + Clone + 'static,
    {
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, conn) = (tx.clone(), conn.clone());
                std::thread::spawn(move || conn(stream, &tx));
            }
        });
    }

    fn helper_conn(mut s: TcpStream, tx: &Sender<Ev>) {
        while let Ok(msg) = read_msg(&mut s) {
            if tx.send(Ev::Helper(msg)).is_err() {
                return;
            }
        }
        let _ = tx.send(Ev::HelperClosed);
    }

    fn module_conn(mut s: TcpStream, tx: &Sender<Ev>, replies: &[Msg]) {
        while let Ok(msg) = read_msg(&mut s) {
            match msg {
                Msg::Hello { pid, .. } => {
                    eprintln!("[uw-gep] in-game module connected (pid {pid}); loading the collector");
                    if replies.iter().any(|m| write_msg(&mut s, m).is_err()) {
                        return;
                    }
                }
                Msg::PluginLoaded { path, ok, error } => {
                    let _ = tx.send(Ev::PluginLoaded { path, ok, error });
                }
                _ => {}
            }
        }
    }

    fn inject(session: &GameSession, o: &Options) -> Result<(), String> {
        let dll = to_wine_path(&o.install_dir.join(MODULE_DLL));
        let mut cmd = session.wine(&o.install_dir.join(INJECTOR), &[OsString::from(&o.exe), OsString::from(dll)]);
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
        }
        // stdout is for the status table only.
        cmd.stdout(std::io::stderr());
        let status = cmd.status().map_err(|e| format!("running the injector: {e}"))?;
        if !status.success() {
            return Err(format!("injector failed: {status}"));
        }
        Ok(())
    }

    /// A helper left behind by an earlier `uw-gep` would relay into this one alongside ours.
    fn kill_stale_helpers() {
        for pid in find_pids(HELPER) {
            eprintln!("[uw-gep] stopping a leftover {HELPER} (pid {pid})");
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    /// Keeps one helper running in the game's Wine session.
    struct Supervisor {
        session: GameSession,
        exe: PathBuf,
        channel: u32,
        port: u16,
        child: Option<(Child, Instant)>,
    }

    impl Supervisor {
        fn spawn(&self, exe: &Path) -> std::io::Result<Child> {
            let mut cmd = self.session.wine(exe, &[OsString::from(self.channel.to_string())]);
            cmd.env("UW_SPIKE_GEP_PORT", self.port.to_string());
            if std::env::var_os("WINEDEBUG").is_none() {
                cmd.env("WINEDEBUG", "-all");
            }
            // Its own process group: Ctrl+C is ours to handle; we stop the helper ourselves.
            cmd.process_group(0);
            cmd.stdout(std::io::stderr());
            cmd.spawn()
        }

        /// Starts the helper if it is not running (at most once per `HELPER_RESTART`). `false` once
        /// the game is gone: a new helper would start an empty Wine session in the game's prefix.
        fn ensure(&mut self) -> bool {
            if !Path::new(&format!("/proc/{}", self.session.pid)).exists() {
                return false;
            }
            if let Some((child, started)) = &mut self.child {
                match child.try_wait() {
                    Ok(None) => return true,
                    Ok(Some(status)) => {
                        if started.elapsed() < HELPER_RESTART {
                            return true;
                        }
                        eprintln!("[uw-gep] helper exited ({status}); restarting it");
                    }
                    Err(e) => eprintln!("[uw-gep] helper: {e}; restarting it"),
                }
            }
            match self.spawn(&self.exe) {
                Ok(child) => {
                    eprintln!("[uw-gep] started {} (pid {}) on channel {}", HELPER, child.id(), self.channel);
                    self.child = Some((child, Instant::now()));
                }
                Err(e) => eprintln!("[uw-gep] could not start {}: {e}", self.exe.display()),
            }
            true
        }

        fn stop(&mut self) {
            if let Some((mut child, _)) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    struct Sink<'a> {
        out: &'a mut BufWriter<File>,
        provider: Provider,
        status: StatusTable,
        seen_events: HashSet<(String, u64)>,
    }

    impl Sink<'_> {
        fn line(&mut self, mut v: Value) {
            v["t_ms"] = json!(now_ms());
            let text = v.to_string();
            let _ = writeln!(self.out, "{text}");
            let _ = self.out.flush();
        }

        fn status(&mut self, v: Value) {
            eprintln!("[uw-gep] status {v}");
            let mut v = v;
            v["kind"] = json!("status");
            self.line(v);
        }

        fn emit(&mut self, emissions: Vec<Emission>, ts: u64) {
            for e in emissions {
                self.status.observe(&e);
                let v = match &e {
                    Emission::Info { feature, category, key, value } => {
                        eprintln!("[uw-gep] info {category}.{key} = {value:?}");
                        json!({"kind": "info", "feature": feature, "category": category, "key": key, "value": value, "ts": ts})
                    }
                    Emission::Event { name, data } => {
                        eprintln!("[uw-gep] event {name} {data}");
                        json!({"kind": "event", "name": name, "data": data, "ts": ts})
                    }
                };
                self.line(v);
            }
        }

        fn on(&mut self, ev: Ev) {
            match ev {
                Ev::Helper(Msg::GepConnected { channel, pid, version }) => {
                    self.status(json!({"status": "connected", "channel": channel, "pid": pid, "version": version}))
                }
                Ev::Helper(Msg::GepInfo { category, key, value, ts }) => {
                    let e = self.provider.on_info(&category, &key, value.as_deref());
                    self.emit(e, ts);
                }
                Ev::Helper(Msg::GepEvent { name, data, ts }) => {
                    // A reconnecting helper replays the ring; each event counts once per run.
                    if self.seen_events.insert((name.clone(), ts)) {
                        let e = self.provider.on_event(&name, &data);
                        self.emit(e, ts);
                    }
                }
                Ev::Helper(Msg::GepLost { events }) => self.status(json!({"status": "lost", "events": events})),
                Ev::Helper(Msg::GepDisconnected) => self.status(json!({"status": "disconnected"})),
                Ev::Helper(other) => eprintln!("[uw-gep] unexpected message from the helper: {other:?}"),
                Ev::HelperClosed => eprintln!("[uw-gep] helper connection closed"),
                Ev::PluginLoaded { path, ok, error } => {
                    self.status(json!({"status": "plugin_loaded", "path": path, "ok": ok, "error": error}))
                }
            }
        }
    }

    fn pump(rx: &Receiver<Ev>, out: &mut BufWriter<File>, helper: &mut Supervisor) -> Result<StatusTable, String> {
        let mut sink =
            Sink { out, provider: Provider::default(), status: StatusTable::default(), seen_events: HashSet::new() };
        while !STOP.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ev) => sink.on(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("listeners stopped".into()),
            }
            if !helper.ensure() {
                eprintln!("[uw-gep] the game (pid {}) exited", helper.session.pid);
                break;
            }
        }
        Ok(sink.status)
    }
}
