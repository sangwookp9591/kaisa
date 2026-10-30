//! The only Kaisa crate allowed to contain `unsafe`: PTYs, process handles, job objects,
//! sandboxes.
//!
//! Design: `docs/architecture.md` §5.1, §8 and §13.

#[cfg(windows)]
mod conpty;
#[cfg(windows)]
mod console_process;
#[cfg(windows)]
mod job;
#[cfg(unix)]
mod openpty;
#[cfg(target_os = "linux")]
mod pidfd;
mod process;
mod pty;
mod terminal;
#[cfg(windows)]
mod windows_child;
#[cfg(any(windows, test))]
mod windows_command_line;

pub use process::{OwnedProcess, ProcessExit, ProcessSpec};
pub use pty::{InvalidTerminalSize, PseudoTerminal, TerminalSize};
pub use terminal::Terminal;
