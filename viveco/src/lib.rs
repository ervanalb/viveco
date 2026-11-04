//! The Viveco vector-art and animation asset format.
//!
//! This crate owns the format's object model ([`Asset`]), reading and writing
//! it as JSON, and the operations other tools need to inspect or edit an
//! asset:
//!
//! - [`Asset`]: the complete asset, its JSON encoding, and its shape/bone
//!   addressing.
//! - shapes: [`Shape`], [`Path`], [`Segment`], styling, and vertex weights.
//! - bones: [`Bone`], the skeleton tree, and rig editing.
//! - poses: [`Pose`] and the linear-blend skinning it applies to artwork.
//! - animation: [`Animation`] clips, keyframes, sampling, and playback.
//!
//! All of these types live at the crate root.

mod animation;
mod asset;
mod bone;
mod pose;
mod shape;

pub use animation::*;
pub use asset::*;
pub use bone::*;
pub use pose::*;
pub use shape::*;
