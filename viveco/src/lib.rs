//! The Viveco vector-art and animation asset format.
//!
//! This crate owns the format's object model ([`Asset`]), reading and writing
//! it as JSON or in a postcard-based binary encoding, and the operations other
//! tools need to inspect or edit an asset:
//!
//! - [`Asset`]: the complete asset, its encodings, and its shape/bone
//!   addressing.
//! - shapes: [`Shape`], [`Path`], [`Segment`], styling, and vertex weights.
//! - bones: [`Bone`], the skeleton tree, and rig editing.
//! - poses: [`Pose`] and the linear-blend skinning it applies to artwork.
//! - animation: [`Animation`], [`AnimationInstance`], keyframes, and sampling.
//! - rendering: [`render`] converts assets into Vello scenes when the
//!   `vello` feature is enabled.
//! - Bevy: the `bevy` feature adds an asset loader, an instance component that
//!   draws into an explicitly spawned, Z-ordered scene, and a rendering plugin
//!   (see [`bevy`]).
//!
//! All of these types live at the crate root.

mod animation;
mod asset;
mod bone;
mod pose;
mod shape;

#[cfg(feature = "vello")]
pub mod render;

#[cfg(feature = "bevy")]
pub mod bevy;

pub use animation::*;
pub use asset::*;
pub use bone::*;
pub use pose::*;
pub use shape::*;

#[cfg(feature = "bevy")]
pub use self::bevy::{
    VivecoDefaultScene, VivecoInstance, VivecoLoader, VivecoPlugin, VivecoScene,
    VivecoSceneMembers, VivecoSceneOf, VivecoStrokeWidthInPixels,
};
