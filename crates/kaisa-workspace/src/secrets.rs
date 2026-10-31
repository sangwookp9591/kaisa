//! A minimal secret scanner run before work is committed (architecture §5.5, step 4).
//!
//! It recognises the credential shapes an agent is most likely to leave in a worktree. A flagged
//! file is never committed; it stays out of the receipt, so reclamation is refused until it is
//! preserved another way. False positives therefore cost a manual step, never a leak.

/// Why a piece of text looks like a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretKind {
    /// A PEM-encoded private key.
    PrivateKey,
    /// An AWS access key id.
    AwsAccessKey,
    /// A standalone AWS secret access key assignment.
    AwsSecretKey,
    /// A live Stripe secret or restricted key.
    StripeKey,
    /// A GitHub personal access or app token.
    GitHubToken,
    /// An Anthropic API key.
    AnthropicKey,
    /// A Slack token.
    SlackToken,
    /// A Google API key.
    GoogleKey,
    /// An OpenAI API key.
    OpenAiKey,
    /// A JSON Web Token.
    Jwt,
    /// The file could not be scanned (too large or unreadable), so it is treated as if it held
    /// a secret rather than committed unseen.
    Unscannable,
}

/// Characters a token body may contain.
fn is_token_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Length of the token body that starts at `start`.
fn token_length(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .take_while(|&&byte| is_token_character(byte))
        .count()
}

/// True if `prefix` occurs in `text` followed by at least `minimum` token characters.
fn has_prefixed_token(text: &str, prefix: &str, minimum: usize) -> bool {
    let bytes: &[u8] = text.as_bytes();
    text.match_indices(prefix)
        .any(|(index, _)| token_length(bytes, index + prefix.len()) >= minimum)
}

/// Shortest body a real GitHub or Anthropic token has after its prefix.
const MIN_TOKEN_BODY: usize = 20;
/// Characters after `AKIA` in an AWS access key id.
const AWS_KEY_BODY: usize = 16;

/// Binary keystores and conventional SSH private-key names cannot be cleared by
/// a text scan. Public keys retain their ordinary preservation behaviour.
pub(crate) fn denied_filename(path: &str) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    [".p12", ".pfx", ".jks", ".keystore", ".tfstate", ".kdbx"]
        .iter()
        .any(|extension| name.ends_with(extension))
        || [".env", ".npmrc", ".netrc"].contains(&name.as_str())
        || name.starts_with(".env.")
        || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .any(|key| {
                name == *key || (name.starts_with(&format!("{key}.")) && !name.ends_with(".pub"))
            })
}

fn has_aws_secret_assignment(text: &str) -> bool {
    text.lines().any(|line| {
        let Some((name, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            return false;
        };
        let name = name.to_ascii_uppercase();
        let value = value.trim().trim_start_matches(['\"', '\'']);
        name.contains("AWS")
            && name.contains("SECRET")
            && value
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'/' || *b == b'+')
                .count()
                > 0
    })
}

/// The kinds of credential found in `text`.
#[must_use]
pub fn scan(text: &str) -> Vec<SecretKind> {
    let mut found: Vec<SecretKind> = Vec::new();
    if text.contains("-----BEGIN")
        && (text.contains("PRIVATE KEY-----") || text.contains("PRIVATE KEY BLOCK-----"))
    {
        found.push(SecretKind::PrivateKey);
    }
    let bytes: &[u8] = text.as_bytes();
    let aws: bool = text.match_indices("AKIA").any(|(index, _)| {
        let body: &[u8] = &bytes[index + "AKIA".len()..];
        body.len() >= AWS_KEY_BODY
            && body[..AWS_KEY_BODY]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    });
    if has_aws_secret_assignment(text) {
        found.push(SecretKind::AwsSecretKey);
    }
    if ["sk_live_", "rk_live_"]
        .iter()
        .any(|prefix| has_prefixed_token(text, prefix, MIN_TOKEN_BODY))
    {
        found.push(SecretKind::StripeKey);
    }
    if aws {
        found.push(SecretKind::AwsAccessKey);
    }
    if ["ghp_", "gho_", "ghs_", "ghu_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| has_prefixed_token(text, prefix, MIN_TOKEN_BODY))
    {
        found.push(SecretKind::GitHubToken);
    }
    if has_prefixed_token(text, "sk-ant-", MIN_TOKEN_BODY) {
        found.push(SecretKind::AnthropicKey);
    }
    if has_prefixed_token(text, "xox", MIN_TOKEN_BODY) {
        found.push(SecretKind::SlackToken);
    }
    if has_prefixed_token(text, "AIza", MIN_TOKEN_BODY) {
        found.push(SecretKind::GoogleKey);
    }
    if has_prefixed_token(text, "sk-", MIN_TOKEN_BODY) {
        found.push(SecretKind::OpenAiKey);
    }
    if text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.')
        .any(|token| {
            let parts: Vec<_> = token.split('.').collect();
            parts.len() == 3
                && parts[0].starts_with("eyJ")
                && parts.iter().all(|part| !part.is_empty())
        })
    {
        found.push(SecretKind::Jwt);
    }
    found
}

/// Scan UTF-8 and UTF-16 in either byte order. BOM-less UTF-16 is common in
/// generated configuration; scanning both orders avoids trusting an encoding guess.
pub(crate) fn scan_bytes(bytes: &[u8]) -> Vec<SecretKind> {
    let mut found = scan(&String::from_utf8_lossy(bytes));
    for little_endian in [true, false] {
        let units = bytes.as_chunks::<2>().0.iter().map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        });
        let decoded = String::from_utf16_lossy(&units.collect::<Vec<_>>());
        for kind in scan(&decoded) {
            if !found.contains(&kind) {
                found.push(kind);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{SecretKind, scan};

    #[test]
    fn ordinary_source_code_is_not_flagged() {
        assert_eq!(
            scan("fn refresh_token(session: &Session) -> Token { session.renew() }\n"),
            Vec::<SecretKind>::new(),
            "code that merely talks about tokens must still be preserved normally"
        );
    }

    #[test]
    fn a_pem_private_key_is_flagged() {
        let key: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";

        assert_eq!(
            scan(key),
            vec![SecretKind::PrivateKey],
            "a private key must never end up in a git commit"
        );
    }

    #[test]
    fn an_api_key_in_a_config_file_is_flagged() {
        let config: String = format!("ANTHROPIC_API_KEY=sk-ant-{}\n", "a1".repeat(24));

        assert_eq!(
            scan(&config),
            vec![SecretKind::AnthropicKey, SecretKind::OpenAiKey],
            "an agent that wrote a key into .env must not get it committed"
        );
    }
    #[test]
    fn common_credential_filenames_and_token_shapes_are_flagged() {
        let filenames = [
            ".env",
            ".env.local",
            "config/.npmrc",
            ".netrc",
            "state.tfstate",
            "vault.kdbx",
        ];
        for path in filenames {
            assert!(
                super::denied_filename(path),
                "credential filename {path} must require manual preservation"
            );
        }
        let tokens = [
            (format!("xoxb-{}", "a".repeat(24)), SecretKind::SlackToken),
            (format!("AIza{}", "a".repeat(35)), SecretKind::GoogleKey),
            (format!("sk-proj-{}", "a".repeat(24)), SecretKind::OpenAiKey),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.signature".to_owned(),
                SecretKind::Jwt,
            ),
            (
                "AWS_SECRET_ACCESS_KEY=shortButSensitive".to_owned(),
                SecretKind::AwsSecretKey,
            ),
            (
                format!("AWS_SECRET_ACCESS_KEY={}", "a".repeat(60)),
                SecretKind::AwsSecretKey,
            ),
        ];
        for (text, kind) in tokens {
            assert!(
                scan(&text).contains(&kind),
                "credential shape {kind:?} must not enter history"
            );
        }
    }
}
