//! Runtime support for the `viveco` binary format.
//!
//! The binary is produced at compile time by the [`viveco`](viveco_macros::viveco)
//! proc macro from a `.json` document. It is a flat, little-endian,
//! zero-copy representation: every name is replaced by an array index, and every
//! list is a `(start, count)` range into a shared array. Parsing therefore
//! borrows directly from the embedded bytes and performs no allocation.
//!
//! This crate has no Bevy dependency. It contains the format reader, the
//! animation sampler, linear-blend skinning, and the conversion of a posed
//! document into a [`vello::Scene`]. The `viveco-bevy` crate wraps this in an
//! asset loader and ECS components.
//!
//! # Layout
//!
//! All sections are 4-byte aligned and stored in this order:
//!
//! ```text
//! header, shapes, paths, segments, weights, child indices,
//! bones, animations, curves, keyframes
//! ```
//!
//! See [`format`] for field-level details.

pub mod anim;
pub mod error;
pub mod format;
pub mod pose;
pub mod scene;

pub use anim::sample_curve;
pub use error::FormatError;
pub use format::{
    AnimationRef, BoneRef, Bounds, CurveRef, KeyframeRef, PathRef, SegmentRef, ShapeRef, Viveco,
    WeightRef,
};
pub use pose::{BonePose, Layer, Pose, Similarity, bone_worlds, sample_pose};
pub use scene::build_scene;
