//! NDJSON framing: one JSON value per line (architecture §6).
//!
//! Compact JSON never contains a raw newline (newlines inside strings are escaped as `\n`), so
//! a single `\n` byte is an unambiguous frame terminator.

use std::fmt;
use std::io::{self, Write};

use serde::Serialize;

/// The byte that ends every frame.
pub const FRAME_TERMINATOR: u8 = b'\n';

/// Why a frame could not be written or read.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
    /// The underlying stream failed.
    Io(io::Error),
    /// The value could not be encoded, or the frame is not valid JSON for the expected type.
    Malformed(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "frame stream error: {error}"),
            Self::Malformed(error) => write!(f, "malformed frame: {error}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Malformed(error) => Some(error),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Writes `message` as one compact JSON line, terminated by [`FRAME_TERMINATOR`].
///
/// # Errors
///
/// Returns [`FrameError::Malformed`] if `message` cannot be encoded, or [`FrameError::Io`] if
/// writing fails.
pub fn write_frame<M: Serialize>(writer: &mut dyn Write, message: &M) -> Result<(), FrameError> {
    let mut line: Vec<u8> = serde_json::to_vec(message).map_err(FrameError::Malformed)?;
    line.push(FRAME_TERMINATOR);
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use serde::{Deserialize, Serialize};

    /// A client asking for the status of a run, with a free-text note.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub(crate) struct StatusRequest {
        pub(crate) run: String,
        pub(crate) note: String,
    }

    /// "What is run 41 doing?"
    pub(crate) fn status_request_for_run_41() -> StatusRequest {
        StatusRequest {
            run: "run_41".to_owned(),
            note: "from kaisa watch".to_owned(),
        }
    }

    /// The same request with a note the user typed over two lines.
    pub(crate) fn status_request_with_a_two_line_note() -> StatusRequest {
        StatusRequest {
            run: "run_41".to_owned(),
            note: "first line\nsecond line".to_owned(),
        }
    }

    /// Bytes `message` would occupy on the wire.
    pub(crate) fn written(message: &StatusRequest) -> Vec<u8> {
        let mut wire: Vec<u8> = Vec::new();
        super::write_frame(&mut wire, message).expect("writing to memory succeeds");
        wire
    }
}

#[cfg(test)]
mod tests {
    use super::FRAME_TERMINATOR;
    use super::test_support::{
        status_request_for_run_41, status_request_with_a_two_line_note, written,
    };

    /// A frame is a single line.
    const TERMINATORS_PER_FRAME: usize = 1;

    fn terminators_in(wire: &[u8]) -> usize {
        wire.iter()
            .filter(|byte| **byte == FRAME_TERMINATOR)
            .count()
    }

    #[test]
    fn a_written_frame_ends_with_exactly_one_newline() {
        let wire: Vec<u8> = written(&status_request_for_run_41());

        assert_eq!(
            wire.last(),
            Some(&FRAME_TERMINATOR),
            "a frame ends at its terminator"
        );
        assert_eq!(
            terminators_in(&wire),
            TERMINATORS_PER_FRAME,
            "a frame contains no other newline, so readers can split on it"
        );
    }

    #[test]
    fn a_frame_containing_a_newline_in_a_string_is_escaped_on_one_line() {
        let wire: Vec<u8> = written(&status_request_with_a_two_line_note());

        assert_eq!(
            terminators_in(&wire),
            TERMINATORS_PER_FRAME,
            "newlines inside strings are escaped, never written raw"
        );
    }
}
