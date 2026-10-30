//! macOS Seatbelt profiles (architecture §5.7 containment matrix, write-set confinement).
//!
//! The profile denies everything by default, then allows what an agent needs to run, reads,
//! writes only inside its writable paths, and network only to its egress broker. Paths are
//! embedded as escaped SBPL strings, so no path name can end the string and add rules.

use std::path::PathBuf;

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
    if let Some(port) = policy.broker_port {
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    profile
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
}
