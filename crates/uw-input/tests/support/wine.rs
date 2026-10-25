#![cfg(unix)]
use std::{path::PathBuf, process::Command};
pub fn probe(case: &str) {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release/examples/input_probe.exe")
        .canonicalize()
        .expect("build input_probe first");
    let prefix = std::env::temp_dir().join(format!("uw-routing-{}-{case}", std::process::id()));
    std::fs::create_dir_all(&prefix).unwrap();
    let result = Command::new("timeout")
        .args(["60", "wine"])
        .arg(binary)
        .args(["--routing", case])
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .output()
        .unwrap();
    let _ = Command::new("wineserver").arg("-k").env("WINEPREFIX", &prefix).status();
    let _ = std::fs::remove_dir_all(prefix);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("PASS routing probe"));
}
