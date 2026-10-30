//! macOS Seatbelt profiles (architecture §5.7 containment matrix, write-set confinement).
//!
//! The profile denies everything by default, then allows what an agent needs to run, reads,
//! writes only inside its writable paths, and network only to its egress broker. Paths are
//! embedded as escaped SBPL strings, so no path name can end the string and add rules.

use std::path::PathBuf;

use crate::process::ProcessSpec;

/// The macOS tool that runs a program under a Seatbelt profile; it execs the program in place,
/// so process-group ownership is unchanged.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// What one attempt's Seatbelt profile allows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeatbeltPolicy {
    /// Directories the agent may never read, such as Kaisa's state directory and other
    /// attempts' worktrees. Everything else may be read.
    pub unreadable: Vec<PathBuf>,
    /// Directories (or files) the agent may change: the write set and the scratch area. Paths
    /// must be absolute and canonical (`/private/var`, not `/var`).
    pub writable: Vec<PathBuf>,
    /// The loopback port of the attempt's egress broker; `None` means no network at all.
    pub broker_port: Option<u16>,
    /// The Unix socket of the attempt's tool broker, the one local endpoint the agent may
    /// connect to. Must be canonical, like `writable`.
    pub broker_socket: Option<PathBuf>,
}

/// The rules every agent needs just to run: start processes, use its terminal and system
/// services, read system configuration.
const BASE_RULES: &str = "(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow mach-lookup)
(allow ipc-posix-shm)
(allow file-read*)
(allow file-write-data (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\"))
(allow file-write* (regex #\"^/dev/ttys[0-9]+$\"))
(allow file-ioctl (regex #\"^/dev/ttys[0-9]+$\") (literal \"/dev/tty\"))
";

/// The SBPL text of `policy`.
#[must_use]
pub fn seatbelt_profile(policy: &SeatbeltPolicy) -> String {
    let mut profile: String = String::from("(version 1)\n(deny default)\n");
    profile.push_str(BASE_RULES);
    for path in &policy.unreadable {
        profile.push_str(&format!(
            "(deny file-read* file-write* (subpath {}))\n",
            sbpl_string(&path.to_string_lossy())
        ));
    }
    for path in &policy.writable {
        profile.push_str(&format!(
            "(allow file-write* (subpath {}))\n",
            sbpl_string(&path.to_string_lossy())
        ));
    }
    if let Some(socket) = &policy.broker_socket {
        profile.push_str(&format!(
            "(allow network-outbound (literal {}))\n",
            sbpl_string(&socket.to_string_lossy())
        ));
    }
    if let Some(port) = policy.broker_port {
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    profile
}

/// `spec` wrapped so that it runs under the Seatbelt profile of `policy`.
#[must_use]
pub fn sandboxed_spec(spec: &ProcessSpec, policy: &SeatbeltPolicy) -> ProcessSpec {
    let mut args: Vec<String> = vec![
        "-p".to_owned(),
        seatbelt_profile(policy),
        "--".to_owned(),
        spec.program.to_string_lossy().into_owned(),
    ];
    args.extend(spec.args.iter().cloned());
    ProcessSpec {
        program: PathBuf::from(SANDBOX_EXEC),
        args,
        working_directory: spec.working_directory.clone(),
    }
}

/// `text` as an SBPL string literal: wrapped in quotes, with backslashes and quotes escaped.
fn sbpl_string(text: &str) -> String {
    let mut literal: String = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        if matches!(character, '"' | '\\') {
            literal.push('\\');
        }
        literal.push(character);
    }
    literal.push('"');
    literal
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{SeatbeltPolicy, seatbelt_profile};

    #[test]
    fn the_seatbelt_profile_denies_by_default() {
        let profile: String = seatbelt_profile(&SeatbeltPolicy::default());

        assert!(
            profile.starts_with("(version 1)\n(deny default)\n"),
            "anything not explicitly allowed must be refused, got {profile:?}"
        );
        assert!(
            !profile.contains("network-outbound"),
            "an agent without a broker gets no network at all"
        );
    }

    #[test]
    fn a_path_with_quotes_cannot_break_out_of_the_profile() {
        let hostile: PathBuf = PathBuf::from("/tmp/x\") (allow file-write* (subpath \"/");
        let policy: SeatbeltPolicy = SeatbeltPolicy {
            writable: vec![hostile],
            ..SeatbeltPolicy::default()
        };

        let profile: String = seatbelt_profile(&policy);

        assert!(
            !profile.contains("(subpath \"/\")"),
            "a directory name must never become a rule that opens the whole disk, got {profile:?}"
        );
    }

    #[cfg(target_os = "macos")]
    mod enforced {
        use std::path::{Path, PathBuf};

        use super::super::{SeatbeltPolicy, sandboxed_spec};
        use crate::process::{OwnedProcess, ProcessExit, ProcessSpec};

        /// A worktree with an owned directory and one the attempt does not own.
        struct ScratchWorktree {
            root: PathBuf,
        }

        impl ScratchWorktree {
            fn new(name: &str) -> Self {
                let root: PathBuf = std::env::temp_dir()
                    .join(format!("kaisa-seatbelt-{name}-{}", std::process::id()));
                for directory in ["src/auth", "src/billing"] {
                    std::fs::create_dir_all(root.join(directory)).expect("writable temp dir");
                }
                // Seatbelt matches canonical paths: /var is /private/var on macOS.
                let root: PathBuf = root.canonicalize().expect("the directory exists");
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

        /// The attempt owns `src/auth` only.
        fn an_auth_only_policy(worktree: &ScratchWorktree) -> SeatbeltPolicy {
            SeatbeltPolicy {
                writable: vec![worktree.path("src/auth")],
                ..SeatbeltPolicy::default()
            }
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

        fn run_sandboxed(spec: &ProcessSpec, policy: &SeatbeltPolicy) -> ProcessExit {
            OwnedProcess::spawn(&sandboxed_spec(spec, policy))
                .expect("sandbox-exec is part of macOS")
                .wait()
                .expect("the agent exits")
        }

        #[test]
        fn a_sandboxed_agent_cannot_write_outside_its_write_set() {
            let worktree = ScratchWorktree::new("outside");
            let foreign: PathBuf = worktree.path("src/billing/invoice.rs");

            run_sandboxed(&an_agent_writing(&foreign), &an_auth_only_policy(&worktree));

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
                run_sandboxed(&an_agent_writing(&owned), &an_auth_only_policy(&worktree));

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

        /// True if `listener` received a connection.
        fn was_reached(listener: &std::net::TcpListener) -> bool {
            listener.accept().is_ok()
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

        fn a_policy_with_broker(port: u16) -> SeatbeltPolicy {
            SeatbeltPolicy {
                broker_port: Some(port),
                ..SeatbeltPolicy::default()
            }
        }

        #[test]
        fn a_sandboxed_agent_cannot_open_a_direct_connection() {
            let (_broker, broker_port) = a_loopback_listener();
            let (elsewhere, elsewhere_port) = a_loopback_listener();

            run_sandboxed(
                &an_agent_connecting_to(elsewhere_port),
                &a_policy_with_broker(broker_port),
            );

            assert!(
                !was_reached(&elsewhere),
                "an agent must not reach anything but its broker, not even on this machine"
            );
        }

        #[test]
        fn a_sandboxed_agent_can_reach_its_broker() {
            let (broker, broker_port) = a_loopback_listener();

            let exit: ProcessExit = run_sandboxed(
                &an_agent_connecting_to(broker_port),
                &a_policy_with_broker(broker_port),
            );

            assert_eq!(
                (exit, was_reached(&broker)),
                (ProcessExit::Code(0), true),
                "the broker is the agent's one way out, and it must work"
            );
        }

        /// A Unix socket listener in a fresh canonical directory, polled by the test.
        fn a_unix_listener(name: &str) -> (std::os::unix::net::UnixListener, PathBuf) {
            let directory: PathBuf =
                std::env::temp_dir().join(format!("ksb-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).expect("writable temp dir");
            let socket: PathBuf = directory.canonicalize().expect("exists").join("b.sock");
            let listener =
                std::os::unix::net::UnixListener::bind(&socket).expect("the socket binds");
            listener
                .set_nonblocking(true)
                .expect("the listener can be polled");
            (listener, socket)
        }

        /// An agent that connects to a Unix socket, as Kaisa's relay does.
        fn an_agent_connecting_to_socket(socket: &Path) -> ProcessSpec {
            ProcessSpec {
                program: PathBuf::from("/usr/bin/nc"),
                // `-z`: connect and hang up at once, so the test never waits on the connection.
                args: vec![
                    "-z".to_owned(),
                    "-U".to_owned(),
                    socket.to_string_lossy().into_owned(),
                ],
                working_directory: std::env::temp_dir(),
            }
        }

        #[test]
        fn a_sandboxed_agent_reaches_its_tool_broker_socket_and_no_other() {
            let (broker, broker_socket) = a_unix_listener("own");
            let (elsewhere, elsewhere_socket) = a_unix_listener("other");
            let policy = SeatbeltPolicy {
                broker_socket: Some(broker_socket.clone()),
                ..SeatbeltPolicy::default()
            };

            run_sandboxed(&an_agent_connecting_to_socket(&broker_socket), &policy);
            run_sandboxed(&an_agent_connecting_to_socket(&elsewhere_socket), &policy);
            let reached_own: bool = broker.accept().is_ok();
            let reached_other: bool = elsewhere.accept().is_ok();
            for socket in [&broker_socket, &elsewhere_socket] {
                if let Some(directory) = socket.parent() {
                    let _ = std::fs::remove_dir_all(directory);
                }
            }

            assert!(
                reached_own,
                "the relay inside the sandbox must reach its broker"
            );
            assert!(
                !reached_other,
                "no other local socket is reachable from the sandbox"
            );
        }
    }
}
