//! Windows AppContainer, the confinement candidate for Windows agents (T15.5s spike,
//! `docs/research/windows-appcontainer.md`).
//!
//! A per-user AppContainer profile needs no administrator rights. A process started in it can
//! read system directories but none of the user's files until Kaisa grants them, and it has no
//! network capabilities. This module only creates the profile and runs a process in it; the
//! spike's tests record what the container actually allows on a non-admin runner.

use std::ffi::{OsStr, c_void};
use std::io;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{FreeSid, PSID, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::windows_command_line::command_line;

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`: the profile was created earlier.
const PROFILE_EXISTS: i32 = 0x8007_00B7_u32 as i32;
/// The attribute list holds one attribute: the security capabilities.
const ONE_ATTRIBUTE: u32 = 1;
/// Reserved flag arguments, which must be zero.
const NO_FLAGS: u32 = 0;
/// The contained process inherits no handles.
const INHERIT_NO_HANDLES: i32 = 0;

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(once(0)).collect()
}

fn hresult(result: i32) -> io::Result<()> {
    if result >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("HRESULT {:#010x}", result as u32)))
    }
}

/// A per-user AppContainer profile and its SID.
#[derive(Debug)]
pub struct AppContainerProfile {
    name: Vec<u16>,
    sid: PSID,
}

impl AppContainerProfile {
    /// Creates the profile `name` for this user, or opens it if it exists.
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the profile cannot be created or opened.
    pub fn ensure(name: &str) -> io::Result<Self> {
        let name: Vec<u16> = wide(OsStr::new(name));
        let mut sid: PSID = null_mut();
        #[allow(unsafe_code)]
        // SAFETY: `name` is NUL-terminated; no capabilities are passed (null, count 0); `sid`
        // is a live local that receives a SID this type frees with FreeSid.
        let created: i32 = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &raw mut sid,
            )
        };
        if created == PROFILE_EXISTS {
            #[allow(unsafe_code)]
            // SAFETY: as above; the derived SID is also freed with FreeSid.
            let derived: i32 =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
            hresult(derived)?;
        } else {
            hresult(created)?;
        }
        Ok(Self { name, sid })
    }

    /// Deletes the profile (its settings and storage, not files it was granted).
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the profile cannot be deleted.
    pub fn delete(self) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: `name` is NUL-terminated.
        let deleted: i32 = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        hresult(deleted)
    }

    /// Runs `program` with `args` inside the container, in `directory`, and returns its exit
    /// code.
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the process cannot be created or waited for.
    pub fn run(&self, program: &str, args: &[String], directory: &Path) -> io::Result<u32> {
        let line: String = command_line(program, args)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut wide_line: Vec<u16> = wide(OsStr::new(&line));
        let wide_program: Vec<u16> = wide(OsStr::new(program));
        let wide_directory: Vec<u16> = wide(directory.as_os_str());
        let mut capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: self.sid,
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let mut list = AttributeList::with_capabilities(&raw mut capabilities)?;
        #[allow(unsafe_code)]
        // SAFETY: an all-zero STARTUPINFOEXW is a valid "no options" value.
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
            .map_err(|_| io::Error::other("STARTUPINFOEXW size out of range"))?;
        startup.lpAttributeList = list.as_mut_ptr();
        #[allow(unsafe_code)]
        // SAFETY: an all-zero PROCESS_INFORMATION is the documented out-parameter state.
        let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
        #[allow(unsafe_code)]
        // SAFETY: every string is NUL-terminated and outlives the call; the command line is
        // mutable as CreateProcessW requires; `startup` carries a live attribute list whose
        // capabilities point at the live local `capabilities`.
        let created: i32 = unsafe {
            CreateProcessW(
                wide_program.as_ptr(),
                wide_line.as_mut_ptr(),
                null(),
                null(),
                INHERIT_NO_HANDLES,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                wide_directory.as_ptr(),
                (&raw const startup).cast::<STARTUPINFOW>(),
                &raw mut information,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let process: HANDLE = information.hProcess;
        let mut exit_code: u32 = 0;
        #[allow(unsafe_code)]
        // SAFETY: both handles were just returned by CreateProcessW and are closed once here.
        let waited: bool = unsafe {
            let waited: bool = WaitForSingleObject(process, INFINITE) == 0
                && GetExitCodeProcess(process, &raw mut exit_code) != 0;
            CloseHandle(information.hThread);
            CloseHandle(process);
            waited
        };
        if waited {
            Ok(exit_code)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        if !self.sid.is_null() {
            #[allow(unsafe_code)]
            // SAFETY: the SID came from CreateAppContainerProfile or its Derive counterpart,
            // which document FreeSid as the way to release it; it is freed once, here.
            unsafe {
                FreeSid(self.sid);
            }
        }
    }
}

/// A process-thread attribute list holding security capabilities, deleted on drop.
struct AttributeList {
    /// Backing memory; `u64` elements keep it aligned for the pointers Windows stores in it.
    buffer: Vec<u64>,
}

impl AttributeList {
    fn with_capabilities(capabilities: *mut SECURITY_CAPABILITIES) -> io::Result<Self> {
        let mut size: usize = 0;
        #[allow(unsafe_code)]
        // SAFETY: with a null list the call only writes the required size to `size`; it
        // reports ERROR_INSUFFICIENT_BUFFER by design, so its result is ignored.
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
        let mut list = Self { buffer };
        #[allow(unsafe_code)]
        // SAFETY: the list was initialized above; the value points at a SECURITY_CAPABILITIES
        // that the caller keeps alive until the process is created.
        let updated: i32 = unsafe {
            UpdateProcThreadAttribute(
                list.as_mut_ptr(),
                NO_FLAGS,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                capabilities.cast::<c_void>(),
                size_of::<SECURITY_CAPABILITIES>(),
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::AppContainerProfile;

    /// What the contained command exits with, so the test knows it really ran.
    const CONTAINED_EXIT: u32 = 7;
    /// cmd.exe, readable by every AppContainer through ALL APPLICATION PACKAGES.
    const CMD: &str = r"C:\Windows\System32\cmd.exe";

    fn system_directory() -> PathBuf {
        PathBuf::from(r"C:\Windows")
    }

    fn a_profile(test: &str) -> AppContainerProfile {
        AppContainerProfile::ensure(&format!("kaisa.spike.{test}.{}", std::process::id()))
            .expect("a non-admin user can create an AppContainer profile")
    }

    #[test]
    fn a_process_runs_inside_an_appcontainer_without_admin_rights() {
        let profile = a_profile("runs");

        let exit = profile.run(
            CMD,
            &["/c".to_owned(), format!("exit {CONTAINED_EXIT}")],
            &system_directory(),
        );
        let _ = profile.delete();

        assert_eq!(
            exit.expect("the contained process starts"),
            CONTAINED_EXIT,
            "spike check 1: a non-admin user can run a process in an AppContainer"
        );
    }

    #[test]
    fn a_contained_process_cannot_read_the_users_files_without_a_grant() {
        let profile = a_profile("read");
        let secret: PathBuf =
            std::env::temp_dir().join(format!("kaisa-spike-{}.txt", std::process::id()));
        std::fs::write(&secret, "user data").expect("writable temp dir");

        let exit = profile.run(
            CMD,
            &["/c".to_owned(), format!("type \"{}\"", secret.display())],
            &system_directory(),
        );
        let _ = std::fs::remove_file(&secret);
        let _ = profile.delete();

        assert_ne!(
            exit.expect("the contained process starts"),
            0,
            "spike check 2: the user's profile is unreadable until Kaisa grants a path"
        );
    }
}
