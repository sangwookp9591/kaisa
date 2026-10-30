//! Newtype identifiers for runs, tasks and attempts.
//!
//! The textual form is `<prefix>_<suffix>`, where the suffix is 1 to 32 lowercase ASCII
//! letters or digits. Generating new identifiers is the ledger's job; this module only
//! validates and formats them.

use std::fmt;
use std::str::FromStr;

/// Maximum length of the part after the prefix.
const MAX_SUFFIX_LEN: usize = 32;

/// Why a string is not a valid identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdError {
    /// The string does not start with the expected `<prefix>_`.
    WrongPrefix {
        /// The prefix that was expected, without the underscore.
        expected: &'static str,
    },
    /// The part after the prefix is empty or longer than 32 characters.
    BadLength,
    /// The part after the prefix contains a character other than `a-z` or `0-9`.
    BadCharacter,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix { expected } => write!(f, "identifier must start with `{expected}_`"),
            Self::BadLength => write!(
                f,
                "identifier suffix must be 1 to {MAX_SUFFIX_LEN} characters"
            ),
            Self::BadCharacter => write!(f, "identifier suffix may only contain a-z and 0-9"),
        }
    }
}

impl std::error::Error for IdError {}

/// Returns the validated suffix of `text`, or why it is invalid.
fn parse_suffix<'text>(text: &'text str, prefix: &'static str) -> Result<&'text str, IdError> {
    let suffix: &str = text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('_'))
        .ok_or(IdError::WrongPrefix { expected: prefix })?;
    if suffix.is_empty() || suffix.len() > MAX_SUFFIX_LEN {
        return Err(IdError::BadLength);
    }
    if !suffix
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(IdError::BadCharacter);
    }
    Ok(suffix)
}

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The textual prefix of this identifier kind, without the underscore.
            pub const PREFIX: &'static str = $prefix;

            /// Parses an identifier such as
            #[doc = concat!("`", $prefix, "_a1b2c3`.")]
            pub fn try_new(text: &str) -> Result<Self, IdError> {
                parse_suffix(text, Self::PREFIX)?;
                Ok(Self(text.to_owned()))
            }

            /// Returns the full textual form, including the prefix.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(text: &str) -> Result<Self, IdError> {
                Self::try_new(text)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a run: a goal with its plan, budget and policy.
    RunId,
    "run"
);
define_id!(
    /// Identifies a task: one unit of work inside a run.
    TaskId,
    "task"
);
define_id!(
    /// Identifies an attempt: one execution of a task by one agent.
    AttemptId,
    "att"
);

#[cfg(test)]
mod tests {
    use super::{AttemptId, IdError, RunId, TaskId};

    #[test]
    fn parses_and_formats_valid_identifiers() {
        let run: RunId = "run_41".parse().expect("valid run id should parse");
        assert_eq!(
            run.as_str(),
            "run_41",
            "as_str must return the original text"
        );
        assert_eq!(run.to_string(), "run_41", "Display must round-trip");

        let task = TaskId::try_new("task_a1b2c3").expect("valid task id should parse");
        assert_eq!(task.to_string(), "task_a1b2c3", "Display must round-trip");
    }

    #[test]
    fn rejects_the_wrong_prefix() {
        assert_eq!(
            RunId::try_new("task_41"),
            Err(IdError::WrongPrefix { expected: "run" }),
            "a task id must not parse as a run id"
        );
        assert_eq!(
            AttemptId::try_new("att41"),
            Err(IdError::WrongPrefix { expected: "att" }),
            "the underscore after the prefix is required"
        );
    }

    #[test]
    fn rejects_bad_suffixes() {
        assert_eq!(
            RunId::try_new("run_"),
            Err(IdError::BadLength),
            "empty suffix"
        );
        let too_long = format!("run_{}", "a".repeat(33));
        assert_eq!(
            RunId::try_new(&too_long),
            Err(IdError::BadLength),
            "33-character suffix"
        );
        assert_eq!(
            RunId::try_new("run_AB"),
            Err(IdError::BadCharacter),
            "uppercase is invalid"
        );
        assert_eq!(
            RunId::try_new("run_a-b"),
            Err(IdError::BadCharacter),
            "dash is invalid"
        );
    }
}
