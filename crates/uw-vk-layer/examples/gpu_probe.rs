#[cfg(target_os = "linux")]
#[path = "../tests/support/gpu.rs"]
mod gpu;
#[cfg(target_os = "linux")]
fn main() {
    let scenario = std::env::args().nth(1).unwrap_or("unorm".into());
    match gpu::run(&scenario) {
        Ok(report) => println!("{report}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    std::process::exit(2)
}

#[cfg(target_os = "linux")]
pub(crate) use gpu::{lifetime, loader_abi, resources};
#[cfg(target_os = "linux")]
pub use uw_vk_layer::{DrawOutcome, submit_if_ready, upload_region};
