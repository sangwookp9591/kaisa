//! The Windows control endpoint: a named pipe only the daemon's own user can use
//! (architecture §5.7 "Windows pipe checks", §8 "Control IPC").
//!
//! The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so another process cannot
//! squat the name before the daemon; remote clients are rejected by the pipe itself; and every
//! client's user SID, read from its process token, must equal the daemon's.

use std::ffi::OsStr;
use std::io;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Buffer size, in bytes, for each direction of the pipe.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
/// Zero means "use the default timeout" for `CreateNamedPipeW`.
const DEFAULT_TIMEOUT: u32 = 0;
/// The handle is not inherited by child processes.
const NOT_INHERITED: i32 = 0;

/// One instance of the control pipe, owned by the daemon.
#[derive(Debug)]
pub struct ControlPipe {
    handle: OwnedHandle,
}

impl ControlPipe {
    /// Creates the first instance of the pipe `name`.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error; an existing instance by any process makes this fail,
    /// which is what stops a squatter from posing as the daemon.
    pub fn create_first(name: &str) -> io::Result<Self> {
        Self::create_first_with(name, null())
    }

    /// Creates the first instance of the pipe `name` that the AppContainer `container_sid` may
    /// also open: the attempt's broker endpoint, reachable from inside its container. The
    /// descriptor admits the owner and that container only, with a low mandatory label so the
    /// low-integrity container may write (spike check 4b).
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the descriptor cannot be built or the pipe exists.
    pub fn create_first_admitting(name: &str, container_sid: &str) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

        if !container_sid.starts_with("S-1-15-2-")
            || !container_sid
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, 'S' | '-'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an AppContainer SID",
            ));
        }
        let sddl: Vec<u16> = OsStr::new(&format!(
            "D:(A;;GA;;;OW)(A;;GRGW;;;{container_sid})S:(ML;;NW;;;LW)"
        ))
        .encode_wide()
        .chain(once(0))
        .collect();
        let mut descriptor: *mut core::ffi::c_void = null_mut();
        #[allow(unsafe_code)]
        // SAFETY: the SDDL is NUL-terminated; `descriptor` receives a LocalAlloc'd descriptor
        // freed below.
        let parsed: i32 = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if parsed == 0 {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: descriptor,
            bInheritHandle: NOT_INHERITED,
        };
        let created = Self::create_first_with(name, &raw const attributes);
        #[allow(unsafe_code)]
        // SAFETY: the descriptor was allocated above and the pipe keeps its own copy.
        unsafe {
            LocalFree(descriptor as HLOCAL);
        }
        created
    }

    fn create_first_with(
        name: &str,
        attributes: *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    ) -> io::Result<Self> {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(once(0)).collect();
        #[allow(unsafe_code)]
        // SAFETY: the name is a NUL-terminated UTF-16 buffer that outlives the call; the
        // attributes are null (default, creator-only descriptor) or point at a live descriptor.
        let handle: HANDLE = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                DEFAULT_TIMEOUT,
                attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        #[allow(unsafe_code)]
        // SAFETY: the handle is fresh and owned by nothing else.
        let handle: OwnedHandle = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Self { handle })
    }

    /// Waits for a client to connect (or accepts one that already did).
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the connection fails.
    pub fn accept(&self) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: the pipe handle is valid; no overlapped structure is used.
        let connected: i32 = unsafe { ConnectNamedPipe(self.handle.as_raw_handle(), null_mut()) };
        if connected != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        // A client that connected between creation and this call is already connected.
        if error.raw_os_error() == Some(i32::try_from(ERROR_PIPE_CONNECTED).unwrap_or(i32::MAX)) {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// The connected pipe as a byte stream for reading and writing.
    #[must_use]
    pub fn into_stream(self) -> std::fs::File {
        std::fs::File::from(self.handle)
    }

    /// True if the connected client runs as the same user as this process.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the client's identity cannot be read; callers
    /// must then refuse the client.
    pub fn client_is_same_user(&self) -> io::Result<bool> {
        let mut client_pid: u32 = 0;
        #[allow(unsafe_code)]
        // SAFETY: the pipe handle is valid and the out-pointer refers to a live local.
        let found: i32 = unsafe {
            GetNamedPipeClientProcessId(self.handle.as_raw_handle(), &raw mut client_pid)
        };
        if found == 0 {
            return Err(io::Error::last_os_error());
        }
        #[allow(unsafe_code)]
        // SAFETY: opening a process by id has no memory-safety preconditions.
        let client: HANDLE =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, NOT_INHERITED, client_pid) };
        if client.is_null() {
            return Err(io::Error::last_os_error());
        }
        #[allow(unsafe_code)]
        // SAFETY: `client` was just opened and is owned by nothing else.
        let client: OwnedHandle = unsafe { OwnedHandle::from_raw_handle(client) };
        #[allow(unsafe_code)]
        // SAFETY: the pseudo-handle of the current process is always valid and need not be closed.
        let own_process: HANDLE = unsafe { GetCurrentProcess() };
        Ok(user_sid_of(client.as_raw_handle())? == user_sid_of(own_process)?)
    }
}

/// The bytes of the user SID in `process`'s token.
fn user_sid_of(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut token: HANDLE = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: `process` is a valid process handle and the out-pointer refers to a live local.
    let opened: i32 = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: the token handle was just opened and is owned by nothing else.
    let token: OwnedHandle = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed: u32 = 0;
    #[allow(unsafe_code)]
    // SAFETY: with a null buffer and zero length the call only writes the needed size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            null_mut(),
            0,
            &raw mut needed,
        );
    }
    // `u64` elements keep the buffer aligned for the pointer inside TOKEN_USER.
    let mut buffer: Vec<u64> = vec![0; (needed as usize).div_ceil(size_of::<u64>())];
    #[allow(unsafe_code)]
    // SAFETY: the buffer holds at least `needed` bytes, as the previous call asked for.
    let read: i32 = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: the call filled the buffer with a TOKEN_USER whose SID pointer points inside it;
    // GetLengthSid reads only that SID, and the copy happens while the buffer is alive.
    let sid: Vec<u8> = unsafe {
        let user: &TOKEN_USER = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length: usize = GetLengthSid(user.User.Sid) as usize;
        std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length).to_vec()
    };
    Ok(sid)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::ptr::null;

    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};

    use super::ControlPipe;

    /// What the contained relay sends its broker.
    const BROKER_HELLO: &str = "hello-broker-pipe";

    /// No sharing, attributes or template for the client end.
    const NO_SHARING: u32 = 0;
    const NO_FLAGS: u32 = 0;

    fn a_pipe_name(purpose: &str) -> String {
        format!(r"\\.\pipe\kaisa-test-{purpose}-{}", std::process::id())
    }

    /// Connects to the pipe as a client of the same user, as the Kaisa CLI would.
    fn connect_as_the_cli(name: &str) -> OwnedHandle {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(once(0)).collect();
        #[allow(unsafe_code)]
        // SAFETY: the name is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                NO_SHARING,
                null(),
                OPEN_EXISTING,
                NO_FLAGS,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE, "the CLI can open the pipe");
        #[allow(unsafe_code)]
        // SAFETY: the handle is fresh and owned by nothing else.
        unsafe {
            OwnedHandle::from_raw_handle(handle)
        }
    }

    #[test]
    fn a_client_of_the_same_user_is_accepted() {
        let name: String = a_pipe_name("same-user");
        let pipe: ControlPipe = ControlPipe::create_first(&name).expect("the pipe can be created");
        let _client: OwnedHandle = connect_as_the_cli(&name);

        pipe.accept().expect("the connection is accepted");

        assert!(
            pipe.client_is_same_user()
                .expect("the client's token is readable"),
            "the user's own CLI must reach their daemon"
        );
    }

    #[test]
    fn a_second_server_on_the_same_pipe_name_is_refused() {
        let name: String = a_pipe_name("squatter");
        let _daemon: ControlPipe =
            ControlPipe::create_first(&name).expect("the pipe can be created");

        let squatter = ControlPipe::create_first(&name);

        assert!(
            squatter.is_err(),
            "nobody else may pose as the daemon on an existing pipe name"
        );
    }

    #[test]
    fn a_broker_pipe_admits_its_container_and_carries_its_bytes() {
        use std::io::Read;
        let profile = crate::AppContainerProfile::ensure(&format!(
            "kaisa.test.broker.{}",
            std::process::id()
        ))
        .expect("a non-admin user can create a profile");
        let name: String = format!(r"\\.\pipe\kaisa-broker-test-{}", std::process::id());
        let pipe = ControlPipe::create_first_admitting(&name, &profile.sid_string().expect("sid"))
            .expect("the broker pipe is created");
        let server = std::thread::spawn(move || {
            pipe.accept().expect("the contained client connects");
            let mut received = String::new();
            let _ = pipe.into_stream().read_to_string(&mut received);
            received
        });

        let exit = profile.run(
            r"C:\Windows\System32\cmd.exe",
            &["/c".to_owned(), format!("echo {BROKER_HELLO}> {name}")],
            std::path::Path::new(r"C:\Windows"),
        );
        let received: String = server.join().expect("the server thread finishes");
        let _ = profile.delete();

        assert!(
            received.contains(BROKER_HELLO),
            "the contained agent's relay reaches its broker pipe; exit {exit:?}"
        );
    }

    #[test]
    fn a_broker_pipe_refuses_a_descriptor_built_from_something_other_than_a_container_sid() {
        let name: String = format!(r"\\.\pipe\kaisa-broker-bad-{}", std::process::id());

        let created = ControlPipe::create_first_admitting(&name, "S-1-1-0)(A;;GA;;;WD");

        assert!(
            created.is_err(),
            "only an AppContainer SID may be spliced into the pipe's descriptor"
        );
    }
}
