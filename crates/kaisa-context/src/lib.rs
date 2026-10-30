//! What Kaisa knows about a project before any agent starts: its profile, and later its index
//! and the context packs built from them (architecture §5.9, P18).

mod detect;
mod profile;
#[cfg(test)]
mod test_support;

pub use detect::derive_profile;
pub use profile::{Confirmation, DetectedCommand, Language, ProjectProfile, Purpose};
