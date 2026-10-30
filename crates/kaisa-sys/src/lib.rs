//! The only Kaisa crate allowed to contain `unsafe`: PTYs, process handles, job objects,
//! sandboxes.
//!
//! Design: `docs/architecture.md` §5.1, §8 and §13.

#[cfg(windows)]
#[expect(
    dead_code,
    reason = "used once agents start in a pseudo console (T12.6c)"
)]
mod conpty;
#[cfg(windows)]
mod job;
#[cfg(unix)]
mod openpty;
#[cfg(target_os = "linux")]
mod pidfd;
mod process;
mod pty;
#[cfg(unix)]
mod terminal;
#[cfg(any(windows, test))]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used once agents start in a pseudo console (T12.6c)"
    )
)]
mod windows_command_line;

pub use process::{OwnedProcess, ProcessExit, ProcessSpec};
pub use pty::{InvalidTerminalSize, PseudoTerminal, TerminalSize};
#[cfg(unix)]
pub use terminal::Terminal;
