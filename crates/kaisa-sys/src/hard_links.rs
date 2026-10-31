//! Stable link-count inspection for broker writes.

use std::io;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::Path;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

/// Returns the number of directory entries naming the existing file at `path`.
///
/// This is a snapshot, not an atomic guarantee against later link creation or replacement.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or its information cannot be read.
#[cfg(windows)]
pub fn hard_link_count(path: &Path) -> io::Result<u64> {
    let file = std::fs::File::open(path)?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    #[allow(unsafe_code)]
    // SAFETY: file owns a live handle and information is writable for the call.
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(information.nNumberOfLinks))
}

/// Returns a snapshot of the number of directory entries naming `path`'s inode.
///
/// # Errors
///
/// Returns the metadata error if the file cannot be inspected.
#[cfg(unix)]
pub fn hard_link_count(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).map(|metadata| metadata.nlink())
}
