//! Windows pseudo consoles (ConPTY), the Windows counterpart of `openpty`.
//!
//! A pseudo console reads the agent's input from one pipe and writes everything the agent
//! draws, as VT sequences, to another. Kaisa keeps the other end of each pipe.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

use crate::pty::TerminalSize;

/// Flags for `CreatePseudoConsole`: none, so the console starts with a clean screen.
const NO_FLAGS: u32 = 0;
/// The value of an `HPCON` that does not refer to a console.
const NO_CONSOLE: HPCON = 0;

/// An open pseudo console, closed on drop.
#[derive(Debug)]
pub(crate) struct PseudoConsole {
    handle: HPCON,
}

impl PseudoConsole {
    /// The raw handle, for attaching a process to this console.
    pub(crate) fn handle(&self) -> HPCON {
        self.handle
    }

    /// Changes the size the attached agent sees.
    pub(crate) fn resize(&self, size: TerminalSize) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: the console handle is owned by `self` and valid; the size is passed by value.
        let result: i32 = unsafe { ResizePseudoConsole(self.handle, coord_of(size)?) };
        if result < 0 {
            Err(io::Error::from_raw_os_error(result))
        } else {
            Ok(())
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: the handle came from a successful `CreatePseudoConsole` and is closed exactly
        // once, here.
        unsafe {
            ClosePseudoConsole(self.handle);
        }
    }
}

/// A pseudo console with Kaisa's ends of its two pipes.
#[derive(Debug)]
pub(crate) struct OpenedConsole {
    /// The console itself.
    pub(crate) console: PseudoConsole,
    /// Writing here types into the agent's console.
    pub(crate) input: OwnedHandle,
    /// Reading here yields what the agent draws.
    pub(crate) output: OwnedHandle,
}

/// Opens a pseudo console of `size`.
pub(crate) fn open(size: TerminalSize) -> io::Result<OpenedConsole> {
    let (input_for_console, input): (OwnedHandle, OwnedHandle) = pipe()?;
    let (output, output_for_console): (OwnedHandle, OwnedHandle) = pipe()?;
    let mut handle: HPCON = NO_CONSOLE;
    #[allow(unsafe_code)]
    // SAFETY: both pipe handles are valid for the call and the console handle pointer refers
    // to a live local.
    let result: i32 = unsafe {
        CreatePseudoConsole(
            coord_of(size)?,
            input_for_console.as_raw_handle(),
            output_for_console.as_raw_handle(),
            NO_FLAGS,
            &raw mut handle,
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    // The console keeps its own duplicates. Closing ours is what lets a read of `output`
    // end once the console is closed.
    drop(input_for_console);
    drop(output_for_console);
    Ok(OpenedConsole {
        console: PseudoConsole { handle },
        input,
        output,
    })
}

/// A console size in the form Windows takes it.
pub(crate) fn coord_of(size: TerminalSize) -> io::Result<COORD> {
    let too_large = |_| io::Error::new(io::ErrorKind::InvalidInput, "terminal size too large");
    Ok(COORD {
        X: i16::try_from(size.columns()).map_err(too_large)?,
        Y: i16::try_from(size.rows()).map_err(too_large)?,
    })
}

/// A new anonymous pipe as (read end, write end).
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: both handle pointers refer to live locals; null attributes mean "not
    // inheritable", and a zero size asks for the default buffer.
    let created: i32 = unsafe { CreatePipe(&raw mut read, &raw mut write, null(), 0) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: `CreatePipe` succeeded, so both are fresh handles that nothing else owns.
    let ends: (OwnedHandle, OwnedHandle) = unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    };
    Ok(ends)
}

#[cfg(test)]
mod tests {
    use super::{NO_CONSOLE, OpenedConsole, open};
    use crate::pty::TerminalSize;

    /// Rows of a classic text terminal.
    const CLASSIC_ROWS: u16 = 24;
    /// Columns of a classic text terminal.
    const CLASSIC_COLUMNS: u16 = 80;

    #[test]
    fn a_new_windows_pseudo_console_can_be_opened() {
        let size: TerminalSize =
            TerminalSize::new(CLASSIC_ROWS, CLASSIC_COLUMNS).expect("a classic size is valid");

        let opened: OpenedConsole =
            open(size).expect("Windows 10 1809 and later (every CI runner) has ConPTY");

        assert_ne!(
            opened.console.handle(),
            NO_CONSOLE,
            "an agent can only be attached to a console that exists"
        );
    }
}
