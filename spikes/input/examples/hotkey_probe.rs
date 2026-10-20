//! Prints a line for every Ctrl+Tab seen through XInput2 raw events (candidate H1).
//!   cargo run -p uw-spike-input --example hotkey_probe
fn main() {
    let h = uw_spike_input::hotkey_xi2::spawn(|| println!("HOTKEY Ctrl+Tab")).unwrap();
    h.join().unwrap();
}
