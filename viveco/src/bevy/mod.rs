//! Bevy integration for Viveco assets.
//!
//! This module is enabled by the crate's `bevy` feature. [`VivecoPlugin`]
//! registers an asset loader for `.viveco` files and adds the systems that
//! paint every [`VivecoAnimator`] into the [`VivecoScene`] it refers to. By
//! default that is one shared scene: animators' artwork is Z-ordered together
//! and each entity's [`GlobalTransform`](bevy::prelude::GlobalTransform) is
//! baked into it. Animators can target other scenes with [`VivecoSceneOf`], so
//! a game can render independent scenes (for example, one per camera or render
//! layer).
//!
//! The animator contains only a handle to the loaded [`Asset`](crate::Asset)
//! and an animation mixer: a list of [`AnimationInstance`]s
//! `(animation index, time, weight)`. The
//! heavier steps are ordinary library calls, so a caller that needs different
//! behavior can replace the component: sample a pose with
//! [`crate::Asset::sample_pose`], deform the artwork with
//! [`crate::Asset::deformed`], and combine assets with
//! [`SceneBuilder`](crate::render::SceneBuilder) (or build one scene with
//! [`crate::Asset::into_scene`]).

mod loader;
mod plugin;

pub use crate::AnimationInstance;
pub use loader::VivecoLoader;
pub use plugin::{
    VivecoAnimator, VivecoDefaultScene, VivecoPlugin, VivecoScene, VivecoSceneMembers,
    VivecoSceneOf,
};

pub mod prelude {
    pub use super::{AnimationInstance, VivecoAnimator, VivecoPlugin, VivecoScene, VivecoSceneOf};
}
