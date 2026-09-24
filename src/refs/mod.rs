//! Git's files reference backend: names, direct/symbolic targets, and conditional updates/deletion.
//!
//! [`References`] reads loose refs before packed refs and separates symbolic resolution from
//! object lookup. Its explicitly named write methods omit reflogs; read their contracts before
//! using them in repositories whose recovery history matters.

mod enumerate;
mod name;
mod packed;
mod store;

pub use enumerate::Reference;
pub use name::{InvalidRefName, RefName};
pub use store::{Expected, ReferenceError, References, Resolution, Target};
