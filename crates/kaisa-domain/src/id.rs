//! Newtype identifiers for runs, tasks and attempts.
//!
//! The textual form is `<prefix>_<suffix>`, where the suffix is 1 to 32 lowercase ASCII
//! letters or digits. Generating new identifiers is the ledger's job; this module only
//! validates and formats them.

use std::fmt;
use std::str::FromStr;

/// Maximum length of the part after the prefix.
pub const MAX_SUFFIX_LEN: usize = 32;

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
    use super::{AttemptId, IdError, MAX_SUFFIX_LEN, RunId, TaskId};

    /// Text for a run id whose suffix has exactly `length` valid characters.
    fn run_id_text_with_suffix_of_length(length: usize) -> String {
        format!("{}_{}", RunId::PREFIX, "a".repeat(length))
    }

    /// The text of the 41st run, as a user would type it.
    fn run_forty_one() -> &'static str {
        "run_41"
    }

    #[test]
    fn a_run_id_with_a_valid_suffix_round_trips_through_display() {
        let typed_by_user: &str = run_forty_one();

        let run: RunId = typed_by_user.parse().expect("a valid run id must parse");

        assert_eq!(
            run.as_str(),
            typed_by_user,
            "as_str must return exactly what was typed"
        );
        assert_eq!(
            run.to_string(),
            typed_by_user,
            "Display must print exactly what was typed"
        );
    }

    #[test]
    fn a_task_id_with_letters_and_digits_is_accepted() {
        let task_text: String = format!("{}_a1b2c3", TaskId::PREFIX);

        let task: Result<TaskId, IdError> = TaskId::try_new(&task_text);

        assert!(
            task.is_ok(),
            "lowercase letters and digits are valid suffix characters"
        );
    }

    #[test]
    fn a_task_id_is_rejected_where_a_run_id_is_expected() {
        let task_text: String = format!("{}_41", TaskId::PREFIX);

        let parsed: Result<RunId, IdError> = RunId::try_new(&task_text);

        assert_eq!(
            parsed,
            Err(IdError::WrongPrefix {
                expected: RunId::PREFIX
            }),
            "ids of one kind must never be accepted as another kind"
        );
    }

    #[test]
    fn an_attempt_id_without_the_underscore_separator_is_rejected() {
        let missing_separator: String = format!("{}41", AttemptId::PREFIX);

        let parsed: Result<AttemptId, IdError> = AttemptId::try_new(&missing_separator);

        assert_eq!(
            parsed,
            Err(IdError::WrongPrefix {
                expected: AttemptId::PREFIX
            }),
            "the underscore after the prefix is part of the format"
        );
    }

    #[test]
    fn an_empty_suffix_is_rejected_as_bad_length() {
        let empty_suffix: String = run_id_text_with_suffix_of_length(0);

        assert_eq!(
            RunId::try_new(&empty_suffix),
            Err(IdError::BadLength),
            "an id must name something after its prefix"
        );
    }

    #[test]
    fn a_suffix_of_exactly_the_maximum_length_is_accepted() {
        let longest_allowed: String = run_id_text_with_suffix_of_length(MAX_SUFFIX_LEN);

        assert!(
            RunId::try_new(&longest_allowed).is_ok(),
            "the maximum length itself is inside the allowed range"
        );
    }

    #[test]
    fn a_suffix_longer_than_the_maximum_is_rejected_as_bad_length() {
        let one_too_long: String = run_id_text_with_suffix_of_length(MAX_SUFFIX_LEN + 1);

        assert_eq!(
            RunId::try_new(&one_too_long),
            Err(IdError::BadLength),
            "ids are bounded so they stay readable and cheap to store"
        );
    }

    #[test]
    fn a_suffix_with_uppercase_letters_is_rejected_as_bad_character() {
        let shouting: String = format!("{}_AB", RunId::PREFIX);

        assert_eq!(
            RunId::try_new(&shouting),
            Err(IdError::BadCharacter),
            "ids are lowercase so they compare and display the same everywhere"
        );
    }

    #[test]
    fn a_suffix_with_a_dash_is_rejected_as_bad_character() {
        let dashed: String = format!("{}_a-b", RunId::PREFIX);

        assert_eq!(
            RunId::try_new(&dashed),
            Err(IdError::BadCharacter),
            "only letters and digits are allowed so ids are safe in paths and branch names"
        );
    }
}
