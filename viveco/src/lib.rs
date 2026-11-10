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
//! - poses: [`Pose`], its resolved [`WorldPose`], and the linear-blend
//!   skinning they apply to artwork.
//! - animation: [`Animation`], [`AnimationInstance`], keyframes, and sampling.
//! - rendering: [`render`] converts assets into Vello scenes when the
//!   `vello` feature is enabled.
//! - SVG import: [`svg`] imports the shapes of an SVG document when the `svg`
//!   feature is enabled. It is independent of `bevy`, so the Bevy plugin does
//!   not compile `usvg`.
//! - conversion: [`convert`] imports SVG documents and rewrites assets in
//!   either encoding, including from build scripts.
//! - Bevy: the `bevy` feature adds an asset loader, an immediate-mode scene
//!   built from Z-sorted fragments, a producer for animated instances, a
//!   parallax system, and a rendering plugin (see [`bevy`]).
//! - build scripts: [`codegen`] emits animation indices and shape/bone name
//!   constants from an asset file at build time.
//!
//! All of these types live at the crate root.

mod animation;
mod asset;
mod bone;
mod pose;
mod shape;

pub mod codegen;

#[cfg(feature = "vello")]
pub mod render;

#[cfg(feature = "svg")]
pub mod svg;

#[cfg(feature = "svg")]
pub mod convert;

#[cfg(feature = "bevy")]
pub mod bevy;

pub use animation::*;
pub use asset::*;
pub use bone::*;
pub use pose::*;
pub use shape::*;

#[cfg(feature = "bevy")]
pub use self::bevy::{
    ParallaxLayer, SceneFragment, VivecoDefaultScene, VivecoInstance, VivecoLoader, VivecoPlugin,
    VivecoScene, VivecoSceneMembers, VivecoSceneOf, VivecoSceneSystems, VivecoStrokeWidthInPixels,
    apply_parallax, scene_affine,
};
