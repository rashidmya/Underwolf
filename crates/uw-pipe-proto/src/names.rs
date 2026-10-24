//! Per-game object names. `<pid>` is the game's Windows (Wine) process id in every name (design
//! §3), so the helper (which knows the pid from Toolhelp) and the bootstrap (which reads its own
//! `GetCurrentProcessId`) build the same strings. Events, mutexes and file mappings share one
//! namespace per session (a name taken by another kind fails), so each kind has its own suffix.

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
/// The overlay's own pipe (design 2a: the overlay connects separately from the bootstrap).
pub fn overlay_pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}-overlay")
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

/// The game-data channel's objects (design 2b "Messages and layouts"). The producer in the game
/// creates all of them; the helper only opens them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataObject {
    /// The hello block (file mapping).
    Hello,
    /// The info store (file mapping).
    Info,
    /// The event ring (file mapping).
    Events,
    /// The info store's lock (mutex).
    InfoLock,
    /// The event ring's lock (mutex).
    EventsLock,
    /// Rung when info changed (auto-reset event).
    InfoBell,
    /// Rung when events were written (auto-reset event).
    EventsBell,
    /// Set once the producer is ready (manual-reset event).
    Connect,
    /// Set when the producer goes away (manual-reset event).
    Disconnect,
}

impl DataObject {
    pub const ALL: [DataObject; 9] = [
        DataObject::Hello,
        DataObject::Info,
        DataObject::Events,
        DataObject::InfoLock,
        DataObject::EventsLock,
        DataObject::InfoBell,
        DataObject::EventsBell,
        DataObject::Connect,
        DataObject::Disconnect,
    ];

    /// The name suffix: one per object, since every kind shares one namespace.
    pub fn suffix(self) -> &'static str {
        match self {
            DataObject::Hello => "hello",
            DataObject::Info => "info",
            DataObject::Events => "events",
            DataObject::InfoLock => "info-lock",
            DataObject::EventsLock => "events-lock",
            DataObject::InfoBell => "info-bell",
            DataObject::EventsBell => "events-bell",
            DataObject::Connect => "connect",
            DataObject::Disconnect => "disconnect",
        }
    }
}

/// `Local\underwolf-<pid>-data-<channel>-<suffix>` (design 2b): the pid keeps two games in one
/// Wine session apart, the channel id lets a later plan add more channels.
pub fn data_name(pid: u32, channel: u32, object: DataObject) -> String {
    format!(r"Local\underwolf-{pid}-data-{channel}-{}", object.suffix())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(overlay_pipe_name(1234), r"\\.\pipe\underwolf-1234-overlay");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
        assert_eq!(bell_game_name(1234), r"Local\underwolf-1234-bell-game");
        assert_eq!(bell_host_name(1234), r"Local\underwolf-1234-bell-host");
        assert_eq!(bells_name(1234), r"Local\underwolf-1234-bells");
        assert_eq!(state_name(1234), r"Local\underwolf-1234-state");
        assert_eq!(state_lock_name(1234), r"Local\underwolf-1234-state-lock");
    }

    /// Catches a data object named off the design's table (`Local\underwolf-<pid>-data-<channel>-
    /// <suffix>`), which the producer and the helper would then disagree on.
    #[test]
    fn data_names_embed_the_pid_the_channel_and_the_design_suffix() {
        let names: Vec<String> = DataObject::ALL
            .iter()
            .map(|&o| data_name(1234, 7, o))
            .collect();
        assert_eq!(
            names,
            [
                r"Local\underwolf-1234-data-7-hello",
                r"Local\underwolf-1234-data-7-info",
                r"Local\underwolf-1234-data-7-events",
                r"Local\underwolf-1234-data-7-info-lock",
                r"Local\underwolf-1234-data-7-events-lock",
                r"Local\underwolf-1234-data-7-info-bell",
                r"Local\underwolf-1234-data-7-events-bell",
                r"Local\underwolf-1234-data-7-connect",
                r"Local\underwolf-1234-data-7-disconnect",
            ]
        );
    }

    /// Catches two kinds of kernel object sharing a name: the second `Create*W` would then fail
    /// with `ERROR_INVALID_HANDLE` (design "Platform facts"). The data objects of channel 1 are
    /// in the list too, so they cannot clash with the control channel's.
    #[test]
    fn kernel_object_names_are_all_different() {
        let mut names = vec![
            helper_mutex_name(7),
            marker_mutex_name(7),
            bell_game_name(7),
            bell_host_name(7),
            bells_name(7),
            state_name(7),
            state_lock_name(7),
        ];
        names.extend(DataObject::ALL.iter().map(|&o| data_name(7, 1, o)));
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
