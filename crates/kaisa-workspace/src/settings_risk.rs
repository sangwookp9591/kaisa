//! Repository settings that can expand an agent's authority (architecture §5.7).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Largest settings file inspected, in bytes. Larger files are skipped.
pub const MAX_SETTINGS_FILE_BYTES: usize = 1024 * 1024;
/// Maximum excerpt length in UTF-8 bytes.
pub const MAX_RISK_EXCERPT_BYTES: usize = 240;
/// Bound on JSON nesting, including irrelevant settings values.
const MAX_JSON_DEPTH: usize = 64;

/// An authority expansion declared by repository-owned agent settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum RiskRule {
    /// Claude permissions pre-approve shell commands or wildcard tools.
    ClaudeToolApproval,
    /// Claude's default mode bypasses permission checks.
    ClaudeBypassPermissions,
    /// Claude settings define executable hooks.
    ClaudeHooks,
    /// A repository MCP configuration adds tool servers.
    RepositoryMcpServer,
    /// Codex never asks for approval.
    CodexApprovalNever,
    /// Codex disables sandbox containment.
    CodexDangerFullAccess,
}

/// One rule matched in an agent settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFinding {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// Authority expansion detected in the settings.
    pub rule: RiskRule,
    /// Relevant setting text, bounded by [`MAX_RISK_EXCERPT_BYTES`].
    pub excerpt: String,
}

/// Scans Claude and Codex settings throughout a repository and its root `.mcp.json`.
///
/// Findings are sorted by path, then rule, with one finding per rule per file. Symlinks
/// (including a symlinked repository root), `.git` directories, and files larger than
/// [`MAX_SETTINGS_FILE_BYTES`] are skipped. Filesystem and malformed-JSON errors are
/// returned to the caller rather than treating unreadable settings as safe.
///
/// This checks symlink metadata before opening files; callers must keep the repository
/// quiescent during scanning to prevent concurrent replacements of checked paths.
pub fn scan_settings(repository: &Path) -> io::Result<Vec<RiskFinding>> {
    let mut findings: Vec<RiskFinding> = Vec::new();
    if fs::symlink_metadata(repository)?.file_type().is_symlink() {
        return Ok(findings);
    }
    let mut directories: Vec<PathBuf> = vec![repository.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    directories.push(path);
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let relative = path.strip_prefix(repository).map_err(io::Error::other)?;
            let parent = relative.parent().and_then(Path::file_name);
            let name = relative.file_name();
            let claude = parent.is_some_and(|name| name == ".claude")
                && name
                    .is_some_and(|name| name == "settings.json" || name == "settings.local.json");
            let codex = parent.is_some_and(|name| name == ".codex")
                && name.is_some_and(|name| name == "config.toml");
            let mcp = relative == Path::new(".mcp.json");
            if !claude && !codex && !mcp {
                continue;
            }
            if entry.metadata()?.len() > MAX_SETTINGS_FILE_BYTES as u64 {
                continue;
            }
            // Bound the actual read too, in case the file grows after the metadata check.
            let mut bytes: Vec<u8> = Vec::new();
            File::open(&path)?
                .take(MAX_SETTINGS_FILE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_SETTINGS_FILE_BYTES {
                continue;
            }
            let contents = String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let rules = if codex {
                codex_rules(&contents)
            } else {
                json_rules(&contents, mcp).map_err(|error| {
                    io::Error::new(error.kind(), format!("{}: {error}", relative.display()))
                })?
            };
            let relative = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            for (rule, excerpt) in rules {
                let mut end = excerpt.len().min(MAX_RISK_EXCERPT_BYTES);
                while !excerpt.is_char_boundary(end) {
                    end -= 1;
                }
                findings.push(RiskFinding {
                    path: relative.clone(),
                    rule,
                    excerpt: excerpt[..end].to_owned(),
                });
            }
        }
    }
    findings.sort_by(|left, right| (&left.path, left.rule).cmp(&(&right.path, right.rule)));
    findings.dedup_by(|left, right| left.path == right.path && left.rule == right.rule);
    Ok(findings)
}

fn codex_rules(contents: &str) -> Vec<(RiskRule, String)> {
    let mut rules = Vec::new();
    for line in contents.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        let (rule, dangerous_value) = match key {
            "approval_policy" => (RiskRule::CodexApprovalNever, "never"),
            "sandbox_mode" => (RiskRule::CodexDangerFullAccess, "danger-full-access"),
            _ => continue,
        };
        let value = value.trim();
        // Decode basic string escapes too. Unrecognised forms (including multiline
        // TOML strings) are conservatively flagged instead of hiding a bypass.
        let decoded = if value.starts_with("\"\"\"") || value.starts_with("'''") {
            None
        } else if value.starts_with('"') {
            JsonParser { remaining: value }.string().ok()
        } else if let Some(rest) = value.strip_prefix('\'') {
            rest.split_once('\'').map(|(value, _)| value.to_owned())
        } else {
            None
        };
        if decoded
            .as_deref()
            .is_some_and(|value| value != dangerous_value)
        {
            continue;
        }
        rules.push((rule, line.trim().to_owned()));
    }
    rules
}

/// Only the JSON structure and string values matter to these rules.
enum Json {
    Object(Vec<(String, Json)>),
    Array(Vec<Json>),
    String(String),
    Scalar,
}

impl Json {
    fn entries(&self) -> &[(String, Json)] {
        if let Self::Object(entries) = self {
            entries
        } else {
            &[]
        }
    }

    fn string(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
}

fn json_rules(contents: &str, mcp: bool) -> io::Result<Vec<(RiskRule, String)>> {
    let mut parser = JsonParser {
        remaining: contents,
    };
    let root = parser.value(0)?;
    if !parser.remaining.trim().is_empty() || !matches!(root, Json::Object(_)) {
        return Err(invalid_json());
    }
    let mut rules = Vec::new();
    for (key, value) in root.entries() {
        if mcp {
            if key == "mcpServers" && !value.entries().is_empty() {
                rules.push((RiskRule::RepositoryMcpServer, "mcpServers".to_owned()));
            }
            continue;
        }
        match key.as_str() {
            "hooks" => rules.push((RiskRule::ClaudeHooks, "hooks".to_owned())),
            "defaultMode" if value.string() == Some("bypassPermissions") => rules.push((
                RiskRule::ClaudeBypassPermissions,
                "defaultMode: bypassPermissions".to_owned(),
            )),
            "permissions" => {
                for (key, value) in value.entries() {
                    if key == "defaultMode" && value.string() == Some("bypassPermissions") {
                        rules.push((
                            RiskRule::ClaudeBypassPermissions,
                            "permissions.defaultMode: bypassPermissions".to_owned(),
                        ));
                    }
                    if key == "allow"
                        && let Json::Array(tools) = value
                    {
                        for tool in tools.iter().filter_map(Json::string) {
                            if tool.contains("Bash") || tool.contains('*') {
                                rules.push((
                                    RiskRule::ClaudeToolApproval,
                                    format!("permissions.allow: {tool}"),
                                ));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(rules)
}

fn invalid_json() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or excessively nested settings JSON",
    )
}

/// A bounded JSON reader, implemented locally to keep this crate dependency-free.
struct JsonParser<'a> {
    remaining: &'a str,
}

impl JsonParser<'_> {
    fn consume(&mut self, character: char) -> bool {
        self.remaining = self.remaining.trim_start();
        if let Some(rest) = self.remaining.strip_prefix(character) {
            self.remaining = rest;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> io::Result<Json> {
        if depth > MAX_JSON_DEPTH {
            return Err(invalid_json());
        }
        self.remaining = self.remaining.trim_start();
        if self.remaining.starts_with('"') {
            return self.string().map(Json::String);
        }
        if self.consume('{') {
            let mut entries = Vec::new();
            if self.consume('}') {
                return Ok(Json::Object(entries));
            }
            loop {
                let key = self.string()?;
                if !self.consume(':') {
                    return Err(invalid_json());
                }
                entries.push((key, self.value(depth + 1)?));
                if self.consume('}') {
                    break;
                }
                if !self.consume(',') {
                    return Err(invalid_json());
                }
            }
            return Ok(Json::Object(entries));
        }
        if self.consume('[') {
            let mut entries = Vec::new();
            if self.consume(']') {
                return Ok(Json::Array(entries));
            }
            loop {
                entries.push(self.value(depth + 1)?);
                if self.consume(']') {
                    break;
                }
                if !self.consume(',') {
                    return Err(invalid_json());
                }
            }
            return Ok(Json::Array(entries));
        }
        let end = self
            .remaining
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ']' | '}')
            })
            .unwrap_or(self.remaining.len());
        let scalar = &self.remaining[..end];
        if !matches!(scalar, "true" | "false" | "null")
            && (scalar.is_empty() || scalar.parse::<f64>().is_err())
        {
            return Err(invalid_json());
        }
        self.remaining = &self.remaining[end..];
        Ok(Json::Scalar)
    }

    fn next(&mut self) -> io::Result<char> {
        let character = self.remaining.chars().next().ok_or_else(invalid_json)?;
        self.remaining = &self.remaining[character.len_utf8()..];
        Ok(character)
    }

    fn hex_quad(&mut self) -> io::Result<u32> {
        let mut value = 0;
        for _ in 0..4 {
            value = value * 16 + self.next()?.to_digit(16).ok_or_else(invalid_json)?;
        }
        Ok(value)
    }

    fn string(&mut self) -> io::Result<String> {
        if !self.consume('"') {
            return Err(invalid_json());
        }
        let mut string = String::new();
        loop {
            match self.next()? {
                '"' => return Ok(string),
                '\\' => {
                    let character = match self.next()? {
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'u' => {
                            let mut value = self.hex_quad()?;
                            if (0xd800..=0xdbff).contains(&value) {
                                if self.next()? != '\\' || self.next()? != 'u' {
                                    return Err(invalid_json());
                                }
                                let low = self.hex_quad()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err(invalid_json());
                                }
                                value = 0x10000 + ((value - 0xd800) << 10) + low - 0xdc00;
                            }
                            char::from_u32(value).ok_or_else(invalid_json)?
                        }
                        _ => return Err(invalid_json()),
                    };
                    string.push(character);
                }
                character if character.is_control() => return Err(invalid_json()),
                character => string.push(character),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{MAX_RISK_EXCERPT_BYTES, MAX_SETTINGS_FILE_BYTES, RiskRule, scan_settings};
    use crate::test_support::a_repository_with_one_commit_on_main;

    fn write_settings(repository: &Path, relative: &str, contents: &str, _story: &str) {
        let path = repository.join(relative);
        fs::create_dir_all(path.parent().expect("a fixture has a parent directory"))
            .expect("settings directory is writable");
        fs::write(path, contents).expect("settings fixture is writable");
    }

    #[test]
    fn a_repository_setting_that_widens_agent_authority_is_flagged() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".claude/settings.json",
            r#"{"permissions":{"allow":["Read","Bash(git *)"],"defaultMode":"bypassPermissions"},"hooks":{}}"#,
            "add agent settings",
        );
        let findings = scan_settings(repository.path()).expect("settings can be scanned");
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.rule)
                .collect::<Vec<_>>(),
            vec![
                RiskRule::ClaudeToolApproval,
                RiskRule::ClaudeBypassPermissions,
                RiskRule::ClaudeHooks
            ],
            "each authority expansion needs an independent finding"
        );
        assert!(
            findings
                .iter()
                .all(|finding| finding.path == ".claude/settings.json"),
            "findings must use repository-relative paths"
        );
    }

    #[test]
    fn a_settings_file_that_only_narrows_authority_is_not_flagged() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".claude/settings.local.json",
            r#"{"permissions":{"deny":["Bash","*"],"allow":["Read"],"defaultMode":"plan"},"description":"hooks and bypassPermissions"}"#,
            "restrict agent settings",
        );
        write_settings(
            repository.path(),
            ".codex/config.toml",
            "approval_policy = 'on-request'\nsandbox_mode = 'read-only'\n# approval_policy = 'never'\n",
            "restrict codex settings",
        );
        write_settings(
            repository.path(),
            "AGENTS.md",
            "allow Bash and *; bypassPermissions; hooks",
            "add quoted instructions",
        );
        write_settings(
            repository.path(),
            "CLAUDE.md",
            "approval_policy = 'never'",
            "add more quoted instructions",
        );
        write_settings(
            repository.path(),
            ".mcp.json",
            r#"{"mcpServers":{}}"#,
            "add empty server list",
        );
        assert!(
            scan_settings(repository.path())
                .expect("settings can be scanned")
                .is_empty(),
            "restrictions and instruction documents must not count as authority expansions"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_settings_file_is_never_followed() {
        let repository = a_repository_with_one_commit_on_main();
        let outside = repository.sibling("outside.json");
        fs::write(&outside, r#"{"hooks":{}}"#).expect("outside fixture is writable");
        fs::create_dir(repository.path().join(".claude")).expect("settings directory is writable");
        std::os::unix::fs::symlink(&outside, repository.path().join(".claude/settings.json"))
            .expect("settings symlink can be created");
        std::os::unix::fs::symlink(
            repository.sibling("missing"),
            repository.path().join(".codex"),
        )
        .expect("directory symlink can be created");
        assert!(
            scan_settings(repository.path())
                .expect("symlinks are skipped")
                .is_empty(),
            "neither settings symlinks nor directory symlinks may be followed"
        );
    }

    #[test]
    fn a_repository_mcp_server_is_flagged() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".mcp.json",
            r#"{"mcpServers":{"repository-tools":{"command":"tool-server"}}}"#,
            "add server",
        );
        let findings = scan_settings(repository.path()).expect("settings can be scanned");
        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.path.as_str(), finding.rule))
                .collect::<Vec<_>>(),
            vec![(".mcp.json", RiskRule::RepositoryMcpServer)],
            "a repository server adds tools regardless of its command"
        );
    }

    #[test]
    fn a_codex_config_that_disables_approvals_is_flagged() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".codex/config.toml",
            "approval_policy = \"never\" # unattended\nsandbox_mode = 'danger-full-access'\n",
            "disable codex boundaries",
        );
        let findings = scan_settings(repository.path()).expect("settings can be scanned");
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.rule)
                .collect::<Vec<_>>(),
            vec![
                RiskRule::CodexApprovalNever,
                RiskRule::CodexDangerFullAccess
            ],
            "approval and sandbox bypasses must both be reported"
        );
    }

    #[test]
    fn escaped_and_multiline_codex_values_cannot_hide_an_approval_bypass() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".codex/config.toml",
            r#"approval_policy = "ne\u0076er"
sandbox_mode = """danger-full-access"""
"#,
            "encode codex bypass settings",
        );
        let findings = scan_settings(repository.path()).expect("settings can be scanned");
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.rule)
                .collect::<Vec<_>>(),
            vec![
                RiskRule::CodexApprovalNever,
                RiskRule::CodexDangerFullAccess
            ],
            "escaped values and unsupported TOML string forms must not silently pass"
        );
    }

    #[test]
    fn nested_settings_are_sorted_and_excerpts_are_bounded() {
        let repository = a_repository_with_one_commit_on_main();
        let wildcard = format!("*{}", "界".repeat(MAX_RISK_EXCERPT_BYTES));
        write_settings(
            repository.path(),
            "z/.claude/settings.local.json",
            &format!(r#"{{"permissions":{{"allow":["{wildcard}","Bash"]}},"hooks":{{}}}}"#),
            "add nested settings",
        );
        write_settings(
            repository.path(),
            "a/.claude/settings.json",
            r#"{"permissions":{"allow":["\u0042ash"]}}"#,
            "add escaped tool",
        );
        let findings = scan_settings(repository.path()).expect("settings can be scanned");
        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.path.as_str(), finding.rule))
                .collect::<Vec<_>>(),
            vec![
                ("a/.claude/settings.json", RiskRule::ClaudeToolApproval),
                (
                    "z/.claude/settings.local.json",
                    RiskRule::ClaudeToolApproval
                ),
                ("z/.claude/settings.local.json", RiskRule::ClaudeHooks)
            ],
            "findings must be sorted by path and rule with duplicate matches collapsed"
        );
        assert!(
            findings
                .iter()
                .all(|finding| finding.excerpt.len() <= MAX_RISK_EXCERPT_BYTES),
            "UTF-8 excerpts must fit the documented byte limit"
        );
    }

    #[test]
    fn oversized_settings_are_skipped_and_malformed_json_returns_an_error() {
        let repository = a_repository_with_one_commit_on_main();
        write_settings(
            repository.path(),
            ".claude/settings.json",
            &" ".repeat(MAX_SETTINGS_FILE_BYTES + 1),
            "add oversized settings",
        );
        assert!(
            scan_settings(repository.path())
                .expect("oversized files are skipped")
                .is_empty(),
            "oversized settings must not be read or parsed"
        );
        write_settings(
            repository.path(),
            ".mcp.json",
            "{",
            "add malformed settings",
        );
        assert_eq!(
            scan_settings(repository.path())
                .expect_err("malformed settings cannot pass silently")
                .kind(),
            std::io::ErrorKind::InvalidData,
            "malformed JSON must fail the scan rather than certify safety"
        );
    }
}
