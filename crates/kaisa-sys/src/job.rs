//! Windows Job Objects: every process an agent starts belongs to its job, so the whole tree
//! can be ended at once (architecture §5.1, §8).
//!
//! The agent is created suspended, assigned to a new job, and only then resumed, so it cannot
//! start a child before the job contains it.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::RawHandle;
use std::ptr::null;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

/// Creation flag that starts a process with its main thread suspended.
pub(crate) const CREATE_SUSPENDED: u32 = windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

/// Exit code given to processes ended through the job (reported as `Terminated` anyway).
const TERMINATED_EXIT_CODE: u32 = 1;

/// An owned Job Object handle, closed on drop.
#[derive(Debug)]
pub(crate) struct Job(HANDLE);

impl Job {
    /// Creates an anonymous job.
    pub(crate) fn new() -> io::Result<Self> {
        #[allow(unsafe_code)]
        // SAFETY: null security attributes and a null name are documented as valid and create
        // an anonymous job with default security.
        let handle: HANDLE = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    /// Puts `process` into the job.
    pub(crate) fn assign(&self, process: RawHandle) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: both handles are valid for the duration of the call: the job is owned by
        // `self`, and `process` belongs to a `Child` the caller still holds.
        let assigned: i32 = unsafe { AssignProcessToJobObject(self.0, process as HANDLE) };
        if assigned == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Ends every process in the job.
    pub(crate) fn terminate(&self) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: the job handle is owned by `self` and valid.
        let terminated: i32 = unsafe { TerminateJobObject(self.0, TERMINATED_EXIT_CODE) };
        if terminated == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// How many processes are currently alive in the job.
    #[cfg(test)]
    pub(crate) fn active_processes(&self) -> io::Result<u32> {
        use std::ffi::c_void;
        use std::ptr::null_mut;

        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        #[allow(unsafe_code)]
        // SAFETY: the structure is plain data for which all-zero bytes are valid.
        let mut information: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        let size: u32 = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .expect("the structure is a few dozen bytes");
        #[allow(unsafe_code)]
        // SAFETY: `information` is a valid, writable buffer of exactly `size` bytes for this
        // information class, and the job handle is valid.
        let queried: i32 = unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                (&raw mut information).cast::<c_void>(),
                size,
                null_mut(),
            )
        };
        if queried == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(information.ActiveProcesses)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: the handle is owned by `self` and closed exactly once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Resumes every thread of the suspended process `process_id`.
pub(crate) fn resume_threads_of(process_id: u32) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: a thread snapshot has no preconditions; the process id argument is ignored for
    // TH32CS_SNAPTHREAD.
    let snapshot: HANDLE = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let resumed: io::Result<()> = resume_matching_threads(snapshot, process_id);
    #[allow(unsafe_code)]
    // SAFETY: the snapshot handle was created above and is closed exactly once.
    unsafe {
        CloseHandle(snapshot);
    }
    resumed
}

fn resume_matching_threads(snapshot: HANDLE, process_id: u32) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: the structure is plain data for which all-zero bytes are valid.
    let mut entry: THREADENTRY32 = unsafe { zeroed() };
    entry.dwSize =
        u32::try_from(size_of::<THREADENTRY32>()).expect("the structure is a few dozen bytes");
    #[allow(unsafe_code)]
    // SAFETY: `snapshot` is a valid thread snapshot and `entry.dwSize` is set as required.
    let mut more: i32 = unsafe { Thread32First(snapshot, &raw mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == process_id {
            resume_thread(entry.th32ThreadID)?;
        }
        #[allow(unsafe_code)]
        // SAFETY: as for `Thread32First`.
        let next: i32 = unsafe { Thread32Next(snapshot, &raw mut entry) };
        more = next;
    }
    Ok(())
}

fn resume_thread(thread_id: u32) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: opening a thread by id has no memory-safety preconditions.
    let thread: HANDLE = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: `thread` was opened above with THREAD_SUSPEND_RESUME access.
    let previous_count: u32 = unsafe { ResumeThread(thread) };
    #[allow(unsafe_code)]
    // SAFETY: the thread handle was opened above and is closed exactly once.
    unsafe {
        CloseHandle(thread);
    }
    if previous_count == u32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
