//! The child behind an `OwnedProcess` on Windows.
//!
//! Most processes are started by `std::process::Command`. A process attached to a pseudo
//! console needs `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, which std cannot pass on stable Rust
//! 1.99, so Kaisa creates it with `CreateProcessW` itself. Both kinds answer the same questions
//! with std's own `ExitStatus`, so `OwnedProcess` treats them alike.

use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::{Child, ExitStatus};

use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject,
};

/// A wait timeout of zero: only look, never block.
const DO_NOT_BLOCK: u32 = 0;
/// Exit code given to a process Kaisa terminates (reported as `Terminated` anyway).
const TERMINATED_EXIT_CODE: u32 = 1;

/// A Windows child process, however it was created.
#[derive(Debug)]
pub(crate) enum WindowsChild {
    /// Started by `std::process::Command`.
    Std(Child),
    /// Started by Kaisa through `CreateProcessW`.
    Created(CreatedProcess),
}

/// A process Kaisa created itself, held by its process handle.
#[derive(Debug)]
pub(crate) struct CreatedProcess {
    handle: OwnedHandle,
    id: u32,
}

impl CreatedProcess {
    /// Takes ownership of a process handle returned by `CreateProcessW`.
    pub(crate) fn new(handle: OwnedHandle, id: u32) -> Self {
        Self { handle, id }
    }

    fn has_exited(&self, timeout_ms: u32) -> io::Result<bool> {
        #[allow(unsafe_code)]
        // SAFETY: the process handle is owned by `self` and valid.
        let event: u32 = unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout_ms) };
        match event {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code: u32 = 0;
        #[allow(unsafe_code)]
        // SAFETY: the handle is valid and `code` is a live local the call writes once.
        let read: i32 = unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &raw mut code) };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
    }
}

impl WindowsChild {
    /// The operating system's process id.
    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Std(child) => child.id(),
            Self::Created(process) => process.id,
        }
    }

    /// The process handle, for Job Object assignment.
    pub(crate) fn as_raw_handle(&self) -> RawHandle {
        match self {
            Self::Std(child) => child.as_raw_handle(),
            Self::Created(process) => process.handle.as_raw_handle(),
        }
    }

    /// Waits until the process exits.
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Std(child) => child.wait(),
            Self::Created(process) => {
                process.has_exited(INFINITE)?;
                process.exit_status()
            }
        }
    }

    /// The exit status if the process already exited, without waiting.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Std(child) => child.try_wait(),
            Self::Created(process) => {
                if process.has_exited(DO_NOT_BLOCK)? {
                    process.exit_status().map(Some)
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Ends this one process (not its tree; that is the Job Object's work).
    pub(crate) fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Std(child) => child.kill(),
            Self::Created(process) => {
                #[allow(unsafe_code)]
                // SAFETY: the process handle is owned by `process` and valid.
                let ended: i32 = unsafe {
                    TerminateProcess(process.handle.as_raw_handle(), TERMINATED_EXIT_CODE)
                };
                if ended == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }
}
