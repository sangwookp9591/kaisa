//! Side-effect-free permission checks for diagnostic commands.
use std::io;
use std::path::Path;

/// Whether the current identity can write and search an existing directory, without a probe file.
///
/// Windows opens the existing directory with child-creation access, without creating a child.
/// Actual directory creation is checked when a goal starts, never by read-only diagnosis.
///
/// # Errors
/// Returns an error for invalid paths or unreadable metadata.
#[allow(unsafe_code)]
pub fn directory_writable(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: path is a valid NUL-terminated string; access only reads filesystem permissions.
        Ok(unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } == 0)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_FLAG_BACKUP_SEMANTICS, FILE_TRAVERSE,
        };
        match std::fs::OpenOptions::new()
            .access_mode(FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(false),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(!std::fs::metadata(path)?.permissions().readonly())
    }
}
