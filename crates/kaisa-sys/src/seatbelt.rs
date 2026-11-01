//! macOS Seatbelt profiles (architecture §5.7 containment matrix, write-set confinement).
//!
//! The profile denies everything by default, then allows what an agent needs to run, reads,
//! writes only inside its writable paths, and network only to its egress broker. Paths are
//! embedded as escaped SBPL strings, so no path name can end the string and add rules.

use std::path::{Path, PathBuf};

use crate::process::ProcessSpec;

/// The macOS tool that runs a program under a Seatbelt profile; it execs the program in place,
/// so process-group ownership is unchanged.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// What one attempt's Seatbelt profile allows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeatbeltPolicy {
    /// Directories the agent may not read, except for the explicit `readable` paths.
    /// Production constructors populate this with Kaisa's state directory.
    pub unreadable: Vec<PathBuf>,
    /// Exact files or directory trees needed inside an otherwise unreadable state directory.
    pub readable: Vec<PathBuf>,
    /// Canonical readable paths that existed as directories at policy construction.
    pub readable_directories: Vec<PathBuf>,
    /// Whether this agent needs the login keychain for its own OAuth credential.
    /// Checks and broker exec must leave this false.
    pub needs_keychain_login: bool,
    /// Directories (or files) the agent may change: the write set and the scratch area. Paths
    /// must be absolute and canonical (`/private/var`, not `/var`).
    pub writable: Vec<PathBuf>,
    /// Explicit write denials, including immutable private QA inputs under broad grants.
    pub unwritable: Vec<PathBuf>,
    /// Repository root whose Git controls must stay read-only, even under a root grant.
    pub git_control_root: Option<PathBuf>,
    /// The loopback port of the attempt's egress broker; `None` means no network at all.
    pub broker_port: Option<u16>,
    /// The Unix socket of the attempt's tool broker, the one local endpoint the agent may
    /// connect to. Must be canonical, like `writable`.
    pub broker_socket: Option<PathBuf>,
}

impl SeatbeltPolicy {
    /// Protects both the caller's actual state directory and the configured per-user state.
    /// Only `readable` paths may be read within them; use exact bridge files, never an
    /// attempt directory, so logs and future secrets are not accidentally exposed.
    ///
    /// # Errors
    /// Returns an error if the configured state location cannot be determined.
    pub fn protect_state(&mut self, state: &Path) -> std::io::Result<()> {
        let configured = crate::state_location(|name| std::env::var_os(name))?.directory;
        for directory in [state, configured.as_path()] {
            let directory = canonical_path(&std::path::absolute(directory)?);
            if !self.unreadable.contains(&directory) {
                self.unreadable.push(directory);
            }
        }
        self.readable = self
            .readable
            .iter()
            .map(|path| Ok(canonical_path(&std::path::absolute(path)?)))
            .collect::<std::io::Result<_>>()?;
        self.readable_directories = self
            .readable
            .iter()
            .filter(|path| path.is_dir())
            .cloned()
            .collect();
        Ok(())
    }
}

/// Resolve existing ancestors as well: a not-yet-created state path still needs canonical
/// spelling on macOS, where `/var` and `/tmp` are symlinks.
fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_path(parent).join(name),
        _ => path.to_owned(),
    }
}

/// Include the configured home as well as the account database home.
fn keychain_homes() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = std::env::var_os("HOME")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    #[cfg(unix)]
    homes.extend(passwd_home());
    homes.sort();
    homes.dedup();
    homes
}

#[cfg(unix)]
#[allow(unsafe_code)] // Account lookup belongs at the OS boundary.
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 1024];
    loop {
        // SAFETY: getpwuid_r receives an output passwd, a live writable buffer and result
        // pointer. We inspect the entry only on success, and copy pw_dir before dropping
        // the buffer that owns its string. getuid has no preconditions.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE {
            buffer.resize(buffer.len().checked_mul(2)?, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: successful getpwuid_r initialized entry and its NUL-terminated pw_dir
        // points into buffer, which remains alive while the path is copied.
        return unsafe {
            let home = entry.assume_init().pw_dir;
            (!home.is_null()).then(|| {
                PathBuf::from(std::ffi::OsStr::from_bytes(
                    std::ffi::CStr::from_ptr(home).to_bytes(),
                ))
            })
        };
    }
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
    for path in &policy.writable {
        profile.push_str(&format!(
            "(allow file-write* (subpath {}))\n",
            sbpl_string(&path.to_string_lossy())
        ));
    }
    for path in &policy.unwritable {
        profile.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            sbpl_string(&path.to_string_lossy())
        ));
    }
    // Keep the denials after the broad read/write allowances. Exceptions are filters on
    // the denial. The later ancestor allowances grant metadata only, never file data.
    for path in &policy.unreadable {
        profile.push_str(&format!(
            "(deny file-read* (require-all (subpath {})",
            sbpl_string(&path.to_string_lossy())
        ));
        for readable in &policy.readable {
            let filter = if policy.readable_directories.contains(readable) {
                "subpath"
            } else {
                "literal"
            };
            profile.push_str(&format!(
                " (require-not ({filter} {}))",
                sbpl_string(&readable.to_string_lossy())
            ));
        }
        profile.push_str("))\n");
    }
    // Path-walking runtimes need ancestor metadata, but never directory listings or data.
    for root in &policy.unreadable {
        for readable in &policy.readable {
            for ancestor in readable
                .ancestors()
                .skip(1)
                .take_while(|path| path.starts_with(root))
            {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    sbpl_string(&ancestor.to_string_lossy())
                ));
            }
        }
    }
    if !policy.needs_keychain_login {
        profile.push_str("(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.SecurityServer\") (global-name-regex \"^com[.]apple[.]security(d)?[.]\"))\n");
        for home in keychain_homes() {
            let keychains = canonical_path(&home.join("Library/Keychains"));
            profile.push_str(&format!(
                "(deny file-read* file-write* (subpath {}))\n",
                sbpl_string(&keychains.to_string_lossy())
            ));
        }
    }
    if let Some(root) = &policy.git_control_root {
        let root = regex_literal(root.to_string_lossy().trim_end_matches('/'));
        let controls = "[.][gG][iI][tT]([aA][tT][tT][rR][iI][bB][uU][tT][eE][sS]|[mM][oO][dD][uU][lL][eE][sS])?";
        let pattern = format!("^{root}/{controls}(/|$)");
        // Use a normal SBPL string: it safely quotes roots containing quotes or
        // backslashes. The regex filter compiles the decoded string as its pattern.
        profile.push_str(&format!(
            "(deny file-write* (regex {}))\n",
            sbpl_string(&pattern)
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
        env: spec.env.clone(),
    }
}

/// Escape a canonical root as literal text in the anchored control-path regex.
fn regex_literal(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        if ".^$*+?()[]{}|\\".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
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
                env: Vec::new(),
            }
        }

        fn run_sandboxed(spec: &ProcessSpec, policy: &SeatbeltPolicy) -> ProcessExit {
            OwnedProcess::spawn(&sandboxed_spec(spec, policy))
                .expect("sandbox-exec is part of macOS")
                .wait()
                .expect("the agent exits")
        }

        /// A read-only agent command; arguments avoid shell quoting of fixture paths.
        fn an_agent_reading(path: &Path) -> ProcessSpec {
            ProcessSpec {
                program: "/bin/cat".into(),
                args: vec![path.to_string_lossy().into_owned()],
                working_directory: std::env::temp_dir(),
                env: Vec::new(),
            }
        }

        #[test]
        fn an_unreadable_state_directory_hides_secrets_but_keeps_exact_bridge_files_readable() {
            let worktree = ScratchWorktree::new("state-reads");
            let state = worktree.path("state");
            let own = state.join("attempts/own");
            let other = state.join("attempts/other");
            for directory in [&own, &other] {
                std::fs::create_dir_all(directory).expect("attempt directory");
            }
            let own_token = own.join("bridge.token");
            let secrets = [
                state.join("kaisa.db"),
                other.join("bridge.token"),
                own.join("private.log"),
            ];
            for file in secrets.iter().chain([&own_token]) {
                std::fs::write(file, "planted secret").expect("fixture secret");
            }
            let mut policy = SeatbeltPolicy {
                readable: vec![own_token.clone()],
                ..SeatbeltPolicy::default()
            };
            policy.protect_state(&state).expect("state location");
            for secret in &secrets {
                assert_ne!(
                    run_sandboxed(&an_agent_reading(secret), &policy),
                    ProcessExit::Code(0),
                    "state secrets must be denied: {secret:?}"
                );
            }
            for readable in [&own_token, Path::new("/etc/hosts")] {
                assert_eq!(
                    run_sandboxed(&an_agent_reading(readable), &policy),
                    ProcessExit::Code(0),
                    "the bridge token and ordinary system files remain readable"
                );
            }
        }

        #[test]
        fn readable_bridge_ancestors_allow_metadata_but_deny_directory_listings() {
            const SUCCESS: ProcessExit = ProcessExit::Code(0);
            let worktree = ScratchWorktree::new("ancestor-listing");
            let state = worktree.path("state");
            let attempt = state.join("attempts/own");
            std::fs::create_dir_all(&attempt).expect("attempt directory");
            let token = attempt.join("bridge.token");
            std::fs::write(&token, "fixture").expect("bridge token");
            let mut policy = SeatbeltPolicy {
                readable: vec![token],
                ..SeatbeltPolicy::default()
            };
            policy.protect_state(&state).expect("protected state");
            for ancestor in [&state, &state.join("attempts"), &attempt] {
                let listing = ProcessSpec {
                    program: "/bin/ls".into(),
                    args: vec![ancestor.to_string_lossy().into_owned()],
                    working_directory: worktree.root.clone(),
                    env: Vec::new(),
                };
                assert_ne!(
                    run_sandboxed(&listing, &policy),
                    SUCCESS,
                    "ancestor metadata must not grant directory listing of {ancestor:?}"
                );
            }
        }

        #[test]
        fn an_overridden_home_still_denies_the_account_keychain_directory() {
            const PROBE: &str = "KAISA_KEYCHAIN_HOME_PROBE";
            if std::env::var_os(PROBE).is_some() {
                let home = super::super::passwd_home().expect("account home");
                let keychains = super::super::canonical_path(&home.join("Library/Keychains"));
                let profile = super::super::seatbelt_profile(&SeatbeltPolicy::default());
                let denial = format!(
                    "(deny file-read* file-write* (subpath {}))",
                    super::super::sbpl_string(&keychains.to_string_lossy())
                );
                assert!(
                    profile.contains(&denial),
                    "an overridden HOME must not conceal the account keychain denial"
                );
                return;
            }
            let result = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "seatbelt::tests::enforced::an_overridden_home_still_denies_the_account_keychain_directory", "--nocapture"])
                .env(PROBE, "1").env("HOME", "/tmp/kaisa-fictional-login-home")
                .output().expect("isolated HOME probe");
            assert!(
                result.status.success(),
                "the account keychain must remain denied: {}",
                String::from_utf8_lossy(&result.stdout)
            );
        }

        /// Run the native Keychain query inside a sandboxed copy of this test executable.
        /// Unlike `security`, SecItemCopyMatching preserves the service denial error instead
        /// of falling back to a legacy search and reporting the misleading item-not-found.
        #[test]
        fn a_confined_keychain_query_is_denied_before_item_lookup() {
            const PROBE: &str = "KAISA_KEYCHAIN_SANDBOX_PROBE";
            if std::env::var_os(PROBE).is_some() {
                const BOOTSTRAP_NOT_PRIVILEGED: i32 = 1100;
                for service in [c"com.apple.SecurityServer", c"com.apple.securityd.xpc"] {
                    assert_eq!(
                        security_service_lookup_status(service),
                        BOOTSTRAP_NOT_PRIVILEGED,
                        "Seatbelt must deny the real keychain service {service:?}"
                    );
                }
                const ITEM_NOT_FOUND: i32 = -25300;
                const QUERY_SUCCEEDED: i32 = 0;
                for data_protection in [false, true] {
                    let status = nonexistent_keychain_item_status(data_protection);
                    assert_ne!(
                        status, ITEM_NOT_FOUND,
                        "the query must fail at the sandbox boundary (data protection: {data_protection})"
                    );
                    assert_ne!(
                        status, QUERY_SUCCEEDED,
                        "a nonexistent item cannot be returned"
                    );
                }
                return;
            }
            let spec = ProcessSpec {
                program: std::env::current_exe().expect("test executable"),
                args: vec!["--exact".into(),
                    "seatbelt::tests::enforced::a_confined_keychain_query_is_denied_before_item_lookup".into(),
                    "--nocapture".into()],
                working_directory: std::env::temp_dir(),
                env: vec![(PROBE.into(), "1".into())],
            };
            assert_eq!(
                run_sandboxed(&spec, &SeatbeltPolicy::default()),
                ProcessExit::Code(0),
                "the native query must report service denial instead of absence"
            );
        }

        #[test]
        fn a_login_keychain_policy_keeps_real_keychain_services_reachable() {
            const SUCCESS: ProcessExit = ProcessExit::Code(0);
            const PROBE: &str = "KAISA_LOGIN_KEYCHAIN_PROBE";
            if std::env::var_os(PROBE).is_some() {
                const LOOKUP_SUCCEEDED: i32 = 0;
                for service in [c"com.apple.SecurityServer", c"com.apple.securityd.xpc"] {
                    assert_eq!(
                        security_service_lookup_status(service),
                        LOOKUP_SUCCEEDED,
                        "Claude's declared login capability must retain {service:?}"
                    );
                }
                return;
            }
            let spec = ProcessSpec {
                program: std::env::current_exe().expect("test executable"),
                args: vec!["--exact".into(), "seatbelt::tests::enforced::a_login_keychain_policy_keeps_real_keychain_services_reachable".into(), "--nocapture".into()],
                working_directory: std::env::temp_dir(),
                env: vec![(PROBE.into(), "1".into())],
            };
            let policy = SeatbeltPolicy {
                needs_keychain_login: true,
                ..SeatbeltPolicy::default()
            };
            assert_eq!(
                run_sandboxed(&spec, &policy),
                SUCCESS,
                "the agent-only login policy must retain keychain service access"
            );
        }

        #[allow(unsafe_code)] // Native Mach lookup probe stays within the OS-boundary crate.
        fn security_service_lookup_status(service: &std::ffi::CStr) -> i32 {
            // SAFETY: declarations match bootstrap.h; the immutable bootstrap port belongs
            // to this process and the output port points to initialized writable storage.
            unsafe extern "C" {
                static bootstrap_port: u32;
                fn bootstrap_look_up(
                    bootstrap: u32,
                    service: *const std::ffi::c_char,
                    port: *mut u32,
                ) -> i32;
            }
            let mut port = 0;
            // SAFETY: a NUL-terminated static name and a valid output pointer are supplied.
            unsafe { bootstrap_look_up(bootstrap_port, service.as_ptr(), &mut port) }
        }

        #[allow(unsafe_code)] // Native Keychain probe stays within the OS-boundary crate.
        fn nonexistent_keychain_item_status(data_protection: bool) -> i32 {
            use std::ffi::c_void;
            // SAFETY: declarations match Security.framework's SecItem.h.
            #[link(name = "Security", kind = "framework")]
            unsafe extern "C" {
                static kSecClass: *const c_void;
                static kSecClassGenericPassword: *const c_void;
                static kSecAttrService: *const c_void;
                static kSecUseDataProtectionKeychain: *const c_void;
                fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> i32;
            }
            // SAFETY: declarations match CoreFoundation's creation/release functions.
            #[link(name = "CoreFoundation", kind = "framework")]
            unsafe extern "C" {
                static kCFBooleanTrue: *const c_void;
                fn CFStringCreateWithCString(
                    allocator: *const c_void,
                    text: *const std::ffi::c_char,
                    encoding: u32,
                ) -> *const c_void;
                fn CFDictionaryCreate(
                    allocator: *const c_void,
                    keys: *const *const c_void,
                    values: *const *const c_void,
                    count: isize,
                    key_callbacks: *const c_void,
                    value_callbacks: *const c_void,
                ) -> *const c_void;
                fn CFRelease(value: *const c_void);
            }
            const UTF8: u32 = 0x0800_0100;
            // SAFETY: framework constants are immutable CF objects. Keys/values and the service
            // string remain alive throughout the query; both owned CF objects are released.
            unsafe {
                let service = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"kaisa-f2-nonexistent-sandbox-regression-20261031".as_ptr(),
                    UTF8,
                );
                assert!(!service.is_null(), "the service string must allocate");
                let mut keys = vec![kSecClass, kSecAttrService];
                let mut values = vec![kSecClassGenericPassword, service];
                if data_protection {
                    keys.push(kSecUseDataProtectionKeychain);
                    values.push(kCFBooleanTrue);
                }
                let query = CFDictionaryCreate(
                    std::ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    isize::try_from(keys.len()).expect("small dictionary"),
                    std::ptr::null(),
                    std::ptr::null(),
                );
                assert!(!query.is_null(), "the query must allocate");
                let status = SecItemCopyMatching(query, std::ptr::null_mut());
                CFRelease(query);
                CFRelease(service);
                status
            }
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
                env: Vec::new(),
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
                env: Vec::new(),
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
