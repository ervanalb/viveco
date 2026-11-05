//! The animator component and the systems that render it.

use ::bevy::asset::AssetEvent;
use ::bevy::camera::visibility::NoFrustumCulling;
use ::bevy::prelude::*;
use ::bevy_vello::prelude::VelloScene2d;

use super::VivecoLoader;

/// One animation sample: `(animation index, time in seconds, weight)`.
pub type AnimationSample = (usize, f32, f32);

/// Renders a loaded [`Asset`](crate::Asset) by blending animations at the
/// given times.
///
/// The document is sampled at each clip's time, the poses are blended by
/// weight, and the resulting deformation is rebuilt into a Vello scene whenever
/// this component or the entity's [`Transform`] changes. Both non-cyclic and
/// cyclic clips use the sampling behavior of
/// [`Animation::sample_pose`](crate::Animation::sample_pose).
///
/// A user who wants a different pose or rendering path can spawn their own
/// component instead: the underlying steps are ordinary library calls.
#[derive(Component, Clone, Debug)]
#[require(VelloScene2d, NoFrustumCulling)]
pub struct VivecoAnimator {
    /// The loaded document to render.
    pub asset: Handle<crate::Asset>,
    /// The animation clips to blend, as `(animation index, time, weight)`.
    pub clips: Vec<AnimationSample>,
}

impl VivecoAnimator {
    /// Creates an animator for `asset` with the given clips.
    pub fn new(asset: Handle<crate::Asset>, clips: Vec<AnimationSample>) -> Self {
        Self { asset, clips }
    }

    /// Creates an animator that plays only the animation at `index`.
    pub fn from_animation(asset: Handle<crate::Asset>, index: usize) -> Self {
        Self::new(asset, vec![(index, 0.0, 1.0)])
    }
}

/// Registers the asset loader and the rendering systems.
pub struct VivecoPlugin;

impl Plugin for VivecoPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<crate::Asset>()
            .init_asset_loader::<VivecoLoader>()
            .add_systems(
                PostUpdate,
                (mark_reloaded_animators, update_viveco_scenes).chain(),
            );
    }
}

/// Makes animators rebuild when their asynchronously loaded asset arrives or
/// changes. Change detection on the component only sees edits to the clips.
fn mark_reloaded_animators(
    mut events: MessageReader<AssetEvent<crate::Asset>>,
    mut animators: Query<&mut VivecoAnimator>,
) {
    for event in events.read() {
        let id = match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::LoadedWithDependencies { id } => *id,
            _ => continue,
        };
        for mut animator in &mut animators {
            if animator.asset.id() == id {
                animator.set_changed();
            }
        }
    }
}

/// Samples the animator's clips, deforms the artwork, and replaces the scene.
// Bevy system signatures are inherently verbose; this query is still the
// clearest way to express the three components the update needs.
#[allow(clippy::type_complexity)]
fn update_viveco_scenes(
    mut animators: Query<
        (&VivecoAnimator, &Transform, &mut VelloScene2d),
        Or<(Changed<VivecoAnimator>, Changed<Transform>)>,
    >,
    assets: Res<Assets<crate::Asset>>,
) {
    for (animator, transform, mut scene) in &mut animators {
        let Some(asset) = assets.get(&animator.asset) else {
            continue;
        };
        let scale = transform.scale.x.abs().max(transform.scale.y.abs());
        if scale <= f32::EPSILON {
            continue;
        }

        let pose = asset.sample_pose(&animator.clips);
        let deformed = asset.deformed(&pose);
        let stroke_width = 1.0 / scale;
        *scene = VelloScene2d::from(deformed.into_scene(stroke_width));
    }
}
