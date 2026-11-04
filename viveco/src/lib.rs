//! The Viveco vector-art and animation document format.
//!
//! This crate owns the format's document object model ([`model`]), reading and
//! writing it as JSON, and the operations other tools need to inspect or edit a
//! document:
//!
//! - [`model`]: the complete document object model and its JSON encoding.
//! - [`rig`]: skeleton tree addressing and bone manipulation.
//! - [`pose`]: poses and the linear-blend skinning they apply to artwork.
//! - [`animation`]: sampling animation curves into poses.

pub mod animation;
pub mod model;
pub mod pose;
pub mod rig;
