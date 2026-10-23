//! Per-game object names. `<pid>` is the game's Windows (Wine) process id in every name (design
//! §3), so the helper (which knows the pid from Toolhelp) and the bootstrap (which reads its own
//! `GetCurrentProcessId`) build the same strings. Events, mutexes and file mappings share one
//! namespace per session (a name taken by another kind fails), so each kind has its own suffix.

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
pub fn helper_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-helper-{pid}")
}
pub fn marker_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-bootstrap-{pid}")
}
/// Host -> game doorbell (auto-reset event).
pub fn bell_game_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bell-game")
}
/// Game -> host doorbell (auto-reset event).
pub fn bell_host_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bell-host")
}
/// The 16-byte doorbell mask region (file mapping).
pub fn bells_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bells")
}
/// The state block (file mapping).
pub fn state_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-state")
}
/// The state block's lock (mutex).
pub fn state_lock_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-state-lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
        assert_eq!(bell_game_name(1234), r"Local\underwolf-1234-bell-game");
        assert_eq!(bell_host_name(1234), r"Local\underwolf-1234-bell-host");
        assert_eq!(bells_name(1234), r"Local\underwolf-1234-bells");
        assert_eq!(state_name(1234), r"Local\underwolf-1234-state");
        assert_eq!(state_lock_name(1234), r"Local\underwolf-1234-state-lock");
    }

    /// Catches two kinds of kernel object sharing a name: the second `Create*W` would then fail
    /// with `ERROR_INVALID_HANDLE` (design "Platform facts").
    #[test]
    fn kernel_object_names_are_all_different() {
        let names = [
            helper_mutex_name(7),
            marker_mutex_name(7),
            bell_game_name(7),
            bell_host_name(7),
            bells_name(7),
            state_name(7),
            state_lock_name(7),
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
