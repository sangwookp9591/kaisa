//! Who is on the other end of a local socket (architecture §5.7, §8 "Control IPC").
//!
//! The control endpoint carries human authority, so a connection is accepted only from the
//! daemon's own user, as the kernel reports it: `SO_PEERCRED` on Linux, `getpeereid` on macOS.
//! What a client says about itself is never consulted.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::{fmt, io};

/// A connection came from another user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignPeer {
    /// The user id the kernel reported for the peer.
    pub peer_uid: u32,
}

impl fmt::Display for ForeignPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing a control connection from user {}: only the daemon's own user may connect",
            self.peer_uid
        )
    }
}

impl std::error::Error for ForeignPeer {}

/// The effective user id of this process.
#[must_use]
pub fn current_uid() -> u32 {
    #[allow(unsafe_code)]
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory.
    let uid: libc::uid_t = unsafe { libc::geteuid() };
    uid
}

/// Accepts a peer only if it is `own_uid`.
///
/// # Errors
///
/// Returns [`ForeignPeer`] for any other user.
pub fn require_same_user(peer_uid: u32, own_uid: u32) -> Result<(), ForeignPeer> {
    if peer_uid == own_uid {
        Ok(())
    } else {
        Err(ForeignPeer { peer_uid })
    }
}

/// The user id of the process on the other end of `stream`, as the kernel reports it.
///
/// # Errors
///
/// Returns the operating system's error if the credentials cannot be read.
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length: libc::socklen_t = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .map_err(|_| io::Error::other("ucred size does not fit socklen_t"))?;
    #[allow(unsafe_code)]
    // SAFETY: the descriptor is a live socket; `credentials` and `length` are live locals of the
    // types SO_PEERCRED writes, and `length` holds the buffer's size.
    let result: libc::c_int = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast::<libc::c_void>(),
            &raw mut length,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

/// The user id of the process on the other end of `stream`, as the kernel reports it.
///
/// # Errors
///
/// Returns the operating system's error if the credentials cannot be read.
#[cfg(not(target_os = "linux"))]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    #[allow(unsafe_code)]
    // SAFETY: the descriptor is a live socket and both out-pointers refer to live locals.
    let result: libc::c_int =
        unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use super::{ForeignPeer, current_uid, peer_uid, require_same_user};

    /// The conventional uid of the unprivileged `nobody` user: someone other than the tester.
    const NOBODY_UID: u32 = 65_534;

    /// A short socket path: Unix socket paths are limited to about 100 bytes.
    fn a_socket_path() -> PathBuf {
        std::env::temp_dir().join(format!("kaisa-peer-{}.sock", std::process::id()))
    }

    #[test]
    fn a_client_of_the_same_user_is_accepted() {
        let path: PathBuf = a_socket_path();
        let _ = std::fs::remove_file(&path);
        let listener: UnixListener = UnixListener::bind(&path).expect("the socket can be bound");
        let _client: UnixStream = UnixStream::connect(&path).expect("the client connects");
        let (server_side, _) = listener.accept().expect("the connection is accepted");

        let peer: u32 = peer_uid(&server_side).expect("the kernel reports the peer");
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            require_same_user(peer, current_uid()),
            Ok(()),
            "the user's own CLI must reach their daemon"
        );
    }

    #[test]
    fn the_peer_check_rejects_another_user() {
        let own: u32 = current_uid();
        if own == NOBODY_UID {
            return;
        }

        assert_eq!(
            require_same_user(NOBODY_UID, own),
            Err(ForeignPeer {
                peer_uid: NOBODY_UID
            }),
            "another local user must never get human authority over this user's agents"
        );
    }
}
