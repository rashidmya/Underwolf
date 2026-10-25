#[cfg(not(windows))]
fn main() {
    eprintln!("native_input_probe requires Windows/Wine");
    std::process::exit(2);
}

#[cfg(windows)]
#[path = "native_input_probe/directinput.rs"]
mod directinput;
#[cfg(windows)]
#[path = "native_input_probe/discovery.rs"]
mod discovery;
#[cfg(windows)]
#[path = "native_input_probe/gameinput.rs"]
mod gameinput;

#[cfg(windows)]
fn main() {
    discovery::run();
}
