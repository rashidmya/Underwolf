#[cfg(target_os = "linux")]
#[path = "../tests/support/actor.rs"]
mod actor;
#[cfg(target_os = "linux")]
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let arg = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
            .expect("--role producer|consumer --scenario <name> --socket <path>")
    };
    actor::run(
        &arg("--role"),
        &arg("--scenario"),
        std::path::Path::new(&arg("--socket")),
    )
    .unwrap();
}
#[cfg(not(target_os = "linux"))]
fn main() {}
