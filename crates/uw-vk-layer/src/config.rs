use std::sync::LazyLock;
pub static IS_TARGET: LazyLock<bool> = LazyLock::new(|| {
    if std::env::var("UW_LAYER_ENABLE").as_deref() != Ok("1")
        || std::env::var("UW_LAYER_DISABLE").as_deref() == Ok("1")
    {
        return false;
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    crate::target_process(&exe, std::env::var("UW_RENDER_PROBE").as_deref() == Ok("1"))
});
pub fn monotonic_ns() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid timespec and supported monotonic clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) } != 0 {
        return 0;
    }
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
