//! Linux write confinement with Landlock (architecture §5.7 containment matrix).
//!
//! Like the macOS profile, reads and execution stay open and every kind of write is handled:
//! only the writable paths (write set, scratch) and `/dev` (for `/dev/null` and the terminal)
//! may be written. On ABI 4 and later, TCP connections are handled too and only the egress
//! broker's port may be reached. Landlock does not cover UDP: until seccomp or a network
//! namespace closes that gap, a Linux run reports its network confinement as TCP-only.
//! The ruleset is built in the parent; the child only calls `prctl` and
//! `landlock_restrict_self` before `exec`, both async-signal-safe.
//!
//! The constants and structures below follow the kernel's public ABI (`linux/landlock.h`);
//! libc 0.2.191 ships the syscall numbers but not these definitions.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null;

/// `landlock_create_ruleset` flag: return the ABI version instead of creating a ruleset.
const CREATE_RULESET_VERSION: libc::c_uint = 1;
/// `landlock_add_rule` rule type for a path and everything beneath it.
const RULE_PATH_BENEATH: libc::c_int = 1;

const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
/// Linking or renaming across directories (ABI 2 and later).
const ACCESS_FS_REFER: u64 = 1 << 13;
/// Truncating a file (ABI 3 and later).
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
/// Connecting a TCP socket (ABI 4 and later).
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;
/// `landlock_add_rule` rule type for a TCP port.
const RULE_NET_PORT: libc::c_int = 2;
/// The first ABI with network rules.
const NETWORK_ABI: i64 = 4;

/// Every write-like right of ABI 1.
const WRITES_ABI_1: u64 = ACCESS_FS_WRITE_FILE
    | ACCESS_FS_REMOVE_DIR
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_MAKE_CHAR
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_SOCK
    | ACCESS_FS_MAKE_FIFO
    | ACCESS_FS_MAKE_BLOCK
    | ACCESS_FS_MAKE_SYM;

/// `struct landlock_ruleset_attr`, first field only: the kernel accepts the shorter struct of
/// an older ABI.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

/// `struct landlock_net_port_attr`.
#[repr(C)]
struct NetPortAttr {
    allowed_access: u64,
    port: u64,
}

/// `struct landlock_path_beneath_attr`, which the kernel declares packed.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// A Landlock ruleset, built in the parent and applied in the child.
#[derive(Debug)]
pub struct LandlockRuleset {
    fd: OwnedFd,
    /// True when outgoing TCP is limited to the broker (ABI 4 and later). UDP is never covered.
    pub tcp_confined: bool,
}

impl LandlockRuleset {
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// The kernel's Landlock ABI version, or `None` when Landlock is unavailable.
fn abi_version() -> Option<i64> {
    #[allow(unsafe_code)]
    // SAFETY: with a null attribute, zero size and the VERSION flag the call only returns a number.
    let version: libc::c_long = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            null::<RulesetAttr>(),
            0_usize,
            CREATE_RULESET_VERSION,
        )
    };
    (version > 0).then_some(version)
}

/// Builds a ruleset that lets processes write only beneath `writable` and `/dev`. Returns
/// `None` when the kernel has no Landlock, so the caller can report a reduced mode instead of
/// pretending to be confined (P13).
///
/// # Errors
///
/// Returns the operating system's error if a path cannot be opened or a rule added.
pub fn write_ruleset(
    writable: &[PathBuf],
    broker_port: Option<u16>,
) -> io::Result<Option<LandlockRuleset>> {
    let Some(abi) = abi_version() else {
        return Ok(None);
    };
    let mut handled: u64 = WRITES_ABI_1;
    if abi >= 2 {
        handled |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        handled |= ACCESS_FS_TRUNCATE;
    }
    let tcp_confined: bool = abi >= NETWORK_ABI;
    let attribute = RulesetAttr {
        handled_access_fs: handled,
        handled_access_net: if tcp_confined {
            ACCESS_NET_CONNECT_TCP
        } else {
            0
        },
    };
    // An older kernel gets only the filesystem field, as its ABI defines the struct.
    let attribute_size: usize = if tcp_confined {
        size_of::<RulesetAttr>()
    } else {
        size_of::<u64>()
    };
    let no_flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: the attribute is a live, correctly laid out local and the size passed covers only
    // fields it initialises.
    let fd: libc::c_long = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attribute,
            attribute_size,
            no_flags,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd: RawFd = RawFd::try_from(fd).map_err(|_| io::Error::other("ruleset fd out of range"))?;
    #[allow(unsafe_code)]
    // SAFETY: the syscall returned a fresh descriptor owned by nothing else.
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(fd) };
    let ruleset = LandlockRuleset {
        fd: owned,
        tcp_confined,
    };
    for path in writable
        .iter()
        .map(PathBuf::as_path)
        .chain([Path::new("/dev")])
    {
        allow_beneath(&ruleset, path, handled)?;
    }
    if let (true, Some(port)) = (tcp_confined, broker_port) {
        allow_port(&ruleset, port)?;
    }
    Ok(Some(ruleset))
}

fn allow_port(ruleset: &LandlockRuleset, port: u16) -> io::Result<()> {
    let rule = NetPortAttr {
        allowed_access: ACCESS_NET_CONNECT_TCP,
        port: u64::from(port),
    };
    let no_flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: the ruleset descriptor is valid and the rule is a live, correctly laid out local.
    let added: libc::c_long = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.raw_fd(),
            RULE_NET_PORT,
            &raw const rule,
            no_flags,
        )
    };
    if added < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn allow_beneath(ruleset: &LandlockRuleset, path: &Path, access: u64) -> io::Result<()> {
    let mut name: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    name.push(0);
    #[allow(unsafe_code)]
    // SAFETY: `name` is a NUL-terminated path that outlives the call.
    let parent: libc::c_int = unsafe {
        libc::open(
            name.as_ptr().cast::<libc::c_char>(),
            libc::O_PATH | libc::O_CLOEXEC,
        )
    };
    if parent < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: `open` returned a fresh descriptor owned by nothing else.
    let parent: OwnedFd = unsafe { OwnedFd::from_raw_fd(parent) };
    let rule = PathBeneathAttr {
        allowed_access: access,
        parent_fd: parent.as_raw_fd(),
    };
    let no_flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: the ruleset and parent descriptors are valid and the rule is a live, packed local.
    let added: libc::c_long = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.raw_fd(),
            RULE_PATH_BENEATH,
            &raw const rule,
            no_flags,
        )
    };
    if added < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Runs in the forked child before `exec`: no new privileges, then the ruleset. Only
/// async-signal-safe calls are made.
pub(crate) fn restrict_self(ruleset_fd: RawFd) -> io::Result<()> {
    let one: libc::c_ulong = 1;
    let zero: libc::c_ulong = 0;
    #[allow(unsafe_code)]
    // SAFETY: `prctl` with PR_SET_NO_NEW_PRIVS takes integers only.
    let no_new_privileges: libc::c_int =
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero) };
    if no_new_privileges != 0 {
        return Err(io::Error::last_os_error());
    }
    let no_flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: the ruleset descriptor was inherited from the parent and is valid here.
    let restricted: libc::c_long =
        unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, no_flags) };
    if restricted != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LandlockRuleset, write_ruleset};
    use crate::process::{OwnedProcess, ProcessExit, ProcessSpec};

    /// A worktree with an owned directory and one the attempt does not own.
    struct ScratchWorktree {
        root: PathBuf,
    }

    impl ScratchWorktree {
        fn new(name: &str) -> Self {
            let root: PathBuf =
                std::env::temp_dir().join(format!("kaisa-landlock-{name}-{}", std::process::id()));
            for directory in ["src/auth", "src/billing"] {
                std::fs::create_dir_all(root.join(directory)).expect("writable temp dir");
            }
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }
    }

    impl Drop for ScratchWorktree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The attempt owns `src/auth` only. Every supported CI runner has Landlock; if one does not,
    /// this fails loudly instead of letting the confinement tests pass vacuously.
    fn an_auth_only_ruleset(worktree: &ScratchWorktree) -> LandlockRuleset {
        write_ruleset(&[worktree.path("src/auth")], None)
            .expect("the ruleset can be built")
            .expect("the CI kernel supports Landlock")
    }

    /// An agent that writes one file with the shell, as any build script could.
    fn an_agent_writing(target: &Path) -> ProcessSpec {
        ProcessSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_owned(),
                format!("echo changed > '{}'", target.display()),
            ],
            working_directory: std::env::temp_dir(),
        }
    }

    fn run_confined(spec: &ProcessSpec, ruleset: &LandlockRuleset) -> ProcessExit {
        OwnedProcess::spawn_with_landlock(spec, ruleset)
            .expect("the agent starts")
            .wait()
            .expect("the agent exits")
    }

    #[test]
    fn a_sandboxed_agent_cannot_write_outside_its_write_set() {
        let worktree = ScratchWorktree::new("outside");
        let foreign: PathBuf = worktree.path("src/billing/invoice.rs");

        run_confined(
            &an_agent_writing(&foreign),
            &an_auth_only_ruleset(&worktree),
        );

        assert!(
            !foreign.exists(),
            "a shell started by the agent must not write outside the write set"
        );
    }

    #[test]
    fn a_sandboxed_agent_can_write_inside_its_write_set() {
        let worktree = ScratchWorktree::new("inside");
        let owned: PathBuf = worktree.path("src/auth/session.rs");

        let exit: ProcessExit =
            run_confined(&an_agent_writing(&owned), &an_auth_only_ruleset(&worktree));

        assert_eq!(
            (exit, owned.exists()),
            (ProcessExit::Code(0), true),
            "the sandbox must not stop the work the agent was given"
        );
    }

    /// A loopback listener a test can check for connections.
    fn a_loopback_listener() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
        listener
            .set_nonblocking(true)
            .expect("the listener can be polled");
        let port: u16 = listener.local_addr().expect("bound").port();
        (listener, port)
    }

    /// An agent that opens a TCP connection to `port`, as any tool could.
    fn an_agent_connecting_to(port: u16) -> ProcessSpec {
        ProcessSpec {
            program: PathBuf::from("/bin/bash"),
            args: vec![
                "-c".to_owned(),
                format!("echo hello > /dev/tcp/127.0.0.1/{port}"),
            ],
            working_directory: std::env::temp_dir(),
        }
    }

    /// Confines TCP to `broker_port`. CI kernels have network rules; an older one fails loudly.
    fn a_broker_only_ruleset(broker_port: u16) -> LandlockRuleset {
        let ruleset: LandlockRuleset = write_ruleset(&[], Some(broker_port))
            .expect("the ruleset can be built")
            .expect("the CI kernel supports Landlock");
        assert!(
            ruleset.tcp_confined,
            "the CI kernel must support Landlock network rules (ABI 4)"
        );
        ruleset
    }

    #[test]
    fn a_sandboxed_agent_cannot_open_a_direct_connection() {
        let (_broker, broker_port) = a_loopback_listener();
        let (elsewhere, elsewhere_port) = a_loopback_listener();

        run_confined(
            &an_agent_connecting_to(elsewhere_port),
            &a_broker_only_ruleset(broker_port),
        );

        assert!(
            elsewhere.accept().is_err(),
            "an agent must not reach anything but its broker, not even on this machine"
        );
    }

    #[test]
    fn a_sandboxed_agent_can_reach_its_broker() {
        let (broker, broker_port) = a_loopback_listener();

        let exit: ProcessExit = run_confined(
            &an_agent_connecting_to(broker_port),
            &a_broker_only_ruleset(broker_port),
        );

        assert_eq!(
            (exit, broker.accept().is_ok()),
            (ProcessExit::Code(0), true),
            "the broker is the agent's one way out, and it must work"
        );
    }
}
