//! The only Kaisa crate allowed to contain `unsafe`: PTYs, process handles, job objects,
//! sandboxes.
//!
//! Design: `docs/architecture.md` §5.1, §8 and §13.

mod capture;
#[cfg(windows)]
mod conpty;
#[cfg(windows)]
mod console_process;
#[cfg(windows)]
mod control_pipe;
mod identity;
#[cfg(windows)]
mod job;
#[cfg(target_os = "linux")]
mod landlock;
#[cfg(unix)]
mod openpty;
#[cfg(unix)]
mod peer;
#[cfg(target_os = "linux")]
mod pidfd;
mod process;
mod pty;
mod random;
mod seatbelt;
mod state_dir;
mod terminal;
#[cfg(windows)]
mod windows_child;
#[cfg(any(windows, test))]
mod windows_command_line;

pub use capture::{CapturedRun, Confinement, run_captured};
#[cfg(windows)]
pub use control_pipe::ControlPipe;
pub use identity::ProcessIdentity;
#[cfg(target_os = "linux")]
pub use landlock::{LandlockRuleset, write_ruleset};
#[cfg(unix)]
pub use peer::{ForeignPeer, current_uid, peer_uid, require_same_user};
pub use process::{OwnedProcess, ProcessExit, ProcessSpec};
pub use pty::{InvalidTerminalSize, PseudoTerminal, TerminalSize};
pub use random::fill_random;
pub use seatbelt::{SeatbeltPolicy, sandboxed_spec, seatbelt_profile};
pub use state_dir::{
    CONTROL_SOCKET_NAME, ControlEndpoint, STATE_DIR_OVERRIDE, StateLocation,
    ensure_private_directory, state_location,
};
#[cfg(unix)]
pub use state_dir::{PRIVATE_FILE_MODE, PRIVATE_MODE};
pub use terminal::Terminal;
