//! Per-game object names. `<pid>` is the game's Windows (Wine) process id in every name (design
//! §3), so the helper (which knows the pid from Toolhelp) and the bootstrap (which reads its own
//! `GetCurrentProcessId`) build the same strings.

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
pub fn helper_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-helper-{pid}")
}
pub fn marker_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-bootstrap-{pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
    }
}
