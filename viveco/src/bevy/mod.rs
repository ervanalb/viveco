//! Bevy integration for Viveco assets.
//!
//! This module is enabled by the crate's `bevy` feature. [`VivecoPlugin`]
//! registers an asset loader for `.viveco` files and adds the systems that turn
//! a [`VivecoAnimator`] into a rendered
//! [`VelloScene2d`](bevy_vello::prelude::VelloScene2d).
//!
//! The animator contains only a handle to the loaded [`Asset`](crate::Asset)
//! and a list of `(animation index, time, weight)` samples. The heavier steps
//! are ordinary library calls, so a caller that needs different behavior can
//! replace the component: sample a pose with [`crate::Asset::sample_pose`],
//! deform the artwork with [`crate::Asset::deformed`], and build a scene with
//! [`crate::Asset::into_scene`].

mod loader;
mod plugin;

pub use loader::{VivecoLoader, VivecoLoaderError};
pub use plugin::{AnimationSample, VivecoAnimator, VivecoPlugin};

pub mod prelude {
    pub use super::{AnimationSample, VivecoAnimator, VivecoPlugin};
}
