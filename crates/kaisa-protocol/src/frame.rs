//! NDJSON framing: one JSON value per line (architecture §6).
//!
//! Compact JSON never contains a raw newline (newlines inside strings are escaped as `\n`), so
//! a single `\n` byte is an unambiguous frame terminator.

use std::fmt;
use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The byte that ends every frame.
pub const FRAME_TERMINATOR: u8 = b'\n';

/// Default upper bound on the bytes of one frame, excluding its terminator.
pub const DEFAULT_FRAME_LIMIT: usize = 1024 * 1024;

/// Why a frame could not be written or read.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
    /// The underlying stream failed.
    Io(io::Error),
    /// The value could not be encoded, or the frame is not valid JSON for the expected type.
    Malformed(serde_json::Error),
    /// A frame exceeded the reader's byte limit. The reader stopped reading and must not be
    /// used again, because the rest of the oversized frame is still in the stream.
    TooLarge {
        /// The limit that was exceeded.
        limit: usize,
    },
    /// The stream ended in the middle of a frame.
    Truncated,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "frame stream error: {error}"),
            Self::Malformed(error) => write!(f, "malformed frame: {error}"),
            Self::TooLarge { limit } => write!(f, "frame exceeds the {limit}-byte limit"),
            Self::Truncated => write!(f, "stream ended in the middle of a frame"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Malformed(error) => Some(error),
            Self::TooLarge { .. } | Self::Truncated => None,
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

/// Reads NDJSON frames from a buffered stream, refusing frames above a byte limit.
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    limit: usize,
}

impl<R: BufRead> FrameReader<R> {
    /// Reads from `inner`, accepting frames of at most `limit` bytes before the terminator.
    pub fn new(inner: R, limit: usize) -> Self {
        Self { inner, limit }
    }

    /// Gives back the underlying stream.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Reads the next frame and decodes it as `M`. Returns `Ok(None)` when the stream ends
    /// cleanly between frames.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::TooLarge`] as soon as the frame exceeds the limit, without reading
    /// the rest of it; [`FrameError::Truncated`] if the stream ends inside a frame;
    /// [`FrameError::Malformed`] if the frame is not valid JSON for `M`; [`FrameError::Io`] if
    /// reading fails.
    pub fn read_frame<M: DeserializeOwned>(&mut self) -> Result<Option<M>, FrameError> {
        let mut frame: Vec<u8> = Vec::new();
        loop {
            let available: &[u8] = self.inner.fill_buf()?;
            if available.is_empty() {
                return if frame.is_empty() {
                    Ok(None)
                } else {
                    Err(FrameError::Truncated)
                };
            }
            let terminator: Option<usize> =
                available.iter().position(|byte| *byte == FRAME_TERMINATOR);
            let body: &[u8] = &available[..terminator.unwrap_or(available.len())];
            if frame.len() + body.len() > self.limit {
                return Err(FrameError::TooLarge { limit: self.limit });
            }
            frame.extend_from_slice(body);
            let consumed: usize = terminator.map_or(available.len(), |at| at + 1);
            self.inner.consume(consumed);
            if terminator.is_some() {
                return serde_json::from_slice(&frame)
                    .map(Some)
                    .map_err(FrameError::Malformed);
            }
        }
    }
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

    /// A request whose note is `length` characters long, for size-limit tests.
    pub(crate) fn status_request_with_a_note_of_length(length: usize) -> StatusRequest {
        StatusRequest {
            run: "run_41".to_owned(),
            note: "n".repeat(length),
        }
    }

    /// The number of bytes before the terminator that `message` occupies on the wire.
    pub(crate) fn frame_body_length(message: &StatusRequest) -> usize {
        written(message).len() - 1
    }

    /// A stream that hands out its bytes a few at a time and remembers how far it got, so tests
    /// can see how much a reader actually pulled.
    pub(crate) struct TrickleStream {
        data: Vec<u8>,
        position: usize,
        chunk: usize,
        handed_out: usize,
    }

    impl TrickleStream {
        /// A stream over `data` that hands out at most `chunk` bytes per `fill_buf`.
        pub(crate) fn new(data: Vec<u8>, chunk: usize) -> Self {
            Self {
                data,
                position: 0,
                chunk,
                handed_out: 0,
            }
        }

        /// The furthest byte offset the reader has been shown.
        pub(crate) fn bytes_handed_out(&self) -> usize {
            self.handed_out
        }
    }

    impl std::io::Read for TrickleStream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let available: &[u8] = std::io::BufRead::fill_buf(self)?;
            let copied: usize = available.len().min(buffer.len());
            buffer[..copied].copy_from_slice(&available[..copied]);
            std::io::BufRead::consume(self, copied);
            Ok(copied)
        }
    }

    impl std::io::BufRead for TrickleStream {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            let end: usize = (self.position + self.chunk).min(self.data.len());
            self.handed_out = self.handed_out.max(end);
            Ok(&self.data[self.position..end])
        }

        fn consume(&mut self, amount: usize) {
            self.position += amount;
        }
    }

    /// The bytes of a frame whose writer died before writing the terminator.
    pub(crate) fn a_frame_cut_off_before_its_terminator() -> Vec<u8> {
        let mut wire: Vec<u8> = written(&status_request_for_run_41());
        wire.pop();
        wire
    }

    /// A complete line that is not JSON, as a confused or hostile peer might send.
    pub(crate) fn a_line_of_plain_text() -> Vec<u8> {
        b"hello daemon, are you there?\n".to_vec()
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
    use super::test_support::{
        StatusRequest, TrickleStream, a_frame_cut_off_before_its_terminator, a_line_of_plain_text,
        frame_body_length, status_request_for_run_41, status_request_with_a_note_of_length,
        status_request_with_a_two_line_note, written,
    };
    use super::{DEFAULT_FRAME_LIMIT, FRAME_TERMINATOR, FrameError, FrameReader};

    /// How many bytes the trickle stream hands out per read in these tests.
    const TRICKLE_CHUNK: usize = 16;

    /// A limit small enough to exceed with a readable test message.
    const SMALL_LIMIT: usize = 256;

    fn reader_over(wire: Vec<u8>, limit: usize) -> FrameReader<TrickleStream> {
        FrameReader::new(TrickleStream::new(wire, TRICKLE_CHUNK), limit)
    }

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

    #[test]
    fn a_written_frame_is_read_back_unchanged() {
        let request: StatusRequest = status_request_with_a_two_line_note();
        let mut reader = reader_over(written(&request), DEFAULT_FRAME_LIMIT);

        let read: Option<StatusRequest> = reader.read_frame().expect("a valid frame reads");

        assert_eq!(
            read,
            Some(request),
            "what one side writes the other side reads"
        );
    }

    #[test]
    fn two_frames_written_back_to_back_are_read_as_two_messages() {
        let first: StatusRequest = status_request_for_run_41();
        let second: StatusRequest = status_request_with_a_two_line_note();
        let mut wire: Vec<u8> = written(&first);
        wire.extend(written(&second));
        let mut reader = reader_over(wire, DEFAULT_FRAME_LIMIT);

        let read_first: Option<StatusRequest> = reader.read_frame().expect("the first frame reads");
        let read_second: Option<StatusRequest> =
            reader.read_frame().expect("the second frame reads");

        assert_eq!(read_first, Some(first), "frames are not merged");
        assert_eq!(
            read_second,
            Some(second),
            "the second frame starts after the first terminator"
        );
    }

    #[test]
    fn a_frame_exactly_at_the_limit_is_accepted() {
        let request: StatusRequest = status_request_for_run_41();
        let limit: usize = frame_body_length(&request);
        let mut reader = reader_over(written(&request), limit);

        let read: Option<StatusRequest> = reader.read_frame().expect("a frame at the limit reads");

        assert_eq!(read, Some(request), "the limit itself is allowed");
    }

    #[test]
    fn a_frame_larger_than_the_limit_is_rejected() {
        let oversized: StatusRequest = status_request_with_a_note_of_length(SMALL_LIMIT);
        let mut reader = reader_over(written(&oversized), SMALL_LIMIT);

        let read: Result<Option<StatusRequest>, FrameError> = reader.read_frame();

        assert!(
            matches!(read, Err(FrameError::TooLarge { limit: SMALL_LIMIT })),
            "an oversized frame must be refused, got {read:?}"
        );
    }

    #[test]
    fn the_reader_stops_reading_once_the_limit_is_exceeded() {
        let oversized: StatusRequest =
            status_request_with_a_note_of_length(SMALL_LIMIT * TRICKLE_CHUNK);
        let wire: Vec<u8> = written(&oversized);
        let wire_length: usize = wire.len();
        let mut reader = reader_over(wire, SMALL_LIMIT);

        let _refused = reader.read_frame::<StatusRequest>();

        assert!(
            reader.into_inner().bytes_handed_out() <= SMALL_LIMIT + TRICKLE_CHUNK,
            "a peer must not be able to make the reader buffer a whole oversized frame \
             (the frame was {wire_length} bytes)"
        );
    }

    #[test]
    fn an_empty_stream_reports_a_clean_end() {
        let mut reader = reader_over(Vec::new(), DEFAULT_FRAME_LIMIT);

        let read: Option<StatusRequest> = reader
            .read_frame()
            .expect("an empty stream is not an error");

        assert_eq!(
            read, None,
            "a peer that closes before sending anything ended cleanly"
        );
    }

    #[test]
    fn the_end_of_stream_after_the_last_frame_is_a_clean_end() {
        let mut reader = reader_over(written(&status_request_for_run_41()), DEFAULT_FRAME_LIMIT);
        reader
            .read_frame::<StatusRequest>()
            .expect("the only frame reads");

        let after_last: Option<StatusRequest> = reader.read_frame().expect("the end is clean");

        assert_eq!(
            after_last, None,
            "closing after a complete frame is a normal shutdown"
        );
    }

    #[test]
    fn a_partial_frame_at_end_of_stream_is_reported_as_truncated() {
        let mut reader = reader_over(a_frame_cut_off_before_its_terminator(), DEFAULT_FRAME_LIMIT);

        let read: Result<Option<StatusRequest>, FrameError> = reader.read_frame();

        assert!(
            matches!(read, Err(FrameError::Truncated)),
            "a half-written frame must never be decoded as if it were complete, got {read:?}"
        );
    }

    #[test]
    fn a_line_that_is_not_json_is_reported_as_malformed() {
        let mut reader = reader_over(a_line_of_plain_text(), DEFAULT_FRAME_LIMIT);

        let read: Result<Option<StatusRequest>, FrameError> = reader.read_frame();

        assert!(
            matches!(read, Err(FrameError::Malformed(_))),
            "non-JSON input must be rejected, got {read:?}"
        );
    }
}
