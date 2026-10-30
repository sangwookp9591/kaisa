//! What Kaisa knows about a project before any agent starts: its profile, and later its index
//! and the context packs built from them (architecture §5.9, P18).

mod confirm;
mod detect;
mod profile;
#[cfg(test)]
mod test_support;

pub use confirm::confirm;
pub use detect::{INSTRUCTION_EXCERPT_LIMIT, derive_profile};
pub use profile::{
    Confirmation, DetectedCommand, InstructionExcerpt, Language, ProjectProfile, Purpose,
};
