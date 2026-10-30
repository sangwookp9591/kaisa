//! KRPC message types and framing shared by the daemon and its clients.
//!
//! Design: `docs/architecture.md` §6. Kaisa has exactly one protocol version; new
//! operations are negotiated through capability lists, not new versions.

mod frame;
mod hello;

pub use frame::{DEFAULT_FRAME_LIMIT, FRAME_TERMINATOR, FrameError, FrameReader, write_frame};
pub use hello::{Capability, HandshakeError, Hello, Session, negotiate};

/// The KRPC protocol version spoken by this build.
pub const PROTOCOL_VERSION: u32 = 1;
