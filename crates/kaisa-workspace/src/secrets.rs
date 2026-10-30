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
    /// A GitHub personal access or app token.
    GitHubToken,
    /// An Anthropic API key.
    AnthropicKey,
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

/// The kinds of credential found in `text`.
#[must_use]
pub fn scan(text: &str) -> Vec<SecretKind> {
    let mut found: Vec<SecretKind> = Vec::new();
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
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
            vec![SecretKind::AnthropicKey],
            "an agent that wrote a key into .env must not get it committed"
        );
    }
}
