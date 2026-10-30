//! Creating a Windows process attached to a pseudo console.
//!
//! The process is created suspended, like every Windows agent, so the caller can put it in
//! its Job Object before it runs a single instruction.

use std::ffi::{OsStr, c_void};
use std::io;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::System::Console::HPCON;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, UpdateProcThreadAttribute,
};

use crate::conpty::PseudoConsole;
use crate::process::ProcessSpec;
use crate::windows_child::CreatedProcess;
use crate::windows_command_line::command_line;

/// The attribute list holds exactly one attribute: the pseudo console.
const ONE_ATTRIBUTE: u32 = 1;
/// No flags for the attribute-list calls (reserved, must be zero).
const NO_FLAGS: u32 = 0;
/// The agent inherits no handles from Kaisa; its console is its only standard I/O.
const INHERIT_NO_HANDLES: i32 = 0;

/// A process-thread attribute list naming a pseudo console, deleted on drop.
struct AttributeList {
    /// Backing memory; `u64` elements keep it aligned for the pointers Windows stores in it.
    buffer: Vec<u64>,
}

impl AttributeList {
    fn naming(console: &PseudoConsole) -> io::Result<Self> {
        let mut size: usize = 0;
        #[allow(unsafe_code)]
        // SAFETY: with a null list the call only writes the required size to the live local
        // `size`. It reports ERROR_INSUFFICIENT_BUFFER by design, so its result is ignored.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), ONE_ATTRIBUTE, NO_FLAGS, &raw mut size);
        }
        let mut buffer: Vec<u64> = vec![0; size.div_ceil(size_of::<u64>())];
        #[allow(unsafe_code)]
        // SAFETY: `buffer` holds at least `size` bytes, as the previous call asked for.
        let initialized: i32 = unsafe {
            InitializeProcThreadAttributeList(
                buffer.as_mut_ptr().cast::<c_void>(),
                ONE_ATTRIBUTE,
                NO_FLAGS,
                &raw mut size,
            )
        };
        if initialized == 0 {
            return Err(io::Error::last_os_error());
        }
        // From here on, drop deletes the list.
        let mut list = Self { buffer };
        let pseudo_console: HPCON = console.handle();
        #[allow(unsafe_code)]
        // SAFETY: the list was initialized above. For this attribute the value is the console
        // handle itself (not a pointer to it), as Microsoft's ConPTY documentation shows; the
        // console outlives the process creation that reads it.
        let updated: i32 = unsafe {
            UpdateProcThreadAttribute(
                list.as_mut_ptr(),
                NO_FLAGS,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                pseudo_console as *const c_void,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr().cast::<c_void>()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: the list was initialized when `self` was built and is deleted once, here.
        unsafe {
            DeleteProcThreadAttributeList(self.as_mut_ptr());
        }
    }
}

/// Creates `spec` attached to `console`, suspended.
pub(crate) fn create_suspended_in(
    console: &PseudoConsole,
    spec: &ProcessSpec,
) -> io::Result<CreatedProcess> {
    // The console path inherits Kaisa's environment; refusing beats silently dropping
    // variables such as an agent's proxy settings.
    if !spec.env.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "environment variables are not applied to console processes yet",
        ));
    }
    let program: &str = spec.program.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path is not valid Unicode",
        )
    })?;
    let line: String = command_line(program, &spec.args)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut wide_line: Vec<u16> = wide(OsStr::new(&line));
    let directory: Vec<u16> = wide(spec.working_directory.as_os_str());
    let mut attributes: AttributeList = AttributeList::naming(console)?;

    #[allow(unsafe_code)]
    // SAFETY: STARTUPINFOEXW is plain data for which all-zero bytes are valid.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
        .map_err(|_| io::Error::other("STARTUPINFOEXW size does not fit in u32"))?;
    // Null standard handles: without this flag the agent could write to Kaisa's own
    // (possibly redirected) output instead of its pseudo console.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.as_mut_ptr();
    #[allow(unsafe_code)]
    // SAFETY: PROCESS_INFORMATION is plain data for which all-zero bytes are valid.
    let mut information: PROCESS_INFORMATION = unsafe { zeroed() };

    #[allow(unsafe_code)]
    // SAFETY: the command line is a mutable, NUL-terminated UTF-16 buffer as CreateProcessW
    // requires; the directory is NUL-terminated; the extended startup information is fully
    // initialized and its attribute list stays alive for the call; `information` is a live
    // local the call writes.
    let created: i32 = unsafe {
        CreateProcessW(
            null(),
            wide_line.as_mut_ptr(),
            null(),
            null(),
            INHERIT_NO_HANDLES,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
            null(),
            directory.as_ptr(),
            (&raw const startup).cast::<STARTUPINFOW>(),
            &raw mut information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: CreateProcessW succeeded, so both handles are fresh and owned by nothing else.
    let (process, main_thread): (OwnedHandle, OwnedHandle) = unsafe {
        (
            OwnedHandle::from_raw_handle(information.hProcess),
            OwnedHandle::from_raw_handle(information.hThread),
        )
    };
    // Threads are resumed by id after the Job Object assignment, like every other agent.
    drop(main_thread);
    Ok(CreatedProcess::new(process, information.dwProcessId))
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(once(0)).collect()
}
