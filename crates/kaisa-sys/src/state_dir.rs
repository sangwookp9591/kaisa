//! Where Kaisa keeps its state and its control endpoint, per OS (architecture §5.7, §8).
//!
//! The control endpoint carries human authority, so it lives in a directory only the user can
//! enter (Unix) or behind a per-user pipe whose server checks the client (Windows, T14.4).
//! Environment lookups are injected, so tests never touch the real home directory.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// Overrides every OS default; used by tests and by CI.
pub const STATE_DIR_OVERRIDE: &str = "KAISA_STATE_DIR";
/// File name of the control socket inside the state directory (Unix).
pub const CONTROL_SOCKET_NAME: &str = "control.sock";

/// How human clients reach the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEndpoint {
    /// A Unix domain socket at this path.
    UnixSocket(PathBuf),
    /// A Windows named pipe with this name.
    NamedPipe(String),
}

/// Kaisa's per-user state directory and control endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLocation {
    /// Where the ledger, attempt directories and settings live.
    pub directory: PathBuf,
    /// Where the control endpoint is.
    pub control: ControlEndpoint,
}

/// Works out the state location from environment lookups (`env`).
///
/// # Errors
///
/// Returns `NotFound` if neither the override nor the OS's usual variables are set.
pub fn state_location(env: impl Fn(&str) -> Option<OsString>) -> io::Result<StateLocation> {
    let directory: PathBuf = match env(STATE_DIR_OVERRIDE) {
        Some(directory) => PathBuf::from(directory),
        None => default_directory(&env)?,
    };
    Ok(StateLocation {
        control: control_endpoint(&directory, &env),
        directory,
    })
}

fn missing(variable: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("cannot find Kaisa's state directory: {variable} is not set"),
    )
}

#[cfg(target_os = "linux")]
fn default_directory(env: &impl Fn(&str) -> Option<OsString>) -> io::Result<PathBuf> {
    if let Some(state_home) = env("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join("kaisa"));
    }
    let home: OsString = env("HOME").ok_or_else(|| missing("HOME"))?;
    Ok(PathBuf::from(home).join(".local/state/kaisa"))
}

#[cfg(target_os = "macos")]
fn default_directory(env: &impl Fn(&str) -> Option<OsString>) -> io::Result<PathBuf> {
    let home: OsString = env("HOME").ok_or_else(|| missing("HOME"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/Kaisa"))
}

#[cfg(windows)]
fn default_directory(env: &impl Fn(&str) -> Option<OsString>) -> io::Result<PathBuf> {
    let local: OsString = env("LOCALAPPDATA").ok_or_else(|| missing("LOCALAPPDATA"))?;
    Ok(PathBuf::from(local).join("Kaisa"))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn default_directory(env: &impl Fn(&str) -> Option<OsString>) -> io::Result<PathBuf> {
    let home: OsString = env("HOME").ok_or_else(|| missing("HOME"))?;
    Ok(PathBuf::from(home).join(".kaisa"))
}

#[cfg(unix)]
fn control_endpoint(
    directory: &std::path::Path,
    _env: &impl Fn(&str) -> Option<OsString>,
) -> ControlEndpoint {
    ControlEndpoint::UnixSocket(directory.join(CONTROL_SOCKET_NAME))
}

#[cfg(windows)]
fn control_endpoint(
    _directory: &std::path::Path,
    env: &impl Fn(&str) -> Option<OsString>,
) -> ControlEndpoint {
    let user: String = env("USERNAME")
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let safe: String = user
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    ControlEndpoint::NamedPipe(format!(r"\\.\pipe\kaisa-control-{safe}"))
}

/// Creates `directory` (and its parents) if needed and makes it private to the current user:
/// mode `0700` on Unix. On Windows the per-user profile directory is already private.
///
/// # Errors
///
/// Returns the I/O error if the directory cannot be created or its permissions set.
pub fn ensure_private_directory(directory: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(PRIVATE_MODE))?;
    }
    Ok(())
}

/// Owner may read, write and enter; nobody else may do anything.
#[cfg(unix)]
pub const PRIVATE_MODE: u32 = 0o700;

/// The permission bits of a Unix mode (owner, group, others).
#[cfg(all(unix, test))]
const PERMISSION_BITS: u32 = 0o777;

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{ControlEndpoint, STATE_DIR_OVERRIDE, StateLocation, state_location};

    /// A state directory a test chooses, so nothing touches the real home directory.
    fn a_scratch_state_directory() -> PathBuf {
        std::env::temp_dir().join(format!("kaisa-state-test-{}", std::process::id()))
    }

    fn an_environment_with_state_dir(
        directory: &std::path::Path,
    ) -> impl Fn(&str) -> Option<OsString> {
        let directory: OsString = directory.as_os_str().to_owned();
        move |name: &str| match name {
            STATE_DIR_OVERRIDE => Some(directory.clone()),
            "USERNAME" => Some(OsString::from("alice")),
            _ => None,
        }
    }

    #[test]
    fn the_control_endpoint_lives_in_the_users_state_directory() {
        let directory: PathBuf = a_scratch_state_directory();

        let location: StateLocation =
            state_location(an_environment_with_state_dir(&directory)).expect("the override is set");

        let belongs_to_the_user: bool = match &location.control {
            ControlEndpoint::UnixSocket(path) => path.starts_with(&directory),
            ControlEndpoint::NamedPipe(name) => name.ends_with("alice"),
        };
        assert!(
            belongs_to_the_user,
            "human authority must only be reachable from the user's own space, got {:?}",
            location.control
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_state_directory_is_private_to_its_user() {
        use std::os::unix::fs::PermissionsExt;

        let directory: PathBuf = a_scratch_state_directory().join("private");

        super::ensure_private_directory(&directory).expect("the directory can be created");
        let mode: u32 = std::fs::metadata(&directory)
            .expect("the directory exists")
            .permissions()
            .mode();
        let _ = std::fs::remove_dir_all(a_scratch_state_directory());

        assert_eq!(
            mode & super::PERMISSION_BITS,
            super::PRIVATE_MODE,
            "no other local user may reach the control socket or the ledger"
        );
    }
}
