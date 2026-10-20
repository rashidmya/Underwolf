//! A do-nothing Windows process for `tests/wine_e2e.rs` to inject into.

fn main() {
    std::thread::sleep(std::time::Duration::from_secs(300));
}
