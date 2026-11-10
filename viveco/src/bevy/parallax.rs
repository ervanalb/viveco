//! Depth-based parallax for 2D layers.
//!
//! Every layer renders under one orthographic `Camera2d`. A zero-sized
//! [`ParallaxLayer`] marker opts a vello scene into [`apply_parallax`], which
//! derives its scale and offset from the layer's Z and the camera's position,
//! emulating the way a perspective camera would treat planes at different
//! depths.
//!
//! Run [`apply_parallax`] after whatever system moves the camera:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use viveco::{ParallaxLayer, apply_parallax};
//! # fn move_camera() {}
//! # let mut app = App::new();
//! app.add_systems(Update, (move_camera, apply_parallax).chain());
//! ```

use ::bevy::prelude::*;
use ::bevy_vello::prelude::VelloView;

/// Marks an entity as a parallax layer.
///
/// The entity's Z is its depth relative to the camera: negative is farther
/// away. Add it to the same entity as a [`VivecoScene`](crate::VivecoScene), or
/// to any other transform hierarchy that should follow the camera at a depth.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct ParallaxLayer;

/// Scales and offsets each [`ParallaxLayer`] as a perspective camera would.
///
/// The camera's Z sets the eye distance, so a
/// layer at Z has scale `camera_z / (camera_z - layer_z)`. The layer is
/// repositioned about the camera's view center as well, which makes far layers
/// follow the camera more slowly than near ones.
///
/// The system reads the single `Camera2d` with `VelloView` and does nothing if
/// there is not exactly one. Run it after the camera has moved.
pub fn apply_parallax(
    camera: Query<&Transform, (With<Camera2d>, With<VelloView>)>,
    mut layers: Query<&mut Transform, (With<ParallaxLayer>, Without<Camera2d>)>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };

    for mut layer in &mut layers {
        let depth = camera.translation.z - layer.translation.z;
        // A layer at the camera's own depth has no perspective projection.
        if depth.abs() <= f32::EPSILON {
            continue;
        }
        let scale = camera.translation.z / depth;

        // Content is authored around the layer origin, so this both scales it
        // about the camera's view center and makes far layers follow the camera.
        layer.translation.x = camera.translation.x * (1.0 - scale);
        layer.translation.y = camera.translation.y * (1.0 - scale);
        layer.scale = Vec3::splat(scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer_transforms(layers: &[(f32, Transform)]) -> Vec<Transform> {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, apply_parallax);
        app.world_mut()
            .spawn((Camera2d, VelloView, Transform::from_xyz(400.0, 100.0, 10.0)));
        let entities: Vec<Entity> = layers
            .iter()
            .map(|(z, transform)| {
                let mut transform = *transform;
                transform.translation.z = *z;
                app.world_mut().spawn((ParallaxLayer, transform)).id()
            })
            .collect();

        app.update();

        entities
            .into_iter()
            .map(|entity| *app.world().get::<Transform>(entity).unwrap())
            .collect()
    }

    #[test]
    fn scales_layers_by_their_distance_from_the_camera() {
        let transforms = layer_transforms(&[
            (-3.0, Transform::default()),
            (0.0, Transform::default()),
            (3.0, Transform::default()),
        ]);

        // Camera z = 10; depth 13, 10, and 7.
        assert!((transforms[0].scale.x - 10.0 / 13.0).abs() < 1e-6);
        assert!((transforms[1].scale.x - 1.0).abs() < 1e-6);
        assert!((transforms[2].scale.x - 10.0 / 7.0).abs() < 1e-6);

        // Far layers drift with the camera least; the mid layer is unmoved.
        assert!((transforms[0].translation.x - 400.0 * (1.0 - 10.0 / 13.0)).abs() < 1e-4);
        assert_eq!(transforms[1].translation, Vec3::ZERO);
        assert!((transforms[2].translation.x - 400.0 * (1.0 - 10.0 / 7.0)).abs() < 1e-4);
    }

    #[test]
    fn leaves_a_layer_at_the_camera_depth_alone() {
        let transform = Transform::from_xyz(7.0, 8.0, 10.0);
        let after = layer_transforms(&[(10.0, transform)])[0];
        assert_eq!(after, transform);
    }

    #[test]
    fn does_nothing_without_a_camera() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, apply_parallax);
        let transform = Transform::from_xyz(7.0, 8.0, 9.0);
        let layer = app.world_mut().spawn((ParallaxLayer, transform)).id();

        app.update();

        assert_eq!(*app.world().get::<Transform>(layer).unwrap(), transform);
    }
}
