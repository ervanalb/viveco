//! End-to-end checks that the proc macro, binary reader, sampler, and scene
//! builder agree on the embedded stick document.

use viveco_bevy::*;

static STICK: &[u8] = viveco!("../stick.json");
static CHARACTER: &[u8] = viveco!("../character.json");
static EXAMPLE: &[u8] = viveco!("../example.json");

#[test]
fn embedded_document_has_expected_structure() {
    let art = Viveco::parse(STICK).expect("stick binary is valid");

    assert_eq!(art.shape_count(), 4);
    assert_eq!(art.bone_count(), 11);
    assert_eq!(art.animation_count(), 4);
    assert_eq!(art.root_shapes().count(), 4);

    let counts = art.counts();
    assert_eq!(counts.paths, 4);
    assert_eq!(counts.segments, 14);
    assert_eq!(counts.weights, 55);

    assert!(art.bounds().is_some());
}

#[test]
fn compiles_styles_and_nested_children() {
    let character = Viveco::parse(CHARACTER).expect("character binary is valid");
    assert_eq!(character.root_shapes().count(), 3);
    assert!(character.root_shapes().any(|shape| shape.fill().is_some()));
    assert!(
        character
            .root_shapes()
            .any(|shape| shape.stroke().is_some())
    );

    let example = Viveco::parse(EXAMPLE).expect("example binary is valid");
    assert_eq!(example.root_shapes().count(), 6);
    // The example document nests a child shape under a root.
    assert!(example.root_shapes().any(|shape| shape.has_children()));
    assert!(example.bounds().is_some());
}

#[test]
fn every_animation_samples_and_builds_a_scene() {
    let art = Viveco::parse(STICK).expect("stick binary is valid");

    for animation_index in 0..art.animation_count() {
        let animation = art.animation(animation_index).unwrap();
        assert!(
            animation.curve_count() > 0,
            "animation {animation_index} has no curves"
        );

        let first = sample_pose(&art, &[AnimationLayer::new(animation_index, 1.0, 0.0)]);
        let mut moved = false;

        for step in 0..=8 {
            let time = animation.duration() * step as f32 / 8.0;
            let pose = sample_pose(&art, &[AnimationLayer::new(animation_index, 1.0, time)]);
            assert_eq!(pose.len(), art.bone_count());
            moved |= pose != first;

            let mut scene = vello::Scene::new();
            build_scene(&art, &pose, &mut scene);
        }

        assert!(moved, "animation {animation_index} never changes the pose");
    }
}

#[test]
fn no_layers_produces_the_rest_pose() {
    let art = Viveco::parse(STICK).expect("stick binary is valid");

    let pose = sample_pose(&art, &[]);
    assert_eq!(pose.len(), art.bone_count());
    assert!(pose.locals().iter().all(|local| local.is_rest()));
}

#[test]
fn layers_add_over_rest() {
    let art = Viveco::parse(STICK).expect("stick binary is valid");

    let walk = sample_pose(&art, &[AnimationLayer::new(1, 1.0, 0.5)]);
    let arms = sample_pose(&art, &[AnimationLayer::new(2, 1.0, 0.5)]);
    let both = sample_pose(
        &art,
        &[
            AnimationLayer::new(1, 1.0, 0.5),
            AnimationLayer::new(2, 1.0, 0.5),
        ],
    );

    assert_eq!(walk.len(), arms.len());
    assert_eq!(walk.len(), both.len());

    for bone in 0..art.bone_count() {
        let a = walk.local(bone).unwrap();
        let b = arms.local(bone).unwrap();
        let ab = both.local(bone).unwrap();

        // Each layer contributes `weight * (value - rest)`; combining two
        // layers is the same as adding both contributions over rest.
        let expected_rotation = a.rotation + b.rotation;
        let expected_translation = [
            a.translation[0] + b.translation[0],
            a.translation[1] + b.translation[1],
        ];
        let expected_scale = a.scale + b.scale - 1.0;

        assert!((ab.rotation - expected_rotation).abs() < 1e-5);
        assert!((ab.translation[0] - expected_translation[0]).abs() < 1e-4);
        assert!((ab.translation[1] - expected_translation[1]).abs() < 1e-4);
        assert!((ab.scale - expected_scale).abs() < 1e-5);
    }
}
