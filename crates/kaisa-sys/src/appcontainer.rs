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

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS,
    SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, FreeSid, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
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

    /// Grants the container `access` to `path` and, for a directory, everything created in it.
    /// The grant is an ACE on the path itself; [`Self::revoke`] removes it.
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the path's DACL cannot be read or written.
    pub fn grant(&self, path: &Path, access: ContainerAccess) -> io::Result<()> {
        self.change_dacl(path, access.rights(), GRANT_ACCESS)?;
        if access == ContainerAccess::Modify {
            // A container runs at low integrity; without a low label the path's default medium
            // label forbids it to write ("no write up"), whatever the DACL says (spike check 3a).
            set_label(path, LOW_LABEL_INHERITED)?;
        }
        Ok(())
    }

    /// Removes every ACE this container has on `path`.
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the path's DACL cannot be read or written.
    pub fn revoke(&self, path: &Path) -> io::Result<()> {
        self.change_dacl(path, 0, REVOKE_ACCESS)?;
        set_label(path, NO_EXPLICIT_LABEL)
    }

    fn change_dacl(&self, path: &Path, rights: u32, mode: i32) -> io::Result<()> {
        // Read-modify-write of a DACL: two attempts granting the same directory (their shared
        // program directory) at once would otherwise lose one grant, and that agent could not
        // start. Serialized within this process; across Kaisa processes see T15.5e.
        let _serialized = DACL_CHANGES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name: Vec<u16> = wide(path.as_os_str());
        let mut old_dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        #[allow(unsafe_code)]
        // SAFETY: `name` is NUL-terminated; the out-pointers are live locals; the descriptor
        // returned owns `old_dacl` and is released with LocalFree below.
        let read: u32 = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &raw mut old_dacl,
                null_mut(),
                &raw mut descriptor,
            )
        };
        if read != 0 {
            return Err(io::Error::from_raw_os_error(read as i32));
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: rights,
            grfAccessMode: mode,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: self.sid.cast::<u16>(),
            },
        };
        let mut new_dacl: *mut ACL = null_mut();
        #[allow(unsafe_code)]
        // SAFETY: one live entry whose trustee is this profile's SID; `old_dacl` is owned by
        // `descriptor`, still alive; the new ACL is released with LocalFree below.
        let merged: u32 =
            unsafe { SetEntriesInAclW(1, &raw const entry, old_dacl, &raw mut new_dacl) };
        let written: u32 = if merged == 0 {
            #[allow(unsafe_code)]
            // SAFETY: `name` is NUL-terminated and `new_dacl` is the ACL just built.
            unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    new_dacl,
                    null(),
                )
            }
        } else {
            merged
        };
        #[allow(unsafe_code)]
        // SAFETY: both were allocated by the calls above (or are null) and are freed once.
        unsafe {
            LocalFree(new_dacl as HLOCAL);
            LocalFree(descriptor as HLOCAL);
        }
        if written == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(written as i32))
        }
    }

    /// The container's SID as text (`S-1-15-2-…`), for security descriptors.
    ///
    /// # Errors
    ///
    /// Returns the error Windows reports if the SID cannot be converted.
    pub fn sid_string(&self) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        #[allow(unsafe_code)]
        // SAFETY: `sid` is a valid SID owned by `self`; `text` receives a LocalAlloc'd string.
        let converted: i32 = unsafe {
            windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
                self.sid,
                &raw mut text,
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length: usize = 0;
        #[allow(unsafe_code)]
        // SAFETY: Windows returned a NUL-terminated string; it is read up to the NUL, copied,
        // and freed once with LocalFree.
        let sid: String = unsafe {
            while *text.add(length) != 0 {
                length += 1;
            }
            let copied = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text as HLOCAL);
            copied
        };
        Ok(sid)
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

// SAFETY: the SID is written once when the profile is created and only read afterwards; it is
// freed once, on drop, by whichever thread owns the profile then.
#[allow(unsafe_code)]
unsafe impl Send for AppContainerProfile {}
// SAFETY: as above; every method only reads the SID, and DACL changes are serialized.
#[allow(unsafe_code)]
unsafe impl Sync for AppContainerProfile {}

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

/// Creates `spec` inside `profile`'s container, suspended, with `stdin`, `stdout` and `stderr`
/// as its standard handles and no other handle inherited. The caller adopts the process
/// (Job Object, resume) like every other agent.
pub(crate) fn create_suspended_contained(
    profile: &AppContainerProfile,
    spec: &crate::process::ProcessSpec,
    stdin: std::os::windows::io::RawHandle,
    stdout: std::os::windows::io::RawHandle,
    stderr: std::os::windows::io::RawHandle,
) -> io::Result<crate::windows_child::CreatedProcess> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        STARTF_USESTDHANDLES,
    };

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
    let environment: Option<Vec<u16>> = environment_block(&spec.env);
    let mut handles: Vec<HANDLE> = vec![stdin, stdout, stderr];
    handles.sort_unstable();
    handles.dedup();
    for handle in &handles {
        #[allow(unsafe_code)]
        // SAFETY: each handle is a live pipe end owned by the caller for this call.
        let marked: i32 =
            unsafe { SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        if marked == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut list = AttributeList::new(TWO_ATTRIBUTES)?;
    list.set(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        (&raw mut capabilities).cast::<c_void>(),
        size_of::<SECURITY_CAPABILITIES>(),
    )?;
    list.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        handles.as_mut_ptr().cast::<c_void>(),
        size_of::<HANDLE>() * handles.len(),
    )?;
    #[allow(unsafe_code)]
    // SAFETY: an all-zero STARTUPINFOEXW is a valid "no options" value.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
        .map_err(|_| io::Error::other("STARTUPINFOEXW size out of range"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin;
    startup.StartupInfo.hStdOutput = stdout;
    startup.StartupInfo.hStdError = stderr;
    startup.lpAttributeList = list.as_mut_ptr();
    #[allow(unsafe_code)]
    // SAFETY: an all-zero PROCESS_INFORMATION is the documented out-parameter state.
    let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
    let flags: u32 = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_SUSPENDED
        | if environment.is_some() {
            CREATE_UNICODE_ENVIRONMENT
        } else {
            0
        };
    #[allow(unsafe_code)]
    // SAFETY: all strings are NUL-terminated and outlive the call; the command line is mutable;
    // the attribute list, the capabilities and the handle array it points at are live locals;
    // inheritance is limited to the three standard handles by the handle list.
    let created: i32 = unsafe {
        CreateProcessW(
            null(),
            wide_line.as_mut_ptr(),
            null(),
            null(),
            INHERIT_LISTED_HANDLES,
            flags,
            environment
                .as_ref()
                .map_or(null(), |block| block.as_ptr().cast::<c_void>()),
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
    drop(main_thread);
    Ok(crate::windows_child::CreatedProcess::new(
        process,
        information.dwProcessId,
    ))
}

/// Kaisa's environment with `overrides` applied, as a UTF-16 environment block; `None` when
/// there is nothing to override, so the child inherits the environment as is.
fn environment_block(overrides: &[(String, String)]) -> Option<Vec<u16>> {
    if overrides.is_empty() {
        return None;
    }
    let mut variables: std::collections::BTreeMap<String, String> = std::env::vars()
        .map(|(name, value)| (name.to_ascii_uppercase(), format!("{name}={value}")))
        .collect();
    for (name, value) in overrides {
        variables.insert(name.to_ascii_uppercase(), format!("{name}={value}"));
    }
    let mut block: Vec<u16> = Vec::new();
    for entry in variables.values() {
        block.extend(OsStr::new(entry).encode_wide());
        block.push(0);
    }
    block.push(0);
    Some(block)
}

/// The contained process inherits only the handles named in its handle list.
const INHERIT_LISTED_HANDLES: i32 = 1;
/// Security capabilities and the inherited-handle list.
const TWO_ATTRIBUTES: u32 = 2;

/// Serializes DACL changes made by this process.
static DACL_CHANGES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A low mandatory label that does not forbid low writers, inherited by everything below.
const LOW_LABEL_INHERITED: &str = "S:(ML;OICI;NW;;;LW)";
/// No explicit label: the object falls back to the default (medium) label.
const NO_EXPLICIT_LABEL: &str = "S:";

/// Replaces the mandatory label of `path` with the one in `sddl`.
fn set_label(path: &Path, sddl: &str) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetSecurityDescriptorSacl, LABEL_SECURITY_INFORMATION};

    let wide_sddl: Vec<u16> = wide(OsStr::new(sddl));
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: `wide_sddl` is NUL-terminated; the descriptor is LocalAlloc'd and freed below.
    let parsed: i32 = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            null_mut(),
        )
    };
    if parsed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut present: i32 = 0;
    let mut defaulted: i32 = 0;
    let mut sacl: *mut ACL = null_mut();
    let name: Vec<u16> = wide(path.as_os_str());
    #[allow(unsafe_code)]
    // SAFETY: `descriptor` is the live descriptor parsed above and owns `sacl`; `name` is
    // NUL-terminated; the descriptor is freed once after the call.
    let written: u32 = unsafe {
        let read: i32 = GetSecurityDescriptorSacl(
            descriptor,
            &raw mut present,
            &raw mut sacl,
            &raw mut defaulted,
        );
        let written: u32 = if read == 0 {
            u32::MAX
        } else {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null(),
                sacl,
            )
        };
        LocalFree(descriptor as HLOCAL);
        written
    };
    match written {
        0 => Ok(()),
        u32::MAX => Err(io::Error::other("the label SDDL has no SACL")),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// What a container may do with a granted path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAccess {
    /// Read files and run programs: the worktree, toolchains, the agent's own install.
    ReadExecute,
    /// Also create, change and delete: the write set and the scratch directory.
    Modify,
}

impl ContainerAccess {
    fn rights(self) -> u32 {
        match self {
            Self::ReadExecute => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            Self::Modify => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | FILE_GENERIC_WRITE | DELETE,
        }
    }
}

/// A process-thread attribute list holding security capabilities, deleted on drop.
struct AttributeList {
    /// Backing memory; `u64` elements keep it aligned for the pointers Windows stores in it.
    buffer: Vec<u64>,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size: usize = 0;
        #[allow(unsafe_code)]
        // SAFETY: with a null list the call only writes the required size to `size`; it
        // reports ERROR_INSUFFICIENT_BUFFER by design, so its result is ignored.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, NO_FLAGS, &raw mut size);
        }
        let mut buffer: Vec<u64> = vec![0; size.div_ceil(size_of::<u64>())];
        #[allow(unsafe_code)]
        // SAFETY: `buffer` holds at least `size` bytes, as the previous call asked for.
        let initialized: i32 = unsafe {
            InitializeProcThreadAttributeList(
                buffer.as_mut_ptr().cast::<c_void>(),
                count,
                NO_FLAGS,
                &raw mut size,
            )
        };
        if initialized == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }

    /// Adds `attribute` whose value is `size` bytes at `value`. The caller keeps the value
    /// alive until the process is created.
    fn set(&mut self, attribute: u32, value: *mut c_void, size: usize) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: the list was initialized in `new`; `value` points at `size` live bytes.
        let updated: i32 = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                NO_FLAGS,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn with_capabilities(capabilities: *mut SECURITY_CAPABILITIES) -> io::Result<Self> {
        let mut list = Self::new(ONE_ATTRIBUTE)?;
        list.set(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            capabilities.cast::<c_void>(),
            size_of::<SECURITY_CAPABILITIES>(),
        )?;
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

    use super::{AppContainerProfile, ContainerAccess};

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
        let directory: PathBuf = a_user_directory("read");
        let secret: PathBuf = directory.join("secret.txt");
        std::fs::write(&secret, "user data").expect("writable temp dir");
        // No quotes: CreateProcess-style escaping of `"` is not what cmd.exe parses, and the
        // temp path on the runner has no spaces.
        let read = || {
            profile
                .run(
                    CMD,
                    &["/c".to_owned(), format!("type {}", secret.display())],
                    &system_directory(),
                )
                .expect("the contained process starts")
        };

        let before_grant: u32 = read();
        let granted = profile.grant(&directory, ContainerAccess::ReadExecute);
        let after_grant: u32 = read();
        let _ = profile.revoke(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        let _ = profile.delete();

        assert_ne!(
            before_grant, 0,
            "spike check 2: the user's files are unreadable until Kaisa grants a path"
        );
        assert_eq!(
            (granted.is_ok(), after_grant),
            (true, 0),
            "spike check 2 control: the same read succeeds once granted, so the refusal above \
             was the container's, not the command's"
        );
    }

    /// A fresh directory under the user's temporary directory, outside every system grant.
    fn a_user_directory(name: &str) -> PathBuf {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("kaisa-spike-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("writable temp dir");
        directory
    }

    #[test]
    fn a_granted_directory_is_writable_and_its_sibling_is_not() {
        let profile = a_profile("grant");
        let write_set: PathBuf = a_user_directory("owned");
        let sibling: PathBuf = a_user_directory("not-owned");
        profile
            .grant(&write_set, ContainerAccess::Modify)
            .expect("a user can grant their own directory to the container");
        let write_into = |directory: &PathBuf| {
            profile
                .run(
                    CMD,
                    &[
                        "/c".to_owned(),
                        format!("echo changed> {}", directory.join("notes.txt").display()),
                    ],
                    &system_directory(),
                )
                .expect("the contained process starts")
        };

        let _ = write_into(&write_set);
        let _ = write_into(&sibling);
        let wrote_owned: bool = write_set.join("notes.txt").exists();
        let wrote_sibling: bool = sibling.join("notes.txt").exists();
        let revoked = profile.revoke(&write_set);
        for directory in [&write_set, &sibling] {
            let _ = std::fs::remove_dir_all(directory);
        }
        let _ = profile.delete();

        assert!(
            wrote_owned,
            "spike check 3a: a granted directory is writable from inside"
        );
        assert!(
            !wrote_sibling,
            "spike check 3b: an ungranted sibling is not writable"
        );
        assert!(
            revoked.is_ok(),
            "spike check 6: the grant can be removed again"
        );
    }

    /// curl.exe, shipped in System32 since Windows 10 1803.
    const CURL: &str = r"C:\Windows\System32\curl.exe";

    #[test]
    fn a_contained_process_cannot_reach_a_loopback_listener() {
        let profile = a_profile("loopback");
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
        listener
            .set_nonblocking(true)
            .expect("the listener can be polled");
        let port: u16 = listener.local_addr().expect("bound").port();

        let exit = profile.run(
            CURL,
            &[
                "--silent".to_owned(),
                "--max-time".to_owned(),
                "5".to_owned(),
                format!("http://127.0.0.1:{port}/"),
            ],
            &system_directory(),
        );
        let reached: bool = listener.accept().is_ok();
        let _ = profile.delete();

        assert!(exit.is_ok(), "curl starts inside the container: {exit:?}");
        assert!(
            !reached,
            "spike check 4a: loopback is closed to a container, so its broker must not be a TCP \
             port on Windows"
        );
    }

    /// A one-shot inbound named pipe whose security descriptor admits `container_sid`.
    /// AppContainers run at low integrity, so the pipe also carries a low mandatory label.
    fn a_pipe_admitting(name: &str, container_sid: &str) -> std::fs::File {
        use std::os::windows::io::FromRawHandle;

        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
        use windows_sys::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let sddl: Vec<u16> = super::wide(std::ffi::OsStr::new(&format!(
            "D:(A;;GA;;;OW)(A;;GRGW;;;{container_sid})S:(ML;;NW;;;LW)"
        )));
        let mut descriptor: *mut std::ffi::c_void = std::ptr::null_mut();
        #[allow(unsafe_code)]
        // SAFETY: `sddl` is NUL-terminated; `descriptor` receives a LocalAlloc'd descriptor that
        // lives until the end of the test process (test-only leak).
        let parsed: i32 = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            parsed,
            0,
            "the pipe's SDDL parses: {}",
            std::io::Error::last_os_error()
        );
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).expect("small"),
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let wide_name: Vec<u16> = super::wide(std::ffi::OsStr::new(name));
        #[allow(unsafe_code)]
        // SAFETY: the name is NUL-terminated and the attributes point at a live descriptor.
        let handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                &raw const attributes,
            )
        };
        assert_ne!(
            handle,
            windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
            "the pipe is created: {}",
            std::io::Error::last_os_error()
        );
        #[allow(unsafe_code)]
        // SAFETY: a fresh, valid handle that nothing else owns.
        unsafe {
            std::fs::File::from_raw_handle(handle)
        }
    }

    /// Bytes each direction of the test pipe buffers.
    const PIPE_BUFFER: u32 = 4096;
    /// What the contained process writes to the broker pipe.
    const HELLO: &str = "hello-broker";

    #[test]
    fn a_contained_process_reaches_a_named_pipe_that_admits_its_sid() {
        use std::io::Read;
        let profile = a_profile("pipe");
        let name: String = format!(r"\\.\pipe\kaisa-spike-{}", std::process::id());
        let mut pipe = a_pipe_admitting(&name, &profile.sid_string().expect("convertible"));
        let server = std::thread::spawn(move || {
            #[allow(unsafe_code)]
            // SAFETY: the handle is a live, unconnected pipe instance.
            unsafe {
                windows_sys::Win32::System::Pipes::ConnectNamedPipe(
                    std::os::windows::io::AsRawHandle::as_raw_handle(&pipe),
                    std::ptr::null_mut(),
                );
            }
            let mut received = String::new();
            let _ = pipe.read_to_string(&mut received);
            received
        });

        let exit = profile.run(
            CMD,
            &["/c".to_owned(), format!("echo {HELLO}> {name}")],
            &system_directory(),
        );
        if exit.as_ref().map_or(true, |code| *code != 0) {
            // Release the server if the client never connected.
            let _ = std::fs::OpenOptions::new().write(true).open(&name);
        }
        let received: String = server.join().expect("the server thread finishes");
        let _ = profile.delete();

        assert!(
            received.contains(HELLO),
            "spike check 4b: a pipe admitting the container's SID is its way to the broker; \
             exit {exit:?}, received {received:?}"
        );
    }

    /// What the contained Node.js exits with, so the test knows it really ran.
    const NODE_EXIT: u32 = 5;

    /// Where `node.exe` is installed on this machine, if anywhere.
    fn installed_node() -> Option<String> {
        let found = std::process::Command::new("where")
            .arg("node")
            .output()
            .ok()?;
        String::from_utf8_lossy(&found.stdout)
            .lines()
            .map(str::trim)
            .find(|line| line.to_ascii_lowercase().ends_with("node.exe"))
            .map(str::to_owned)
    }

    #[test]
    fn nodejs_starts_inside_a_container_with_only_its_install_granted() {
        let Some(node) = installed_node() else {
            eprintln!("spike check 5 skipped: node.exe is not installed on this runner");
            return;
        };
        let profile = a_profile("node");
        let install: PathBuf = PathBuf::from(&node)
            .parent()
            .expect("node.exe lives in a directory")
            .to_owned();
        let granted = profile.grant(&install, ContainerAccess::ReadExecute);

        let exit = profile.run(
            &node,
            &["-e".to_owned(), format!("process.exit({NODE_EXIT})")],
            &system_directory(),
        );
        let _ = profile.revoke(&install);
        let _ = profile.delete();

        eprintln!("spike check 5: node at {node}, grant {granted:?}, exit {exit:?}");
        assert_eq!(
            exit.ok(),
            Some(NODE_EXIT),
            "spike check 5: the agent runtime can run contained; if not, Windows runs are reduced"
        );
    }

    #[test]
    fn concurrent_grants_on_one_directory_keep_every_containers_access() {
        let shared: PathBuf = a_user_directory("shared-programs");
        std::fs::write(shared.join("tool.txt"), "shared tool").expect("writable");
        let profiles: Vec<AppContainerProfile> = (0..CONCURRENT_ATTEMPTS)
            .map(|index| a_profile(&format!("concurrent{index}")))
            .collect();

        std::thread::scope(|scope| {
            for profile in &profiles {
                scope.spawn(|| {
                    profile
                        .grant(&shared, ContainerAccess::ReadExecute)
                        .expect("granted");
                });
            }
        });
        let readable: Vec<u32> = profiles
            .iter()
            .map(|profile| {
                profile
                    .run(
                        CMD,
                        &[
                            "/c".to_owned(),
                            format!("type {}", shared.join("tool.txt").display()),
                        ],
                        &system_directory(),
                    )
                    .expect("starts")
            })
            .collect();
        for profile in profiles {
            let _ = profile.revoke(&shared);
            let _ = profile.delete();
        }
        let _ = std::fs::remove_dir_all(&shared);

        assert_eq!(
            readable,
            vec![0; CONCURRENT_ATTEMPTS],
            "no attempt's grant on a shared directory is lost to another attempt's grant"
        );
    }

    /// Attempts granting the same directory at the same moment.
    const CONCURRENT_ATTEMPTS: usize = 4;
}
