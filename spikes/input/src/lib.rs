//! Spike 1 input candidates (Task 7). See `docs/spikes/01-linux-input.md`.
//!
//! - **H1** ([`hotkey_xi2`]) — Ctrl+Tab via a read-only XInput2 connection in the game process.
//! - **C2** ([`capture_wine_x11`], [`got`]) — filter input inside Wine's winex11.so.
//! - **C3** ([`gate`], [`capture_win32_hook`]) — a Win32-side input hook in the game process under
//!   Proton. The game-independent core is [`gate`]; [`capture_win32_hook`] wires it to the Win32
//!   message path, the raw-input path, and the overlay host.

pub mod capture_win32_hook;
pub mod gate;

#[cfg(unix)]
pub mod capture_wine_x11;
#[cfg(unix)]
pub mod got;
#[cfg(unix)]
pub mod hotkey_xi2;
