/// True when `argv0` names the Deadlock game executable. Under Proton, argv0 is
/// the Windows path of the game, while /proc/self/exe points at Wine's preloader.
pub fn is_target_cmdline(argv0: &str) -> bool {
    let base = argv0.rsplit(['\\', '/']).next().unwrap_or(argv0);
    base.eq_ignore_ascii_case("deadlock.exe")
}
