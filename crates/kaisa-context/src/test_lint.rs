//! Deterministic domain-test lint and criterion coverage (design `spec-driven-workflow.md` §4).
//!
//! No model is involved: the same sources, criteria and manifest always give the same findings.
//! The lint is lexical and says so — it reads comments and string interiors as blank, finds test
//! cases by their declaration syntax, and judges each case by a small rule set that a reader can
//! check by eye.
//!
//! Languages: Rust (`#[test]`, `#[tokio::test]`, any `…::test` attribute; unit and `tests/` files)
//! and Node (`node:test` `test()`/`it()` including `.skip`/`.todo`/`.only`; assertions through
//! `node:assert` — `assert()`, `assert.equal`, `assert.strict.*`, `t.assert.*` …).
//!
//! # Rules
//!
//! | Rule | Default | Fires when |
//! |---|---|---|
//! | `generic-name` | blocking | After dropping leading filler words (`test`, `it`, `should`, `can`, `does`, `must`) nothing but filler remains: `test_case_1`, `test2`, `should_work`, `it works`, `basic`, `works_correctly`. |
//! | `short-name` | advisory | Fewer than [`LintPolicy::min_name_words`] words (default 3) after dropping a leading `test`/`it`. |
//! | `no-outcome-verb` | advisory | No word of the name states an outcome (a verb from a fixed list such as *returns, rejects, keeps*, a state word such as *is, not, never*, or a past participle ending in `ed`). |
//! | `unexplained-literal` | advisory | An assertion compares directly against a number other than 0/1 or a non-empty string literal. A literal bound to a named constant or fixture is an identifier there, so it passes; literals that are only inputs of a call (`parse("42")`) pass too. |
//! | `no-assertion` | blocking | The case asserts nothing: no assertion macro/call, no assertion-named helper, no `#[should_panic]`. |
//! | `trivial-assertion` | blocking | `assert!(true)`, `assert_eq!(a, a)`, `assert!(a == a)`, `assert.ok(true)`, `assert.equal(a, a)`. |
//! | `truthiness-only` | advisory | The case is mapped to a value-bearing (non-`Error`) criterion yet every assertion only tests truthiness (`assert!(x)`, `.is_some()`, `assert.ok(x)`). Needs the mapping; unmapped cases are never judged. |
//! | `missing-error-assertion` | blocking | The case is mapped to a criterion of kind `Error` yet never asserts an error (`#[should_panic]`, `is_err`, `unwrap_err`, `Err(…)`, `assert.throws`/`rejects`, `catch` with an assertion). |
//! | `unknown-test` | blocking | The manifest names a test that no scanned file declares. |
//!
//! Severities are configurable per rule through [`LintPolicy`], including switching a rule off.
//!
//! # Limits (labelled, not hidden)
//!
//! Lexical analysis only: macros that generate tests, regex literals containing quotes, and
//! assertions hidden inside helper functions are not seen through. A helper whose name starts with
//! `assert`/`verify`/`expect`/`check` counts as an assertion without being inspected.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The language of a test file, decided by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestLanguage {
    /// `.rs`.
    Rust,
    /// `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx`.
    Node,
}

impl TestLanguage {
    /// The language of `path`, or `None` if the lint does not read it.
    #[must_use]
    pub fn of_path(path: &str) -> Option<Self> {
        match path.rsplit('.').next()? {
            "rs" => Some(Self::Rust),
            "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => Some(Self::Node),
            _ => None,
        }
    }
}

/// One test file to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Repository-relative path, `/`-separated.
    pub path: String,
    /// The file's text.
    pub source: String,
}

/// What kind of behaviour an acceptance criterion states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriterionKind {
    /// The ordinary path.
    Normal,
    /// A boundary or unusual input.
    Edge,
    /// A failure the system must report.
    Error,
    /// A property that must always hold.
    Invariant,
}

/// A criterion as the lint needs it: identity and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionRef {
    /// For example `AC-1`.
    pub id: String,
    /// What the criterion states.
    pub kind: CriterionKind,
}

/// One manifest row: which criteria a test is written for.
///
/// `test_id` is the Rust function name or the Node test title, optionally prefixed with
/// `<path>::` to tell equal names in different files apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// The test.
    pub test_id: String,
    /// The criteria it covers.
    pub criterion_ids: Vec<String>,
}

/// A lint rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LintRule {
    /// Name carries no information.
    GenericName,
    /// Name has too few words.
    ShortName,
    /// Name states no outcome.
    NoOutcomeVerb,
    /// Assertion compares against a bare literal.
    UnexplainedLiteral,
    /// The case asserts nothing.
    NoAssertion,
    /// The assertion cannot fail.
    TrivialAssertion,
    /// Only truthiness is asserted although a value is specified.
    TruthinessOnly,
    /// An `Error` criterion has no error assertion.
    MissingErrorAssertion,
    /// The manifest names a test that does not exist.
    UnknownTest,
}

impl LintRule {
    /// Every rule, in report order.
    pub const ALL: [Self; 9] = [
        Self::GenericName,
        Self::ShortName,
        Self::NoOutcomeVerb,
        Self::UnexplainedLiteral,
        Self::NoAssertion,
        Self::TrivialAssertion,
        Self::TruthinessOnly,
        Self::MissingErrorAssertion,
        Self::UnknownTest,
    ];

    /// The stable kebab-case code used in reports.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::GenericName => "generic-name",
            Self::ShortName => "short-name",
            Self::NoOutcomeVerb => "no-outcome-verb",
            Self::UnexplainedLiteral => "unexplained-literal",
            Self::NoAssertion => "no-assertion",
            Self::TrivialAssertion => "trivial-assertion",
            Self::TruthinessOnly => "truthiness-only",
            Self::MissingErrorAssertion => "missing-error-assertion",
            Self::UnknownTest => "unknown-test",
        }
    }

    fn default_setting(self) -> RuleSetting {
        match self {
            Self::GenericName
            | Self::NoAssertion
            | Self::TrivialAssertion
            | Self::MissingErrorAssertion
            | Self::UnknownTest => RuleSetting::Blocking,
            Self::ShortName
            | Self::NoOutcomeVerb
            | Self::UnexplainedLiteral
            | Self::TruthinessOnly => RuleSetting::Advisory,
        }
    }
}

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LintSeverity {
    /// The domain test review cannot pass with it.
    Blocking,
    /// Reported, does not stop delivery.
    Advisory,
}

/// What a policy does with a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSetting {
    /// Findings of the rule block.
    Blocking,
    /// Findings of the rule are advice.
    Advisory,
    /// The rule is not applied.
    Off,
}

/// Per-rule severities and the name-length threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintPolicy {
    settings: BTreeMap<LintRule, RuleSetting>,
    /// Names with fewer words than this are `short-name`.
    pub min_name_words: usize,
}

/// The default minimum number of words in a test name.
pub const DEFAULT_MIN_NAME_WORDS: usize = 3;

impl Default for LintPolicy {
    fn default() -> Self {
        Self {
            settings: LintRule::ALL
                .iter()
                .map(|rule| (*rule, rule.default_setting()))
                .collect(),
            min_name_words: DEFAULT_MIN_NAME_WORDS,
        }
    }
}

impl LintPolicy {
    /// This policy with `rule` set to `setting`.
    #[must_use]
    pub fn with(mut self, rule: LintRule, setting: RuleSetting) -> Self {
        self.settings.insert(rule, setting);
        self
    }

    /// What the policy does with `rule`.
    #[must_use]
    pub fn setting(&self, rule: LintRule) -> RuleSetting {
        self.settings
            .get(&rule)
            .copied()
            .unwrap_or_else(|| rule.default_setting())
    }
}

/// One problem in one test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    /// Which rule fired.
    pub rule: LintRule,
    /// Its severity under the policy.
    pub severity: LintSeverity,
    /// The file (`<manifest>` for `unknown-test`).
    pub path: String,
    /// 1-based line (0 for `unknown-test`).
    pub line: u32,
    /// The test's name or title.
    pub test: String,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl fmt::Display for LintFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            LintSeverity::Blocking => "blocking",
            LintSeverity::Advisory => "advisory",
        };
        write!(
            f,
            "{}:{}: {severity} {} [{}]: {}",
            self.path,
            self.line,
            self.rule.code(),
            self.test,
            self.message
        )
    }
}

/// Whether any finding blocks.
#[must_use]
pub fn has_blocking(findings: &[LintFinding]) -> bool {
    findings
        .iter()
        .any(|finding| finding.severity == LintSeverity::Blocking)
}

// ---------------------------------------------------------------------------------------------
// Lexing: blank comments and string interiors, keep every byte offset and line.
// ---------------------------------------------------------------------------------------------

/// `source` with comments blanked and string interiors blanked (quotes kept), same length.
fn mask(source: &str, language: TestLanguage) -> String {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut out, start, i);
        } else if b[i..].starts_with(b"/*") {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") && language == TestLanguage::Rust {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start, i);
        } else if language == TestLanguage::Rust
            && b[i] == b'r'
            && (i == 0 || !is_ident_byte(b[i - 1]))
            && raw_string_open(b, i).is_some()
        {
            let (quote, hashes) = raw_string_open(b, i).unwrap_or((i, 0));
            let mut end = quote + 1;
            while end < b.len()
                && !(b[end] == b'"'
                    && b[end + 1..].iter().take(hashes).all(|c| *c == b'#')
                    && b.len() - end > hashes)
            {
                end += 1;
            }
            blank(&mut out, quote + 1, end.min(b.len()));
            i = (end + 1 + hashes).min(b.len());
        } else if b[i] == b'"'
            || (language == TestLanguage::Node && matches!(b[i], b'\'' | b'`'))
            || (b[i] == b'\'' && is_rust_char_literal(b, i))
        {
            let quote = b[i];
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != quote {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            let end = i.min(b.len());
            blank(&mut out, start.min(end), end);
            i = (end + 1).min(b.len());
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| " ".repeat(source.len()))
}

fn raw_string_open(b: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut quote = at + 1;
    while quote < b.len() && b[quote] == b'#' {
        quote += 1;
    }
    (quote < b.len() && b[quote] == b'"').then_some((quote, quote - at - 1))
}

fn is_rust_char_literal(b: &[u8], at: usize) -> bool {
    b.get(at + 1) == Some(&b'\\') || b.get(at + 2) == Some(&b'\'')
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn line_of(source: &str, offset: usize) -> u32 {
    let newlines = source.as_bytes()[..offset.min(source.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

/// Index of the bracket that closes the one at `open` (any bracket kind), in masked text.
fn matching(masked: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, byte) in masked.get(open..)?.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Top-level comma split of `range` of the text: `(original, masked)` pairs, trimmed.
fn split_args(original: &str, masked: &str, from: usize, to: usize) -> Vec<(String, String)> {
    let m = masked.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0_i32;
    let mut start = from;
    for (index, byte) in m.iter().enumerate().take(to).skip(from) {
        match *byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                args.push((start, index));
                start = index + 1;
            }
            _ => {}
        }
    }
    args.push((start, to));
    args.into_iter()
        .map(|(a, z)| {
            (
                original[a..z].trim().to_owned(),
                masked[a..z].trim().to_owned(),
            )
        })
        .filter(|(arg, _)| !arg.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Test discovery.
// ---------------------------------------------------------------------------------------------

struct TestCase {
    name: String,
    path: String,
    line: u32,
    language: TestLanguage,
    should_panic: bool,
    /// Subtests declared inside this case; they are cases of their own and are blanked out of
    /// this case's body.
    has_children: bool,
    /// Body text and its masked twin (same length), plus the file offset of the body's start.
    body: String,
    masked: String,
    file_source: String,
    body_offset: usize,
}

fn discover(file: &SourceFile) -> Vec<TestCase> {
    match TestLanguage::of_path(&file.path) {
        Some(TestLanguage::Rust) => discover_rust(file),
        Some(TestLanguage::Node) => discover_node(file),
        None => Vec::new(),
    }
}

fn discover_rust(file: &SourceFile) -> Vec<TestCase> {
    let masked = mask(&file.source, TestLanguage::Rust);
    let m = masked.as_bytes();
    let mut cases = Vec::new();
    let mut i = 0;
    while let Some(found) = masked[i..].find("#[") {
        let mut at = i + found;
        let mut heads: Vec<String> = Vec::new();
        // One run of consecutive attributes belongs to one item.
        loop {
            let Some(close) = matching(m, at + 1) else {
                at = m.len();
                break;
            };
            let text = masked[at + 2..close].trim();
            heads.push(text.split('(').next().unwrap_or_default().trim().to_owned());
            let mut next = close + 1;
            while next < m.len() && m[next].is_ascii_whitespace() {
                next += 1;
            }
            if masked[next..].starts_with("#[") {
                at = next;
            } else {
                at = next;
                break;
            }
        }
        i = at.max(i + 2);
        if !heads
            .iter()
            .any(|head| head == "test" || head.ends_with("::test"))
        {
            continue;
        }
        // `… fn name(…) … { body }`: nothing but modifiers may precede `fn`.
        let rest = &masked[at.min(masked.len())..];
        let Some(fn_at) = rest.find("fn ") else {
            continue;
        };
        if rest[..fn_at].contains(['{', ';', '(']) {
            continue;
        }
        let name_start = at + fn_at + 3;
        let name: String = masked[name_start..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some(open) = masked[name_start..].find('{').map(|o| name_start + o) else {
            continue;
        };
        if masked[name_start..open].contains(';') {
            continue;
        }
        let Some(close) = matching(m, open) else {
            continue;
        };
        cases.push(TestCase {
            line: line_of(&file.source, name_start),
            name,
            path: file.path.clone(),
            language: TestLanguage::Rust,
            should_panic: heads.iter().any(|head| head == "should_panic"),
            has_children: false,
            body: file.source[open + 1..close].to_owned(),
            masked: masked[open + 1..close].to_owned(),
            file_source: file.source.clone(),
            body_offset: open + 1,
        });
        i = close;
    }
    cases
}

fn discover_node(file: &SourceFile) -> Vec<TestCase> {
    let masked = mask(&file.source, TestLanguage::Node);
    let mut cases = Vec::new();
    node_cases_within(file, &masked, 0, masked.len(), false, &mut cases);
    cases
}

/// Blanks `ranges` (relative to the text) with spaces, keeping newlines and every offset.
fn blank_ranges(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for (from, to) in ranges {
        for byte in &mut bytes[(*from).min(text.len())..(*to).min(text.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| " ".repeat(text.len()))
}

/// Appends the test cases declared in `from..to` of the masked text and returns the byte range of
/// each outermost call found. Cases inside a case (`t.test(…)`, `it(…)` in a test) are cases of
/// their own, appended after their parent, and are blanked out of the parent's body so the parent
/// is judged on what it asserts itself.
fn node_cases_within(
    file: &SourceFile,
    masked: &str,
    from: usize,
    to: usize,
    nested: bool,
    cases: &mut Vec<TestCase>,
) -> Vec<(usize, usize)> {
    let m = masked.as_bytes();
    let mut found = Vec::new();
    let mut i = from;
    while i < to {
        let after_dot = i > 0 && m[i - 1] == b'.';
        if !is_ident_byte(m[i]) || (i > 0 && is_ident_byte(m[i - 1])) || (after_dot && !nested) {
            i += 1;
            continue;
        }
        let word_end = i + m[i..]
            .iter()
            .take_while(|byte| is_ident_byte(**byte))
            .count();
        let word = &masked[i..word_end];
        if !matches!(word, "test" | "it") {
            i = word_end;
            continue;
        }
        let mut after = word_end;
        for variant in [".skip", ".todo", ".only"] {
            if masked[after..].starts_with(variant) {
                after += variant.len();
                break;
            }
        }
        while after < m.len() && m[after].is_ascii_whitespace() {
            after += 1;
        }
        if m.get(after) != Some(&b'(') {
            i = word_end;
            continue;
        }
        let Some(close) = matching(m, after) else {
            i = word_end;
            continue;
        };
        let mut arg = after + 1;
        while arg < close && m[arg].is_ascii_whitespace() {
            arg += 1;
        }
        let named = matches!(m.get(arg), Some(b'\'' | b'"' | b'`'));
        let (name, body_from) = if named {
            let quote = char::from(m[arg]);
            let end = masked[arg + 1..close]
                .find(quote)
                .map_or(close, |o| arg + 1 + o);
            (file.source[arg + 1..end].to_owned(), (end + 1).min(close))
        } else {
            (String::new(), arg)
        };
        let body = &masked[body_from..close];
        // `pattern.test("text")` is a regular expression, not a subtest: a subtest has a body.
        if after_dot && !(named && (body.contains("=>") || body.contains("function"))) {
            i = word_end;
            continue;
        }
        let index = cases.len();
        cases.push(TestCase {
            line: line_of(&file.source, i),
            name,
            path: file.path.clone(),
            language: TestLanguage::Node,
            should_panic: false,
            has_children: false,
            body: file.source[body_from..close].to_owned(),
            masked: body.to_owned(),
            file_source: file.source.clone(),
            body_offset: body_from,
        });
        let children = node_cases_within(file, masked, body_from, close, true, cases);
        if !children.is_empty() {
            let relative: Vec<(usize, usize)> = children
                .iter()
                .map(|(start, end)| (start - body_from, end - body_from))
                .collect();
            cases[index].body = blank_ranges(&cases[index].body, &relative);
            cases[index].masked = blank_ranges(&cases[index].masked, &relative);
            cases[index].has_children = true;
        }
        found.push((i, close + 1));
        i = close + 1;
    }
    found
}

// ---------------------------------------------------------------------------------------------
// Assertions.
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Assertion {
    line: u32,
    /// `assert!(true)`, `assert_eq!(a, a)` …
    trivial: bool,
    /// Only truthiness is tested.
    truthy_only: bool,
    /// An error is asserted.
    error: bool,
    /// Bare literals compared directly.
    literals: Vec<String>,
}

#[derive(Default)]
struct Assertions {
    direct: Vec<Assertion>,
    /// Calls to assertion-named helpers; their content is not inspected.
    helper_calls: usize,
    /// An error is asserted without an assertion macro/call of its own: `unwrap_err()` and
    /// `expect_err()` panic when there is no error; a Node `catch` block that asserts, or an
    /// `expect(…).rejects` chain. A discarded `is_err()` is not this.
    error_checked: bool,
    /// A local helper that asserts an error is called.
    helper_error: bool,
}

const COMPARATORS: &[&str] = &["===", "!==", "==", "!=", "<=", ">=", "<", ">"];

/// Functions declared in the same file whose bodies assert: name → whether they assert an error.
type LocalHelpers = BTreeMap<String, bool>;

fn word_at(masked: &str, at: usize) -> &str {
    let end = at
        + masked.as_bytes()[at..]
            .iter()
            .take_while(|byte| is_ident_byte(**byte))
            .count();
    &masked[at..end]
}

/// Body ranges of the functions a file declares, by name: Rust `fn`, Node `function` and
/// `const name = (…) => { … }`.
fn function_bodies(masked: &str, language: TestLanguage) -> Vec<(String, usize, usize)> {
    let m = masked.as_bytes();
    let mut bodies = Vec::new();
    let mut i = 0;
    while i < m.len() {
        if !is_ident_byte(m[i]) || (i > 0 && is_ident_byte(m[i - 1])) {
            i += 1;
            continue;
        }
        let word = word_at(masked, i);
        let word_end = i + word.len();
        let declares = match language {
            TestLanguage::Rust => word == "fn",
            TestLanguage::Node => matches!(word, "function" | "const" | "let" | "var"),
        };
        if !declares {
            i = word_end;
            continue;
        }
        let mut at = word_end;
        while at < m.len() && (m[at].is_ascii_whitespace() || m[at] == b'*') {
            at += 1;
        }
        let name = word_at(masked, at.min(m.len().saturating_sub(1)));
        if name.is_empty() || at >= m.len() {
            i = word_end;
            continue;
        }
        let after_name = at + name.len();
        let open = if word == "function" || word == "fn" {
            // Up to the body's `{`, over the parameter list and any return type.
            let mut scan = after_name;
            let mut depth = 0_i32;
            let mut found = None;
            while scan < m.len() {
                match m[scan] {
                    b'(' | b'[' | b'<' => depth += 1,
                    b')' | b']' | b'>' => depth -= 1,
                    b'{' if depth <= 0 => {
                        found = Some(scan);
                        break;
                    }
                    b';' if depth <= 0 => break,
                    _ => {}
                }
                scan += 1;
            }
            found
        } else {
            // `const name = (…) => {` : the arrow before any statement end.
            let rest = &masked[after_name..];
            let statement_end = rest.find(';').unwrap_or(rest.len());
            rest[..statement_end].find("=>").and_then(|arrow| {
                let mut scan = after_name + arrow + 2;
                while scan < m.len() && m[scan].is_ascii_whitespace() {
                    scan += 1;
                }
                (m.get(scan) == Some(&b'{')).then_some(scan)
            })
        };
        if let Some(open) = open
            && let Some(close) = matching(m, open)
        {
            bodies.push((name.to_owned(), open + 1, close));
        }
        i = word_end;
    }
    bodies
}

fn local_helpers(file: &SourceFile) -> LocalHelpers {
    let Some(language) = TestLanguage::of_path(&file.path) else {
        return LocalHelpers::new();
    };
    let masked = mask(&file.source, language);
    let none = LocalHelpers::new();
    function_bodies(&masked, language)
        .into_iter()
        .filter_map(|(name, from, to)| {
            let helper = TestCase {
                name: name.clone(),
                path: file.path.clone(),
                line: line_of(&file.source, from),
                language,
                should_panic: false,
                has_children: false,
                body: file.source[from..to].to_owned(),
                masked: masked[from..to].to_owned(),
                file_source: file.source.clone(),
                body_offset: from,
            };
            let asserted = assertions_of(&helper, &none);
            let asserts = !asserted.direct.is_empty() || asserted.helper_calls > 0;
            let asserts_error = asserted.direct.iter().any(|a| a.error) || asserted.error_checked;
            asserts.then_some((name, asserts_error))
        })
        .collect()
}

/// Whether a Node `catch` block (`catch (e) { … }` or `.catch(e => …)`) contains an assertion.
fn catch_block_asserts(masked: &str) -> bool {
    let m = masked.as_bytes();
    masked.match_indices("catch").any(|(at, _)| {
        let mut next = at + "catch".len();
        while next < m.len() && m[next].is_ascii_whitespace() {
            next += 1;
        }
        let mut region = None;
        if m.get(next) == Some(&b'(') {
            let promise_form = at > 0 && m[at - 1] == b'.';
            let close = matching(m, next);
            if promise_form {
                region = close.map(|close| (next, close));
            } else if let Some(close) = close {
                let mut brace = close + 1;
                while brace < m.len() && m[brace].is_ascii_whitespace() {
                    brace += 1;
                }
                region = matching(m, brace)
                    .filter(|_| m.get(brace) == Some(&b'{'))
                    .map(|end| (brace, end));
            }
        } else if m.get(next) == Some(&b'{') {
            region = matching(m, next).map(|end| (next, end));
        }
        region.is_some_and(|(from, to)| {
            masked[from..to].contains("assert") || masked[from..to].contains("expect(")
        })
    })
}

fn assertions_of(case: &TestCase, helpers: &LocalHelpers) -> Assertions {
    let m = case.masked.as_bytes();
    let mut found = Assertions::default();
    let mut i = 0;
    while i < m.len() {
        if !is_ident_byte(m[i]) || (i > 0 && is_ident_byte(m[i - 1])) {
            i += 1;
            continue;
        }
        let end = i + m[i..]
            .iter()
            .take_while(|byte| is_ident_byte(**byte))
            .count();
        let word = &case.masked[i..end];
        let after_dot = i > 0 && m[i - 1] == b'.';
        let consumed = match case.language {
            TestLanguage::Rust => rust_assertion(case, word, i, end, after_dot, &mut found),
            TestLanguage::Node => node_assertion(case, word, i, end, after_dot, &mut found),
        };
        i = consumed.unwrap_or(end).max(end);
    }
    let text = &case.masked;
    found.error_checked = match case.language {
        // `unwrap_err()` and `expect_err()` fail the test when the call succeeded.
        TestLanguage::Rust => text.contains("unwrap_err(") || text.contains("expect_err("),
        TestLanguage::Node => {
            ((text.contains(".rejects") || text.contains("toThrow")) && found.helper_calls > 0)
                || catch_block_asserts(text)
        }
    };
    // A test that hands its checking to a helper declared in the same file asserts through it.
    let m = text.as_bytes();
    let mut i = 0;
    while i < m.len() {
        if !is_ident_byte(m[i]) || (i > 0 && is_ident_byte(m[i - 1])) {
            i += 1;
            continue;
        }
        let word = word_at(text, i);
        let end = i + word.len();
        let after_dot = i > 0 && m[i - 1] == b'.';
        if !after_dot
            && m.get(end) == Some(&b'(')
            && word != case.name
            && let Some(asserts_error) = helpers.get(word)
        {
            found.helper_calls += 1;
            found.helper_error |= *asserts_error;
        }
        i = end;
    }
    found
}

fn call_args(case: &TestCase, open: usize) -> Option<(Vec<(String, String)>, usize)> {
    let close = matching(case.masked.as_bytes(), open)?;
    Some((split_args(&case.body, &case.masked, open + 1, close), close))
}

fn rust_assertion(
    case: &TestCase,
    word: &str,
    start: usize,
    end: usize,
    after_dot: bool,
    found: &mut Assertions,
) -> Option<usize> {
    let m = case.masked.as_bytes();
    let mut next = end;
    while next < m.len() && m[next].is_ascii_whitespace() {
        next += 1;
    }
    let is_macro = m.get(next) == Some(&b'!');
    if is_macro {
        let mut open = next + 1;
        while open < m.len() && m[open].is_ascii_whitespace() {
            open += 1;
        }
        let base = word.strip_prefix("debug_").unwrap_or(word);
        if !matches!(
            base,
            "assert" | "assert_eq" | "assert_ne" | "assert_matches"
        ) || !matches!(m.get(open), Some(b'(' | b'[' | b'{'))
        {
            return None;
        }
        let (args, close) = call_args(case, open)?;
        let line = line_of(&case.file_source, case.body_offset + start);
        // Arguments after the compared ones are the failure message.
        let value_args = if base == "assert" { 1 } else { 2 };
        let compared: Vec<&(String, String)> = args.iter().take(value_args).collect();
        found.direct.push(classify(base, &compared, line));
        return Some(close);
    }
    if m.get(next) == Some(&b'(')
        && !after_dot
        && helper_name(word, &["assert", "verify", "expect_", "check_"])
    {
        found.helper_calls += 1;
    }
    None
}

fn node_assertion(
    case: &TestCase,
    word: &str,
    start: usize,
    end: usize,
    after_dot: bool,
    found: &mut Assertions,
) -> Option<usize> {
    let m = case.masked.as_bytes();
    if word == "assert" {
        // `assert`, `assert.strict.equal`, `t.assert.equal`: the last segment names the check.
        let mut at = end;
        let mut method = String::new();
        while m.get(at) == Some(&b'.') {
            let seg_end = at
                + 1
                + m[at + 1..]
                    .iter()
                    .take_while(|byte| is_ident_byte(**byte))
                    .count();
            method = case.masked[at + 1..seg_end].to_owned();
            at = seg_end;
        }
        if method == "strict" {
            method.clear();
        }
        let mut open = at;
        while open < m.len() && m[open].is_ascii_whitespace() {
            open += 1;
        }
        if m.get(open) != Some(&b'(') {
            return None;
        }
        let (args, close) = call_args(case, open)?;
        let line = line_of(&case.file_source, case.body_offset + start);
        let kind = if method.is_empty() || method == "ok" {
            "ok"
        } else {
            method.as_str()
        };
        let value_args = match kind {
            "ok" | "ifError" | "fail" => 1,
            "throws" | "rejects" | "doesNotThrow" | "doesNotReject" => 0,
            _ => 2,
        };
        let compared: Vec<&(String, String)> = args.iter().take(value_args).collect();
        found.direct.push(classify_node(kind, &compared, line));
        return Some(close);
    }
    if m.get(end) == Some(&b'(') && !after_dot && helper_name(word, &["expect", "verify", "check"])
    {
        found.helper_calls += 1;
    }
    None
}

fn helper_name(word: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| word.starts_with(prefix))
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Splits `expr` at its first top-level comparator: `(lhs, rhs)` of original text.
fn split_comparison(expr: &(String, String)) -> Option<(String, String)> {
    let m = expr.1.as_bytes();
    let mut depth = 0_i32;
    let mut index = 0;
    while index < m.len() {
        match m[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if depth == 0 => {
                for op in COMPARATORS.iter().copied() {
                    if m[index..].starts_with(op.as_bytes()) {
                        // `->` and `=>` are not comparators.
                        if op == ">" && index > 0 && matches!(m[index - 1], b'-' | b'=') {
                            continue;
                        }
                        let (lhs, rhs) = (&expr.0[..index], &expr.0[index + op.len()..]);
                        return Some((lhs.trim().to_owned(), rhs.trim().to_owned()));
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Whether `operand` is a plain variable, field path, constant or literal: nothing that runs code.
/// Two equal call expressions are not a tautology (`a().bytes() == a().bytes()` guards that the
/// call is repeatable), but one name compared with itself can never differ.
fn is_plain_operand(operand: &str) -> bool {
    !operand.is_empty() && !operand.contains(['(', '[', '{', '!'])
}

fn is_value_predicate(masked_expr: &str) -> bool {
    ["matches!", ".contains(", ".starts_with(", ".ends_with("]
        .iter()
        .any(|pattern| masked_expr.contains(pattern))
}

fn classify(base: &str, args: &[&(String, String)], line: u32) -> Assertion {
    let mut assertion = Assertion {
        line,
        ..Assertion::default()
    };
    match base {
        "assert" => {
            let Some(expr) = args.first() else {
                return assertion;
            };
            let text = compact(&expr.0);
            assertion.error = expr.1.contains("is_err") || expr.1.contains("Err(");
            if text == "true" {
                assertion.trivial = true;
            } else if let Some((lhs, rhs)) = split_comparison(expr) {
                if compact(&lhs) == compact(&rhs)
                    && is_plain_operand(&compact(&lhs))
                    && is_equality(&expr.1)
                {
                    assertion.trivial = true;
                }
                assertion.literals = bare_literals([lhs.as_str(), rhs.as_str()]);
            } else {
                assertion.truthy_only = !is_value_predicate(&expr.1);
            }
        }
        "assert_matches" => {
            assertion.error = args
                .first()
                .is_some_and(|expr| expr.1.contains("Err(") || expr.1.contains("is_err"));
        }
        "assert_eq" | "assert_ne" => {
            if let [lhs, rhs] = args {
                if base == "assert_eq"
                    && compact(&lhs.0) == compact(&rhs.0)
                    && is_plain_operand(&compact(&lhs.1))
                {
                    assertion.trivial = true;
                }
                assertion.error = lhs.1.contains("Err(") || rhs.1.contains("Err(");
                assertion.literals = bare_literals([lhs.0.as_str(), rhs.0.as_str()]);
            }
        }
        _ => {}
    }
    assertion
}

fn is_equality(masked_expr: &str) -> bool {
    masked_expr.contains("==") && !masked_expr.contains("!=")
        || masked_expr.contains("<=")
        || masked_expr.contains(">=")
}

fn classify_node(kind: &str, args: &[&(String, String)], line: u32) -> Assertion {
    let mut assertion = Assertion {
        line,
        ..Assertion::default()
    };
    match kind {
        "ok" => {
            let Some(expr) = args.first() else {
                return assertion;
            };
            if compact(&expr.0) == "true" {
                assertion.trivial = true;
            } else if let Some((lhs, rhs)) = split_comparison(expr) {
                if compact(&lhs) == compact(&rhs)
                    && is_plain_operand(&compact(&lhs))
                    && is_equality(&expr.1)
                {
                    assertion.trivial = true;
                }
                assertion.literals = bare_literals([lhs.as_str(), rhs.as_str()]);
            } else {
                assertion.truthy_only = !is_value_predicate(&expr.1);
            }
        }
        "throws" | "rejects" => assertion.error = true,
        "equal" | "strictEqual" | "deepEqual" | "deepStrictEqual" | "notEqual"
        | "notStrictEqual" | "notDeepEqual" | "notDeepStrictEqual" | "match" => {
            if let [lhs, rhs] = args {
                let positive = matches!(
                    kind,
                    "equal" | "strictEqual" | "deepEqual" | "deepStrictEqual"
                );
                if positive
                    && compact(&lhs.0) == compact(&rhs.0)
                    && is_plain_operand(&compact(&lhs.1))
                {
                    assertion.trivial = true;
                }
                assertion.literals = bare_literals([lhs.0.as_str(), rhs.0.as_str()]);
            }
        }
        _ => {}
    }
    assertion
}

/// Operands that are a bare number other than 0/1 or a non-empty string literal.
fn bare_literals<'a>(operands: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    operands
        .into_iter()
        .filter_map(|operand| {
            let mut text = operand.trim().trim_start_matches('&').trim();
            for wrapper in ["Some(", "Ok(", "Err("] {
                if let Some(inner) = text.strip_prefix(wrapper).and_then(|t| t.strip_suffix(')')) {
                    text = inner.trim();
                }
            }
            for suffix in [".to_string()", ".to_owned()", ".into()"] {
                text = text.strip_suffix(suffix).unwrap_or(text);
            }
            if let Some(inner) = text
                .strip_prefix("String::from(")
                .and_then(|t| t.strip_suffix(')'))
            {
                text = inner.trim();
            }
            is_unexplained_literal(text).then(|| text.to_owned())
        })
        .collect()
}

fn is_unexplained_literal(text: &str) -> bool {
    let text = text
        .strip_prefix('b')
        .filter(|t| t.starts_with('"'))
        .unwrap_or(text);
    let quoted = text.len() > 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'')
                && text.ends_with('\'')
                && !text.contains('\\')
                && text.chars().count() > 3)
            || (text.starts_with('`') && text.ends_with('`')));
    if quoted {
        return !text.contains("${");
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if !digits.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    if !digits
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return false;
    }
    let mut core = digits;
    for suffix in [
        "usize", "isize", "u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16", "u8", "i8",
        "f64", "f32",
    ] {
        if let Some(stripped) = core.strip_suffix(suffix) {
            core = stripped;
            break;
        }
    }
    !matches!(core, "0" | "1")
}

// ---------------------------------------------------------------------------------------------
// Names.
// ---------------------------------------------------------------------------------------------

const FILLER_WORDS: &[&str] = &["test", "tests", "it", "should", "can", "does", "must"];

const GENERIC_WORDS: &[&str] = &[
    "case",
    "cases",
    "work",
    "works",
    "working",
    "worked",
    "basic",
    "simple",
    "sanity",
    "smoke",
    "example",
    "ok",
    "okay",
    "fine",
    "good",
    "correct",
    "correctly",
    "properly",
    "thing",
    "stuff",
    "foo",
    "bar",
    "baz",
    "temp",
    "tmp",
    "todo",
    "check",
    "checks",
    "run",
    "runs",
    "feature",
    "default",
    "something",
];

const OUTCOME_WORDS: &[&str] = &[
    "returns",
    "return",
    "rejects",
    "reject",
    "accepts",
    "accept",
    "fails",
    "fail",
    "succeeds",
    "errors",
    "raises",
    "throws",
    "emits",
    "creates",
    "deletes",
    "removes",
    "adds",
    "updates",
    "sets",
    "keeps",
    "preserves",
    "stops",
    "starts",
    "allows",
    "denies",
    "refuses",
    "grants",
    "blocks",
    "skips",
    "retries",
    "expires",
    "closes",
    "opens",
    "is",
    "are",
    "was",
    "were",
    "has",
    "have",
    "does",
    "do",
    "not",
    "no",
    "never",
    "without",
    "yields",
    "produces",
    "gives",
    "equals",
    "matches",
    "contains",
    "includes",
    "excludes",
    "uses",
    "ignores",
    "logs",
    "records",
    "writes",
    "reads",
    "sends",
    "receives",
    "resolves",
    "triggers",
    "causes",
    "leaves",
    "becomes",
    "stays",
    "remains",
    "charges",
    "applies",
    "computes",
    "sorts",
    "orders",
    "formats",
    "parses",
    "rounds",
    "counts",
    "caps",
    "limits",
    "refunds",
    "cancels",
    "marks",
    "moves",
    "takes",
    "makes",
    "shows",
    "hides",
    "enables",
    "disables",
    "notifies",
    "clears",
    "resets",
    "increments",
    "decrements",
    "falls",
    "rises",
    "exceeds",
    "drops",
    "appends",
    "replaces",
    "fires",
    "finds",
    "locks",
    "unlocks",
    "holds",
    "merges",
    "splits",
    "passes",
    "survives",
    "must",
    "cannot",
    "can",
    "will",
    "be",
    "becomes",
    "reaches",
    "stores",
    "loads",
    "lists",
    "reports",
    "counts",
    "persists",
    "restores",
    "totals",
    "ships",
    "calculates",
    "renders",
    "displays",
    "selects",
    "chooses",
    "handles",
    "ends",
    "begins",
    "waits",
    "wins",
    "loses",
];

fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in name.chars() {
        if !character.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if character.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_lowercase();
        current.extend(character.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_generic_word(word: &str) -> bool {
    let stem = word.trim_end_matches(|c: char| c.is_ascii_digit());
    stem.is_empty() || GENERIC_WORDS.contains(&stem) || FILLER_WORDS.contains(&stem)
}

fn name_findings(name: &str, policy: &LintPolicy) -> Vec<(LintRule, String)> {
    let words = name_words(name);
    let meaningful: Vec<&String> = words
        .iter()
        .skip_while(|word| FILLER_WORDS.contains(&word.as_str()))
        .collect();
    if meaningful.iter().all(|word| is_generic_word(word)) {
        return vec![(
            LintRule::GenericName,
            format!(
                "the name `{name}` says nothing about the condition or the outcome; name what happens and when"
            ),
        )];
    }
    let counted = words
        .iter()
        .skip(usize::from(
            words
                .first()
                .is_some_and(|w| matches!(w.as_str(), "test" | "it")),
        ))
        .count();
    let mut findings = Vec::new();
    if counted < policy.min_name_words {
        findings.push((
            LintRule::ShortName,
            format!("the name `{name}` has {counted} word(s); state the condition and the outcome in at least {}", policy.min_name_words),
        ));
    }
    let states_outcome = words.iter().any(|word| {
        OUTCOME_WORDS.contains(&word.as_str()) || (word.len() > 4 && word.ends_with("ed"))
    });
    if !states_outcome {
        findings.push((
            LintRule::NoOutcomeVerb,
            format!("the name `{name}` has no word that states an outcome (returns, rejects, keeps, is, not …)"),
        ));
    }
    findings
}

// ---------------------------------------------------------------------------------------------
// The lint.
// ---------------------------------------------------------------------------------------------

fn mapped_criteria<'a>(case: &TestCase, manifest: &'a [ManifestEntry]) -> BTreeSet<&'a str> {
    let qualified = format!("{}::{}", case.path, case.name);
    manifest
        .iter()
        .filter(|entry| entry.test_id == case.name || entry.test_id == qualified)
        .flat_map(|entry| entry.criterion_ids.iter().map(String::as_str))
        .collect()
}

/// Lints every test case in `files`.
///
/// `criteria` and `manifest` supply the mapping that the `truthiness-only` and
/// `missing-error-assertion` rules need; with an empty mapping those two rules are silent and
/// everything else still applies. Findings are ordered by file, line and rule.
#[must_use]
pub fn lint_tests(
    files: &[SourceFile],
    criteria: &[CriterionRef],
    manifest: &[ManifestEntry],
    policy: &LintPolicy,
) -> Vec<LintFinding> {
    let kinds: BTreeMap<&str, CriterionKind> = criteria
        .iter()
        .map(|criterion| (criterion.id.as_str(), criterion.kind))
        .collect();
    let mut findings: Vec<LintFinding> = Vec::new();
    let mut emit = |rule: LintRule, case: &TestCase, line: u32, message: String| {
        let severity = match policy.setting(rule) {
            RuleSetting::Off => return,
            RuleSetting::Blocking => LintSeverity::Blocking,
            RuleSetting::Advisory => LintSeverity::Advisory,
        };
        findings.push(LintFinding {
            rule,
            severity,
            path: case.path.clone(),
            line,
            test: case.name.clone(),
            message,
        });
    };
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let helpers = local_helpers(file);
        for case in discover(file) {
            declared.insert(case.name.clone());
            declared.insert(format!("{}::{}", case.path, case.name));
            for (rule, message) in name_findings(&case.name, policy) {
                emit(rule, &case, case.line, message);
            }
            let asserted = assertions_of(&case, &helpers);
            let any_assertion = !asserted.direct.is_empty()
                || asserted.helper_calls > 0
                || case.should_panic
                || asserted.error_checked;
            // A test that only groups subtests is judged through them.
            if !any_assertion && !case.has_children {
                emit(
                    LintRule::NoAssertion,
                    &case,
                    case.line,
                    "the lint sees no assertion in this test: no assert macro or call, no call to a helper that asserts (a function in this file that asserts, or one named assert…/verify…/expect…/check…), and no #[should_panic]; if it asserts inside a helper from another file, name that helper assert_… so the lint can see it".to_owned(),
                );
            }
            for assertion in &asserted.direct {
                if assertion.trivial {
                    emit(
                        LintRule::TrivialAssertion,
                        &case,
                        assertion.line,
                        "the assertion cannot fail (constant or self-comparison)".to_owned(),
                    );
                }
                if !assertion.literals.is_empty() {
                    emit(
                        LintRule::UnexplainedLiteral,
                        &case,
                        assertion.line,
                        format!(
                            "the assertion compares against {} directly; bind it to a named constant or fixture that says what it means",
                            assertion.literals.join(", ")
                        ),
                    );
                }
            }
            let mapped = mapped_criteria(&case, manifest);
            let has_error_criterion = mapped
                .iter()
                .any(|id| kinds.get(id) == Some(&CriterionKind::Error));
            let has_value_criterion = mapped.iter().any(|id| {
                kinds
                    .get(id)
                    .is_some_and(|kind| *kind != CriterionKind::Error)
            });
            if has_error_criterion {
                // The error must be asserted: evaluating `is_err()` and dropping the answer
                // passes whether or not the call failed.
                let asserts_error = case.should_panic
                    || asserted.direct.iter().any(|assertion| assertion.error)
                    || asserted.error_checked
                    || asserted.helper_error;
                if !asserts_error {
                    emit(
                        LintRule::MissingErrorAssertion,
                        &case,
                        case.line,
                        "the test covers an Error criterion but never asserts the error: put it in an assertion (assert!(r.is_err()), assert_eq!(r, Err(…)), matches!, unwrap_err/expect_err, #[should_panic], assert.throws/rejects); an error predicate whose result is dropped proves nothing".to_owned(),
                    );
                }
            }
            let only_truthiness = !asserted.direct.is_empty()
                && asserted.helper_calls == 0
                && asserted.direct.iter().all(|a| a.truthy_only || a.trivial);
            if has_value_criterion && !has_error_criterion && only_truthiness {
                emit(
                    LintRule::TruthinessOnly,
                    &case,
                    case.line,
                    "the criterion specifies a value but every assertion only checks that something is truthy; compare the value".to_owned(),
                );
            }
        }
    }
    for entry in manifest {
        if !declared.contains(&entry.test_id) {
            let unknown = TestCase {
                name: entry.test_id.clone(),
                path: "<manifest>".to_owned(),
                line: 0,
                language: TestLanguage::Rust,
                should_panic: false,
                has_children: false,
                body: String::new(),
                masked: String::new(),
                file_source: String::new(),
                body_offset: 0,
            };
            emit(
                LintRule::UnknownTest,
                &unknown,
                0,
                format!(
                    "the manifest maps `{}` to criteria but no scanned file declares that test",
                    entry.test_id
                ),
            );
        }
    }
    findings.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.rule, &a.message).cmp(&(
            b.path.as_str(),
            b.line,
            b.rule,
            &b.message,
        ))
    });
    findings
}

// ---------------------------------------------------------------------------------------------
// Coverage.
// ---------------------------------------------------------------------------------------------

/// What the manifest does and does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoverageReport {
    /// Criteria no test covers, sorted.
    pub uncovered: Vec<String>,
    /// `(test_id, criterion_id)` pairs naming a criterion the spec does not have, sorted.
    pub unknown_references: Vec<(String, String)>,
    /// Tests that map to no criterion at all, sorted.
    pub unmapped_tests: Vec<String>,
}

impl CoverageReport {
    /// Whether every criterion is covered and every reference is valid.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.uncovered.is_empty()
            && self.unknown_references.is_empty()
            && self.unmapped_tests.is_empty()
    }
}

/// Which criteria lack a test and which tests point at criteria that do not exist.
#[must_use]
pub fn check_coverage(criteria: &[CriterionRef], manifest: &[ManifestEntry]) -> CoverageReport {
    let known: BTreeSet<&str> = criteria.iter().map(|c| c.id.as_str()).collect();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let mut report = CoverageReport::default();
    let mut unknown: BTreeSet<(String, String)> = BTreeSet::new();
    let mut unmapped: BTreeSet<String> = BTreeSet::new();
    for entry in manifest {
        if entry.criterion_ids.is_empty() {
            unmapped.insert(entry.test_id.clone());
        }
        for id in &entry.criterion_ids {
            if known.contains(id.as_str()) {
                covered.insert(id.as_str());
            } else {
                unknown.insert((entry.test_id.clone(), id.clone()));
            }
        }
    }
    report.uncovered = known
        .iter()
        .filter(|id| !covered.contains(*id))
        .map(|id| (*id).to_owned())
        .collect();
    report.unknown_references = unknown.into_iter().collect();
    report.unmapped_tests = unmapped.into_iter().collect();
    report
}

#[cfg(test)]
mod tests {
    use super::{
        CriterionKind, CriterionRef, LintPolicy, LintRule, LintSeverity, ManifestEntry,
        RuleSetting, SourceFile, check_coverage, has_blocking, lint_tests,
    };

    /// The line a fixture test is declared on, counting from the first line of its source.
    const SECOND_LINE: u32 = 2;
    /// How many tests the generic-name fixture names with nothing but filler.
    const GENERICALLY_NAMED_TESTS: usize = 2;
    /// How many tests of the no-assertion fixture assert nothing.
    const SILENT_TESTS: usize = 1;
    /// How many tests of the error-criterion fixture never assert their error.
    const TESTS_MISSING_AN_ERROR_ASSERTION: usize = 1;
    /// Lines of the Rust fixture whose assertions compare against a bare number or string.
    const UNEXPLAINED_LITERAL_LINES: [u32; 3] = [4, 8, 9];
    /// The line of the Node fixture whose assertion compares against a bare number.
    const BARE_LITERAL_LINE: u32 = 2;

    fn rust_file(source: &str) -> SourceFile {
        SourceFile {
            path: "tests/checkout.rs".to_owned(),
            source: source.to_owned(),
        }
    }

    fn node_file(source: &str) -> SourceFile {
        SourceFile {
            path: "test/checkout.test.mjs".to_owned(),
            source: source.to_owned(),
        }
    }

    fn criterion(id: &str, kind: CriterionKind) -> CriterionRef {
        CriterionRef {
            id: id.to_owned(),
            kind,
        }
    }

    fn maps(test: &str, criteria: &[&str]) -> ManifestEntry {
        ManifestEntry {
            test_id: test.to_owned(),
            criterion_ids: criteria.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn rules_of(files: &[SourceFile]) -> Vec<LintRule> {
        lint_tests(files, &[], &[], &LintPolicy::default())
            .iter()
            .map(|finding| finding.rule)
            .collect()
    }

    const GOOD_RUST: &str = r#"
const FREE_SHIPPING_THRESHOLD: u32 = 5000;
#[test]
fn an_order_over_the_free_shipping_threshold_ships_for_nothing() {
    let order = an_order_worth(FREE_SHIPPING_THRESHOLD + 1);
    assert_eq!(shipping_cost(&order), NO_SHIPPING_COST, "orders above the threshold ship free");
}
#[test]
fn an_expired_coupon_is_rejected_with_an_error() {
    let coupon = an_expired_coupon();
    assert!(apply(&coupon).is_err(), "expired coupons must not apply");
}
"#;

    const GOOD_NODE: &str = r#"
import test from 'node:test';
import assert from 'node:assert/strict';
const DEFAULT_QUANTITY = 3;
test('a cart with items totals the price of every line', () => {
  const cart = aCartWith(DEFAULT_QUANTITY);
  assert.equal(total(cart), expectedTotalFor(cart));
});
test('adding a negative quantity is rejected with an error', () => {
  assert.throws(() => add(aCart(), NEGATIVE_QUANTITY), /quantity/);
});
"#;

    #[test]
    fn good_domain_tests_in_both_languages_produce_no_findings() {
        let findings = lint_tests(
            &[rust_file(GOOD_RUST), node_file(GOOD_NODE)],
            &[],
            &[],
            &LintPolicy::default(),
        );
        assert!(findings.is_empty(), "unexpected findings: {findings:#?}");
    }

    #[test]
    fn generic_rust_and_node_names_are_blocking_findings_with_their_line() {
        let rust =
            rust_file("#[test]\nfn test_case_1() { assert_eq!(total(), expected_total()); }\n");
        let node = node_file("test('works', () => { assert.equal(total(), expectedTotal()); });\n");
        let findings = lint_tests(&[rust, node], &[], &[], &LintPolicy::default());
        let generic: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule == LintRule::GenericName)
            .collect();
        assert_eq!(
            generic.len(),
            GENERICALLY_NAMED_TESTS,
            "both names are generic: {findings:#?}"
        );
        assert!(
            generic
                .iter()
                .all(|finding| finding.severity == LintSeverity::Blocking),
            "a name that says nothing blocks by default: {generic:#?}"
        );
        assert!(has_blocking(&findings), "a generic name blocks the review");
        assert_eq!(
            generic
                .iter()
                .find(|finding| finding.path.ends_with(".rs"))
                .map(|finding| finding.line),
            Some(SECOND_LINE),
            "the Rust finding points at the fn line"
        );
    }

    #[test]
    fn each_uninformative_name_is_generic_and_each_sentence_name_is_not() {
        let generic = [
            "test_case_1",
            "test2",
            "should_work",
            "it works",
            "basic",
            "works_correctly",
            "testSimple",
        ];
        for name in generic {
            let source = format!("test('{name}', () => {{ assert.equal(a(), b()); }});");
            assert!(
                rules_of(&[node_file(&source)]).contains(&LintRule::GenericName),
                "{name} should be generic"
            );
        }
        let fine =
            "test('rejects an expired token with an error', () => { assert.equal(a(), b()); });";
        assert!(
            !rules_of(&[node_file(fine)]).contains(&LintRule::GenericName),
            "a name that states condition and outcome is not generic"
        );
    }

    #[test]
    fn short_names_and_names_without_an_outcome_are_advisory() {
        let findings = lint_tests(
            &[node_file(
                "test('login page', () => { assert.equal(a(), b()); });",
            )],
            &[],
            &[],
            &LintPolicy::default(),
        );
        let rules: Vec<_> = findings.iter().map(|f| f.rule).collect();
        assert!(rules.contains(&LintRule::ShortName), "{findings:#?}");
        assert!(rules.contains(&LintRule::NoOutcomeVerb), "{findings:#?}");
        assert!(
            !has_blocking(&findings),
            "name style alone is advisory by default"
        );
    }

    #[test]
    fn a_test_without_any_assertion_is_blocking_and_should_panic_counts_as_one() {
        let source = "#[test]\nfn the_cart_is_built_without_checking_anything() { let _cart = a_cart(); }\n#[test]\n#[should_panic]\nfn an_empty_cart_panics_when_priced() { price(&empty_cart()); }\n";
        let findings = lint_tests(&[rust_file(source)], &[], &[], &LintPolicy::default());
        let missing: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == LintRule::NoAssertion)
            .collect();
        assert_eq!(
            missing.len(),
            SILENT_TESTS,
            "only the first test lacks assertions: {findings:#?}"
        );
        assert_eq!(
            missing[0].line, SECOND_LINE,
            "the finding names the line the silent test is declared on"
        );
    }

    #[test]
    fn trivially_true_assertions_are_blocking_in_both_languages() {
        let rust = rust_file(
            "#[test]\nfn the_total_is_always_true() {\n    assert!(true);\n    let total = 3;\n    assert_eq!(total, total);\n    assert!(total == total);\n}\n",
        );
        let node = node_file(
            "test('the total is always the total', () => {\n  assert.ok(true);\n  assert.equal(total, total);\n  assert.strictEqual(1, 1);\n});\n",
        );
        let findings = lint_tests(&[rust, node], &[], &[], &LintPolicy::default());
        let trivial: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == LintRule::TrivialAssertion)
            .map(|f| (f.path.as_str(), f.line))
            .collect();
        assert_eq!(
            trivial,
            vec![
                ("test/checkout.test.mjs", 2),
                ("test/checkout.test.mjs", 3),
                ("test/checkout.test.mjs", 4),
                ("tests/checkout.rs", 3),
                ("tests/checkout.rs", 5),
                ("tests/checkout.rs", 6),
            ],
            "each trivial assertion is reported at its own line"
        );
    }

    #[test]
    fn magic_numbers_and_strings_in_assertions_are_flagged_but_named_values_are_not() {
        let source = "#[test]\nfn the_total_of_a_full_cart_is_returned() {\n    let expected = 1250;\n    assert_eq!(total(&a_full_cart()), 1250);\n    assert_eq!(total(&a_full_cart()), expected);\n    assert_eq!(parse(\"42\"), parsed_forty_two());\n    assert_eq!(status(), 0);\n    assert_eq!(label(), \"Free shipping\");\n    assert!(count() >= 7);\n}\n";
        let findings = lint_tests(&[rust_file(source)], &[], &[], &LintPolicy::default());
        let literal_lines: Vec<u32> = findings
            .iter()
            .filter(|f| f.rule == LintRule::UnexplainedLiteral)
            .map(|f| f.line)
            .collect();
        assert_eq!(
            literal_lines, UNEXPLAINED_LITERAL_LINES,
            "only the bare number, the bare string and the bare comparison are unexplained: {findings:#?}"
        );
    }

    #[test]
    fn a_node_assertion_against_a_bare_literal_is_flagged() {
        let source = "test('a full cart totals its lines', () => {\n  assert.equal(total(fullCart()), 1250);\n  assert.equal(total(fullCart()), FULL_CART_TOTAL);\n});\n";
        let findings = lint_tests(&[node_file(source)], &[], &[], &LintPolicy::default());
        let lines: Vec<u32> = findings
            .iter()
            .filter(|f| f.rule == LintRule::UnexplainedLiteral)
            .map(|f| f.line)
            .collect();
        assert_eq!(
            lines,
            [BARE_LITERAL_LINE],
            "only the assertion against the bare number is unexplained: {findings:#?}"
        );
    }

    #[test]
    fn truthiness_only_assertions_are_flagged_for_a_value_criterion_but_not_for_an_unmapped_test() {
        let source = "test('a paid order has its receipt total', () => {\n  assert.ok(receiptFor(paidOrder()));\n});\n";
        let criteria = [criterion("AC-1", CriterionKind::Normal)];
        let manifest = [maps("a paid order has its receipt total", &["AC-1"])];
        let mapped = lint_tests(
            &[node_file(source)],
            &criteria,
            &manifest,
            &LintPolicy::default(),
        );
        assert!(
            mapped.iter().any(|f| f.rule == LintRule::TruthinessOnly),
            "{mapped:#?}"
        );
        let unmapped = lint_tests(&[node_file(source)], &[], &[], &LintPolicy::default());
        assert!(
            !unmapped.iter().any(|f| f.rule == LintRule::TruthinessOnly),
            "no mapping, no judgement"
        );
    }

    #[test]
    fn an_error_criterion_needs_an_error_assertion_in_its_test() {
        let weak = "#[test]\nfn an_expired_coupon_is_not_applied() {\n    let result = apply(&an_expired_coupon());\n    assert_eq!(result.len(), COUPON_SLOTS);\n}\n";
        let strong = "#[test]\nfn an_expired_coupon_is_rejected() {\n    assert!(apply(&an_expired_coupon()).is_err());\n}\n";
        let criteria = [criterion("AC-2", CriterionKind::Error)];
        let manifest = [
            maps("an_expired_coupon_is_not_applied", &["AC-2"]),
            maps("an_expired_coupon_is_rejected", &["AC-2"]),
        ];
        let findings = lint_tests(
            &[rust_file(weak), rust_file(strong)],
            &criteria,
            &manifest,
            &LintPolicy::default(),
        );
        let missing: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == LintRule::MissingErrorAssertion)
            .collect();
        assert_eq!(
            missing.len(),
            TESTS_MISSING_AN_ERROR_ASSERTION,
            "only the test that never asserts its error is reported: {findings:#?}"
        );
        assert_eq!(
            missing[0].test, "an_expired_coupon_is_not_applied",
            "the finding names the test that never asserts its error"
        );
        assert_eq!(
            missing[0].severity,
            LintSeverity::Blocking,
            "an error criterion without an error assertion blocks by default"
        );
    }

    #[test]
    fn severities_are_configurable_and_a_rule_can_be_switched_off() {
        let source = "test('login page', () => { assert.equal(a(), b()); });";
        let strict = LintPolicy::default().with(LintRule::ShortName, RuleSetting::Blocking);
        assert!(
            has_blocking(&lint_tests(&[node_file(source)], &[], &[], &strict)),
            "a rule switched to blocking blocks"
        );
        let lax = LintPolicy::default()
            .with(LintRule::ShortName, RuleSetting::Off)
            .with(LintRule::NoOutcomeVerb, RuleSetting::Off);
        assert!(
            lint_tests(&[node_file(source)], &[], &[], &lax).is_empty(),
            "rules switched off report nothing"
        );
    }

    #[test]
    fn comments_and_string_contents_never_count_as_tests_or_assertions() {
        let source = "// test('works', () => { assert.ok(true) })\ntest('a message mentions assert.ok(true) in text', () => {\n  assert.equal(render(), 'assert.ok(true)'.length > 0 ? RENDERED : OTHER);\n});\n";
        let findings = lint_tests(&[node_file(source)], &[], &[], &LintPolicy::default());
        assert!(
            !findings
                .iter()
                .any(|f| f.rule == LintRule::TrivialAssertion || f.rule == LintRule::GenericName),
            "{findings:#?}"
        );
    }

    #[test]
    fn a_manifest_row_for_a_missing_test_is_reported_as_unknown() {
        let findings = lint_tests(
            &[rust_file(GOOD_RUST)],
            &[],
            &[maps("a_test_nobody_wrote", &["AC-1"])],
            &LintPolicy::default(),
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule == LintRule::UnknownTest && f.test == "a_test_nobody_wrote"),
            "{findings:#?}"
        );
    }

    #[test]
    fn criteria_without_tests_and_tests_for_unknown_criteria_are_reported() {
        let criteria = [
            criterion("AC-1", CriterionKind::Normal),
            criterion("AC-2", CriterionKind::Error),
            criterion("AC-3", CriterionKind::Edge),
        ];
        let manifest = [
            maps("covers_first", &["AC-1"]),
            maps("covers_phantom", &["AC-9"]),
            maps("covers_nothing", &[]),
        ];
        let report = check_coverage(&criteria, &manifest);
        assert_eq!(
            report.uncovered,
            vec!["AC-2".to_owned(), "AC-3".to_owned()],
            "criteria no mapped test names are uncovered"
        );
        assert_eq!(
            report.unknown_references,
            vec![("covers_phantom".to_owned(), "AC-9".to_owned())],
            "a test pointing at a criterion the spec lacks is reported with the criterion"
        );
        assert_eq!(
            report.unmapped_tests,
            vec!["covers_nothing".to_owned()],
            "a test mapped to nothing is reported"
        );
        assert!(!report.is_complete(), "a report with gaps is not complete");
        let complete = check_coverage(&criteria[..1], &manifest[..1]);
        assert!(complete.is_complete(), "{complete:?}");
    }

    // --- Review follow-up (RWP3 findings 2, 3, 7, 8) -------------------------------------------

    /// Identifiers whose characters take more than one byte, in the scripts the lint may meet.
    const MULTIBYTE_IDENTIFIERS: [&str; 6] = [
        "résultat",
        "ünïcode_total",
        "合計",
        "сумма",
        "🧾total",
        "naïve_café",
    ];
    const EXPECTED_TOTAL: &str = "EXPECTED_TOTAL";
    /// A hollow helper leaves exactly the one test that calls it without an assertion.
    const EXACTLY_ONE_FINDING: usize = 1;

    fn findings_of(files: &[SourceFile]) -> Vec<super::LintFinding> {
        lint_tests(files, &[], &[], &LintPolicy::default())
    }

    fn blocking_rules(findings: &[super::LintFinding]) -> Vec<LintRule> {
        findings
            .iter()
            .filter(|finding| finding.severity == LintSeverity::Blocking)
            .map(|finding| finding.rule)
            .collect()
    }

    #[test]
    fn a_unicode_identifier_in_a_comparison_is_linted_without_panicking() {
        let source = format!(
            "#[test]\nfn a_total_is_returned() {{ let résultat = total(); assert!(résultat == {EXPECTED_TOTAL}); }}\n"
        );
        let findings = findings_of(&[rust_file(&source)]);
        assert!(
            blocking_rules(&findings).is_empty(),
            "a comparison against a named constant is a real assertion, accented name or not: {findings:#?}"
        );
        let self_comparison = "#[test]\nfn a_total_is_returned() { let résultat = total(); assert!(résultat == résultat); }\n";
        let findings = findings_of(&[rust_file(self_comparison)]);
        assert!(
            blocking_rules(&findings).contains(&LintRule::TrivialAssertion),
            "a plain name compared with itself cannot fail, whatever alphabet it is written in: {findings:#?}"
        );
    }

    #[test]
    fn multibyte_names_strings_and_comments_never_panic_the_lint_in_either_language() {
        for identifier in MULTIBYTE_IDENTIFIERS {
            for operator in ["==", "!=", "<=", ">=", "<", ">", "==="] {
                let rust = format!(
                    "// {identifier}\n#[test]\nfn {identifier}_is_compared() {{ let {identifier} = total(); assert!({identifier} {operator} {EXPECTED_TOTAL}); assert!({EXPECTED_TOTAL} {operator} {identifier}); assert!(\"{identifier}\" {operator} \"{identifier}\"); assert_eq!({identifier}, {identifier}); }}\n"
                );
                let node = format!(
                    "// {identifier}\ntest('{identifier} is compared', () => {{ const {identifier} = total(); assert.ok({identifier} {operator} {EXPECTED_TOTAL}); assert.ok(`{identifier}` {operator} '{identifier}'); assert.equal({identifier}, {identifier}); }});\ntest('{identifier}', async (t) => {{ await t.test('{identifier} nested', () => {{ assert({identifier}); }}); }});\n"
                );
                let files = [rust_file(&rust), node_file(&node)];
                let manifest = [maps(&format!("{identifier}_is_compared"), &["AC-1"])];
                let criteria = [criterion("AC-1", CriterionKind::Error)];
                let findings = lint_tests(&files, &criteria, &manifest, &LintPolicy::default());
                assert!(
                    findings
                        .iter()
                        .all(|finding| finding.line > 0 || finding.path == "<manifest>"),
                    "every finding names a real line for `{identifier} {operator}`: {findings:#?}"
                );
            }
        }
    }

    #[test]
    fn two_equal_call_expressions_are_a_determinism_check_not_a_tautology() {
        let determinism = "#[test]\nfn a_pack_for_the_same_goal_has_the_same_bytes() { assert_eq!(a_pack_for(&goal()).canonical_bytes(), a_pack_for(&goal()).canonical_bytes()); }\n#[test]\nfn a_repeated_grade_is_the_same() { assert!(grade_task(&task()) == grade_task(&task())); }\n";
        let findings = findings_of(&[rust_file(determinism)]);
        assert!(
            !blocking_rules(&findings).contains(&LintRule::TrivialAssertion),
            "repeating a call and comparing the results can fail when the call is not repeatable: {findings:#?}"
        );
        let node = "test('a pack for the same goal has the same bytes', () => { assert.equal(packFor(goal()).bytes(), packFor(goal()).bytes()); });\n";
        let findings = findings_of(&[node_file(node)]);
        assert!(
            !blocking_rules(&findings).contains(&LintRule::TrivialAssertion),
            "the same holds in Node: {findings:#?}"
        );
        for (source, why) in [
            (
                "#[test]\nfn a_total_is_returned() { let total = total(); assert_eq!(total, total); }\n",
                "a variable compared with itself",
            ),
            (
                "#[test]\nfn a_total_is_returned() { assert_eq!(7, 7); }\n",
                "a literal compared with itself",
            ),
            (
                "#[test]\nfn a_total_is_returned() { assert!(EXPECTED_TOTAL == EXPECTED_TOTAL); }\n",
                "a constant compared with itself",
            ),
            (
                "#[test]\nfn a_total_is_returned() { assert!(cart.total == cart.total); }\n",
                "one field compared with itself",
            ),
        ] {
            let findings = findings_of(&[rust_file(source)]);
            assert!(
                blocking_rules(&findings).contains(&LintRule::TrivialAssertion),
                "{why} cannot fail: {findings:#?}"
            );
        }
    }

    #[test]
    fn a_test_that_delegates_to_a_local_asserting_helper_is_not_reported_as_asserting_nothing() {
        let delegating = "fn a_first_pass_that_reaches_full_review() { assert_eq!(status(), EXIT_REVIEWED); }\n#[test]\nfn a_goal_reaches_full_review() { a_first_pass_that_reaches_full_review(); }\n";
        let findings = findings_of(&[rust_file(delegating)]);
        assert!(
            !blocking_rules(&findings).contains(&LintRule::NoAssertion),
            "the helper in the same file asserts, so the test does through it: {findings:#?}"
        );
        let hollow = "fn a_first_pass_that_reaches_full_review() { start(); }\n#[test]\nfn a_goal_reaches_full_review() { a_first_pass_that_reaches_full_review(); }\n";
        let findings = findings_of(&[rust_file(hollow)]);
        let no_assertion: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule == LintRule::NoAssertion)
            .collect();
        assert_eq!(
            no_assertion.len(),
            EXACTLY_ONE_FINDING,
            "a helper that asserts nothing leaves the test asserting nothing: {findings:#?}"
        );
        assert!(
            no_assertion[0].message.contains("helper")
                && !no_assertion[0].message.contains("passes whatever"),
            "the message says what the lint could and could not see rather than claiming the test is worthless: {}",
            no_assertion[0].message
        );
        let node = "function checksOut() { assert.equal(total(), expectedTotal()); }\nconst alsoChecks = () => { assert.ok(valid()); };\ntest('a cart is priced', () => { checksOut(); });\ntest('a cart is valid', () => { alsoChecks(); });\n";
        let findings = findings_of(&[node_file(node)]);
        assert!(
            !blocking_rules(&findings).contains(&LintRule::NoAssertion),
            "function declarations and arrow constants in the same file are followed too: {findings:#?}"
        );
    }

    #[test]
    fn a_discarded_error_predicate_does_not_cover_an_error_criterion() {
        let criteria = [criterion("AC-2", CriterionKind::Error)];
        let cases = [
            (
                "a_checkout_is_not_rejected",
                "#[test]\nfn a_checkout_is_not_rejected() {\n    let result = checkout();\n    let _ = result.is_err();\n    assert_eq!(cart.total(), EXPECTED_TOTAL);\n}\n",
                true,
            ),
            (
                "an_empty_cart_is_rejected",
                "#[test]\nfn an_empty_cart_is_rejected() { assert!(checkout().is_err()); }\n",
                false,
            ),
            (
                "an_empty_cart_is_an_error_value",
                "#[test]\nfn an_empty_cart_is_an_error_value() { assert!(matches!(checkout(), Err(_))); }\n",
                false,
            ),
            (
                "an_empty_cart_equals_the_error",
                "#[test]\nfn an_empty_cart_equals_the_error() { assert_eq!(checkout(), Err(CartError::Empty)); }\n",
                false,
            ),
            (
                "an_empty_cart_unwraps_to_its_error",
                "#[test]\nfn an_empty_cart_unwraps_to_its_error() { let error = checkout().unwrap_err(); assert_eq!(error, CartError::Empty); }\n",
                false,
            ),
            (
                "an_empty_cart_expects_its_error",
                "#[test]\nfn an_empty_cart_expects_its_error() { checkout().expect_err(\"an empty cart is refused\"); }\n",
                false,
            ),
        ];
        for (name, source, expect_missing) in cases {
            let manifest = [maps(name, &["AC-2"])];
            let findings = lint_tests(
                &[rust_file(source)],
                &criteria,
                &manifest,
                &LintPolicy::default(),
            );
            let missing = findings
                .iter()
                .any(|finding| finding.rule == LintRule::MissingErrorAssertion);
            assert_eq!(
                missing, expect_missing,
                "`{name}`: an error criterion is covered only when the error is asserted: {findings:#?}"
            );
        }
        let node_discarded = "test('a checkout is not rejected', async () => {\n  const failed = await checkout().then(() => false, () => true);\n  assert.equal(cart.total(), EXPECTED_TOTAL);\n});\n";
        let manifest = [maps("a checkout is not rejected", &["AC-2"])];
        let findings = lint_tests(
            &[node_file(node_discarded)],
            &criteria,
            &manifest,
            &LintPolicy::default(),
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == LintRule::MissingErrorAssertion),
            "a Node test that evaluates a failure and asserts something unrelated asserts no error: {findings:#?}"
        );
    }

    #[test]
    fn nested_node_subtests_are_cases_of_their_own_with_their_own_assertions() {
        let source = "test('a cart is priced', async (t) => {\n  await t.test('a discount is applied', () => {});\n  await t.test('a coupon is applied', () => { assert.equal(price(), DISCOUNTED_PRICE); });\n  assert.equal(total(), expectedTotal());\n});\ndescribe('checkout', () => {\n  it('an empty cart is rejected', () => {});\n});\n";
        let findings = findings_of(&[node_file(source)]);
        let silent: Vec<&str> = findings
            .iter()
            .filter(|finding| finding.rule == LintRule::NoAssertion)
            .map(|finding| finding.test.as_str())
            .collect();
        assert_eq!(
            silent,
            ["a discount is applied", "an empty cart is rejected"],
            "an assertionless subtest and an assertionless nested it are each reported by name: {findings:#?}"
        );
        let grouping = "test('a cart is priced', async (t) => {\n  await t.test('a coupon is applied', () => { assert.equal(price(), DISCOUNTED_PRICE); });\n});\n";
        let findings = findings_of(&[node_file(grouping)]);
        assert!(
            !blocking_rules(&findings).contains(&LintRule::NoAssertion),
            "a test that only groups asserted subtests is judged through them: {findings:#?}"
        );
        let borrowed = "test('a cart is priced', async (t) => {\n  await t.test('a coupon is applied', () => { assert.equal(price(), DISCOUNTED_PRICE); });\n  const matched = /coupon/.test('a coupon');\n});\n";
        let findings = findings_of(&[node_file(borrowed)]);
        assert!(
            !findings.iter().any(|finding| finding.test == "a coupon"),
            "a regular expression's .test() is not a subtest: {findings:#?}"
        );
        let own_assertion_is_not_the_childs = "test('a cart is priced', async (t) => {\n  await t.test('a coupon is applied', () => { assert.equal(price(), DISCOUNTED_PRICE); });\n  const price = 1;\n});\ntest('a cart is totalled', async (t) => {\n  await t.test('a discount is applied', () => { assert.equal(price(), DISCOUNTED_PRICE); });\n  assert.ok(true);\n});\n";
        let findings = findings_of(&[node_file(own_assertion_is_not_the_childs)]);
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == LintRule::TrivialAssertion
                    && finding.test == "a cart is totalled"),
            "the parent's own trivial assertion is found and attributed to the parent: {findings:#?}"
        );
    }
}
