//! Context packs: what an attempt starts from, assembled by Kaisa (architecture §5.9, P18).
//!
//! A pack is a list of sections in priority order. Its canonical bytes are deterministic, so the
//! same inputs always give the same pack and the same address.

use kaisa_domain::{ContentDigest, WriteSet};

use crate::profile::{Confirmation, Language, ProjectProfile, Purpose};

/// Header of the canonical encoding; changes only with an incompatible format.
const FORMAT_HEADER: &[u8] = b"kaisa-context-pack v1\n";

/// A part of a context pack, in priority order: earlier kinds are never cut before later ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SectionKind {
    /// What to change and why.
    Spec,
    /// How the result will be judged.
    Acceptance,
    /// The paths the attempt may change.
    WriteSet,
    /// Languages and confirmed commands.
    Profile,
    /// Quoted excerpts of the repository's instruction files.
    Instructions,
}

impl SectionKind {
    /// Stable code used in the canonical encoding.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::Acceptance => "acceptance",
            Self::WriteSet => "write_set",
            Self::Profile => "profile",
            Self::Instructions => "instructions",
        }
    }
}

/// One section of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Which part of the pack this is.
    pub kind: SectionKind,
    /// Its text. Instruction excerpts are stored raw here and quoted when rendered.
    pub text: String,
}

/// What a pack is built from.
#[derive(Debug, Clone, Copy)]
pub struct PackInputs<'inputs> {
    /// The Task's spec.
    pub spec: &'inputs str,
    /// The Task's acceptance criteria, one per entry.
    pub acceptance: &'inputs [String],
    /// The paths the Task may change.
    pub write_set: &'inputs WriteSet,
    /// The repository's profile.
    pub profile: &'inputs ProjectProfile,
}

/// The context an attempt starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    sections: Vec<Section>,
}

impl ContextPack {
    /// Assembles a pack with every section, in priority order.
    #[must_use]
    pub fn assemble(inputs: PackInputs<'_>) -> Self {
        let mut sections: Vec<Section> = vec![
            Section {
                kind: SectionKind::Spec,
                text: inputs.spec.to_owned(),
            },
            Section {
                kind: SectionKind::Acceptance,
                text: inputs
                    .acceptance
                    .iter()
                    .map(|criterion| format!("- {criterion}\n"))
                    .collect(),
            },
            Section {
                kind: SectionKind::WriteSet,
                text: inputs
                    .write_set
                    .entries()
                    .iter()
                    .map(|path| format!("{path}\n"))
                    .collect(),
            },
            Section {
                kind: SectionKind::Profile,
                text: profile_summary(inputs.profile),
            },
        ];
        sections.extend(inputs.profile.instructions.iter().map(|excerpt| Section {
            kind: SectionKind::Instructions,
            text: format!("{}\n{}", excerpt.path, excerpt.text),
        }));
        Self { sections }
    }

    /// The sections, in priority order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Deterministic bytes: a header, then each section as length-prefixed code and text.
    /// Length prefixes make the encoding unambiguous, so moving text between fields always
    /// changes the bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = FORMAT_HEADER.to_vec();
        for section in &self.sections {
            push_field(&mut bytes, section.kind.code().as_bytes());
            push_field(&mut bytes, section.text.as_bytes());
        }
        bytes
    }
}

impl ContextPack {
    /// The pack's content address: the blake3 digest of its canonical bytes. Equal packs have
    /// equal addresses, and an attempt records this to say exactly what it started from.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        let hex: String = blake3::hash(&self.canonical_bytes()).to_hex().to_string();
        ContentDigest::try_new(&hex).expect("blake3 hex is 64 lowercase hex characters")
    }
}

fn push_field(bytes: &mut Vec<u8>, field: &[u8]) {
    let length: u64 = u64::try_from(field.len()).unwrap_or(u64::MAX);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(field);
}

/// One line per language, then one line per confirmed command with the directory it runs in.
/// Unconfirmed commands are left out: an agent should only be told about checks that work.
fn profile_summary(profile: &ProjectProfile) -> String {
    let languages: Vec<&str> = profile
        .languages
        .iter()
        .map(|&language| language_name(language))
        .collect();
    let mut summary: String = format!("languages: {}\n", languages.join(", "));
    for command in profile
        .commands
        .iter()
        .filter(|command| command.confirmation == Confirmation::Confirmed)
    {
        let directory: &str = command
            .directory
            .as_ref()
            .map_or(".", kaisa_domain::RepoPath::as_str);
        summary.push_str(&format!(
            "{} ({directory}): {}\n",
            purpose_name(command.purpose),
            command.argv.join(" ")
        ));
    }
    summary
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::Rust => "Rust",
        Language::Node => "Node",
        Language::Python => "Python",
        Language::Go => "Go",
    }
}

fn purpose_name(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Build => "build",
        Purpose::Test => "test",
        Purpose::Lint => "lint",
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{RepoPath, WriteSet};

    use super::{ContextPack, PackInputs};
    use crate::profile::{Confirmation, DetectedCommand, Language, ProjectProfile, Purpose};

    const LOGIN_FIX_SPEC: &str = "Make the login test wait for the token refresh.";
    const RATE_LIMIT_SPEC: &str = "Add rate limiting to the login endpoint.";

    fn the_login_tests_pass() -> Vec<String> {
        vec!["the login tests pass".to_owned()]
    }

    fn the_auth_directory() -> WriteSet {
        WriteSet::new(vec![RepoPath::try_new("src/auth").expect("a valid path")])
    }

    fn a_rust_profile_with_confirmed_tests() -> ProjectProfile {
        ProjectProfile {
            languages: vec![Language::Rust],
            commands: vec![DetectedCommand {
                language: Language::Rust,
                purpose: Purpose::Test,
                argv: vec![
                    "cargo".to_owned(),
                    "test".to_owned(),
                    "--workspace".to_owned(),
                ],
                directory: None,
                confirmation: Confirmation::Confirmed,
            }],
            instructions: Vec::new(),
        }
    }

    fn a_pack_for(spec: &str) -> ContextPack {
        ContextPack::assemble(PackInputs {
            spec,
            acceptance: &the_login_tests_pass(),
            write_set: &the_auth_directory(),
            profile: &a_rust_profile_with_confirmed_tests(),
        })
    }

    #[test]
    fn the_same_inputs_give_the_same_pack_bytes() {
        assert_eq!(
            a_pack_for(LOGIN_FIX_SPEC).canonical_bytes(),
            a_pack_for(LOGIN_FIX_SPEC).canonical_bytes(),
            "a pack must be reproducible, or replaying a run would start from different context"
        );
    }

    #[test]
    fn a_pack_is_addressed_by_the_digest_of_its_bytes() {
        let pack: ContextPack = a_pack_for(LOGIN_FIX_SPEC);
        let expected: String = blake3::hash(&pack.canonical_bytes()).to_hex().to_string();

        assert_eq!(
            pack.digest().as_str(),
            expected,
            "anyone holding the bytes can check which pack an attempt used"
        );
    }

    #[test]
    fn changing_the_spec_changes_the_pack_bytes() {
        assert_ne!(
            a_pack_for(LOGIN_FIX_SPEC).canonical_bytes(),
            a_pack_for(RATE_LIMIT_SPEC).canonical_bytes(),
            "two different tasks must never share a pack"
        );
    }
}
