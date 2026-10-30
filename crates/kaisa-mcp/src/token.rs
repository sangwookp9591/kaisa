//! The per-attempt capability token: what a bridge presents to its attempt's broker
//! (architecture §5.6). Only the attempt's own processes can read it from the attempt
//! directory, so holding it proves the caller belongs to that attempt.

use std::io;
use std::path::Path;

/// The file in an attempt directory that holds its token.
pub const TOKEN_FILE: &str = "bridge.token";

/// Random bytes in a token.
const TOKEN_BYTES: usize = 32;

/// One attempt's capability token, kept as lowercase hex.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptToken(String);

impl std::fmt::Debug for AttemptToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttemptToken(<redacted>)")
    }
}

impl AttemptToken {
    /// Draws a fresh token and writes it, readable only by this user, into `attempt_directory`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if randomness cannot be read or the file cannot be written.
    pub fn issue(attempt_directory: &Path) -> io::Result<Self> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        kaisa_sys::fill_random(&mut bytes)?;
        let token = Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
        let path = attempt_directory.join(TOKEN_FILE);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, kaisa_sys::PRIVATE_FILE_MODE);
        io::Write::write_all(&mut options.open(path)?, token.0.as_bytes())?;
        Ok(token)
    }

    /// Reads the token from `attempt_directory`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file is missing or unreadable.
    pub fn load(attempt_directory: &Path) -> io::Result<Self> {
        Ok(Self(
            std::fs::read_to_string(attempt_directory.join(TOKEN_FILE))?
                .trim()
                .to_owned(),
        ))
    }

    /// The token as presented on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True if `presented` is this token, compared in time independent of where they differ.
    #[must_use]
    pub fn admits(&self, presented: &str) -> bool {
        let ours: &[u8] = self.0.as_bytes();
        let theirs: &[u8] = presented.as_bytes();
        let mut difference: u8 = u8::from(ours.len() != theirs.len());
        for (index, byte) in ours.iter().enumerate() {
            difference |= byte ^ theirs.get(index).copied().unwrap_or(!byte);
        }
        difference == 0
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AttemptToken, TOKEN_FILE};

    fn an_attempt_directory(name: &str) -> PathBuf {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("kaisa-token-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("writable temp dir");
        directory
    }

    #[test]
    fn a_bridge_with_the_wrong_token_is_refused() {
        let attempt = an_attempt_directory("wrong");
        let issued = AttemptToken::issue(&attempt).expect("issued");
        let other_attempt = an_attempt_directory("other");
        let foreign = AttemptToken::issue(&other_attempt).expect("issued");
        let truncated: &str = &issued.as_str()[1..];

        let admits_foreign: bool = issued.admits(foreign.as_str());
        let admits_truncated: bool = issued.admits(truncated);
        let admits_own: bool = issued.admits(AttemptToken::load(&attempt).expect("read").as_str());
        let _ = std::fs::remove_dir_all(&attempt);
        let _ = std::fs::remove_dir_all(&other_attempt);

        assert!(
            !admits_foreign,
            "another attempt's token opens nothing here"
        );
        assert!(!admits_truncated, "a prefix of the token is not the token");
        assert!(
            admits_own,
            "the token read back from the attempt directory is admitted"
        );
    }

    #[test]
    fn a_token_is_never_issued_twice_into_the_same_attempt() {
        let attempt = an_attempt_directory("twice");
        AttemptToken::issue(&attempt).expect("first issue");

        let second = AttemptToken::issue(&attempt);
        let _ = std::fs::remove_dir_all(&attempt);

        assert!(
            second.is_err(),
            "an existing {TOKEN_FILE} is never overwritten"
        );
    }

    #[test]
    fn a_token_never_appears_in_debug_output() {
        let attempt = an_attempt_directory("debug");
        let token = AttemptToken::issue(&attempt).expect("issued");

        let shown: String = format!("{token:?}");
        let _ = std::fs::remove_dir_all(&attempt);

        assert!(
            !shown.contains(token.as_str()),
            "logs must not leak the capability"
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_a_token_file() {
        use std::os::unix::fs::PermissionsExt;
        let attempt = an_attempt_directory("mode");
        AttemptToken::issue(&attempt).expect("issued");

        let mode: u32 = std::fs::metadata(attempt.join(TOKEN_FILE))
            .expect("exists")
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_dir_all(&attempt);

        assert_eq!(
            mode,
            kaisa_sys::PRIVATE_FILE_MODE,
            "group and others get no access"
        );
    }
}
