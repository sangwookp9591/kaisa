//! Unix pseudo-terminal pairs through `openpty` (macOS and Linux).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::null_mut;

use crate::pty::TerminalSize;

/// Both ends of a new pseudo-terminal. Kaisa keeps the master; the agent gets the slave as
/// its standard input, output and error.
#[derive(Debug)]
pub(crate) struct TerminalPair {
    /// The end Kaisa reads the agent's output from and writes its input to.
    pub(crate) master: OwnedFd,
    /// The end the agent sees as its terminal.
    pub(crate) slave: OwnedFd,
}

/// Opens a pseudo-terminal of `size`. Both descriptors are close-on-exec, so no agent
/// inherits a terminal it was not given.
pub(crate) fn open(size: TerminalSize) -> io::Result<TerminalPair> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut window: libc::winsize = window_of(size);
    #[allow(unsafe_code)]
    // SAFETY: both descriptor pointers and the window pointer refer to live locals; a null
    // name and null terminal settings are documented as "not wanted" / "defaults".
    let result: libc::c_int = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            null_mut(),
            null_mut(),
            &raw mut window,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: `openpty` succeeded, so both are fresh descriptors that nothing else owns.
    let pair: TerminalPair = unsafe {
        TerminalPair {
            master: OwnedFd::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        }
    };
    close_on_exec(pair.master.as_raw_fd())?;
    close_on_exec(pair.slave.as_raw_fd())?;
    Ok(pair)
}

fn window_of(size: TerminalSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows(),
        ws_col: size.columns(),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn close_on_exec(fd: RawFd) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: `fcntl` with F_SETFD on a descriptor we own touches no caller memory.
    let result: libc::c_int = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Changes the size of the terminal behind `fd`; the kernel tells the agent with SIGWINCH.
pub(crate) fn set_size(fd: &OwnedFd, size: TerminalSize) -> io::Result<()> {
    let window: libc::winsize = window_of(size);
    #[allow(unsafe_code)]
    // SAFETY: TIOCSWINSZ reads one `winsize` through the pointer, which refers to a live,
    // correctly typed local.
    let result: libc::c_int =
        unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &raw const window) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Makes reads of `fd` return `WouldBlock` instead of waiting.
pub(crate) fn set_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: F_GETFL on a descriptor the caller owns touches no caller memory.
    let flags: libc::c_int = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: F_SETFL with an integer flag set touches no caller memory.
    let result: libc::c_int =
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The size the kernel reports for the terminal behind `fd`.
#[cfg(test)]
pub(crate) fn size_seen_through(fd: &OwnedFd) -> io::Result<TerminalSize> {
    let mut window: libc::winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    #[allow(unsafe_code)]
    // SAFETY: TIOCGWINSZ writes one `winsize` through the pointer, which refers to a live,
    // correctly typed local.
    let result: libc::c_int =
        unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &raw mut window) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    TerminalSize::new(window.ws_row, window.ws_col)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::{TerminalPair, open, size_seen_through};
    use crate::pty::TerminalSize;

    /// Rows of a roomier terminal than the classic one, so a default cannot pass by accident.
    const ROOMY_ROWS: u16 = 40;
    /// Columns of that roomier terminal.
    const ROOMY_COLUMNS: u16 = 132;

    #[test]
    fn a_new_unix_terminal_has_the_requested_size() {
        let requested: TerminalSize =
            TerminalSize::new(ROOMY_ROWS, ROOMY_COLUMNS).expect("a roomy size is valid");

        let pair: TerminalPair = open(requested).expect("every CI runner can open a terminal");

        assert_eq!(
            size_seen_through(&pair.slave).expect("the size can be read"),
            requested,
            "an agent must start drawing at the size the user's window has"
        );
    }
}
