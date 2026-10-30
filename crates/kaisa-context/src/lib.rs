//! What Kaisa knows about a project before any agent starts: its profile, and later its index
//! and the context packs built from them (architecture §5.9, P18).

mod profile;

pub use profile::{Confirmation, DetectedCommand, Language, ProjectProfile, Purpose};
