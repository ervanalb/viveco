//! Skinning and Vello scene construction.
//!
//! Shapes are painted in the order baked into the binary: sibling shapes are
//! sorted by Z at compile time, so a scene build only has to walk the stored
//! root range. Every path point is deformed by linear-blend skinning before it
//! reaches the Bézier path, so animation and skinning are independent of the
//! paint structure.

use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Stroke},
    peniko::{Color, Fill},
};

use crate::format::{PathRef, ShapeRef, Viveco, WeightRef};
use crate::pose::{Pose, Similarity, bone_worlds};

/// Builds a Vello [`Scene`] for `art` in the pose described by `pose`.
pub fn build_scene(art: &Viveco<'_>, pose: &Pose, scene: &mut Scene) {
    let worlds = bone_worlds(art, pose);
    for index in art.root_shape_indices() {
        if let Some(shape) = art.shape(index) {
            draw_shape(art, &worlds, shape, scene);
        }
    }
}

fn draw_shape(art: &Viveco<'_>, worlds: &[Similarity], shape: ShapeRef<'_>, scene: &mut Scene) {
    let bez = deformed_shape_path(art, worlds, shape);

    if let Some(fill) = shape.fill() {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(fill.r, fill.g, fill.b, fill.a),
            None,
            &bez,
        );
    }

    if shape.has_children() {
        // Without paths there is nothing to clip against, so children are
        // drawn unclipped rather than being made invisible.
        let clipped = shape.has_paths();
        if clipped {
            scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &bez);
        }
        for child in shape.children() {
            draw_shape(art, worlds, child, scene);
        }
        if clipped {
            scene.pop_layer();
        }
    }

    if let Some((color, width)) = shape.stroke() {
        scene.stroke(
            &Stroke::new(width as f64),
            Affine::IDENTITY,
            Color::from_rgba8(color.r, color.g, color.b, color.a),
            None,
            &bez,
        );
    }
}

/// Concatenates a shape's paths into one compound path, deforming every point.
fn deformed_shape_path(art: &Viveco<'_>, worlds: &[Similarity], shape: ShapeRef<'_>) -> BezPath {
    let mut bez = BezPath::new();
    for path in shape.paths() {
        append_deformed_path(art, worlds, path, &mut bez);
    }
    bez
}

fn append_deformed_path(
    _art: &Viveco<'_>,
    worlds: &[Similarity],
    path: PathRef<'_>,
    bez: &mut BezPath,
) {
    // A closed loop's first anchor is its last segment's end point, whose
    // weights live on that segment.
    let (start, start_weights) = if path.is_closed() {
        let Some(last) = path.last_segment() else {
            return;
        };
        (last.to(), last.to_weights())
    } else {
        let Some(start) = path.start_point() else {
            return;
        };
        (start, path.start_weights())
    };

    let start = deform_point(worlds, start, start_weights);
    bez.move_to(to_point(start));

    let mut previous = start;
    for segment in path.segments() {
        let to = deform_point(worlds, segment.to(), segment.to_weights());
        let ctrl1 = segment
            .ctrl1()
            .map(|ctrl| deform_point(worlds, ctrl, segment.ctrl1_weights()))
            .unwrap_or(previous);
        let ctrl2 = segment
            .ctrl2()
            .map(|ctrl| deform_point(worlds, ctrl, segment.ctrl2_weights()))
            .unwrap_or(to);

        if segment.is_line() {
            bez.line_to(to_point(to));
        } else {
            bez.curve_to(to_point(ctrl1), to_point(ctrl2), to_point(to));
        }
        previous = to;
    }

    if path.is_closed() {
        bez.close_path();
    }
}

/// Skins one point: a weighted blend of each bone's world transform, with the
/// unused share `1 - sum` left in place.
fn deform_point(
    worlds: &[Similarity],
    position: [f32; 2],
    weights: impl Iterator<Item = WeightRef>,
) -> [f32; 2] {
    let mut blended = [0.0, 0.0];
    let mut matched = 0.0;
    for weight in weights {
        let Some(world) = worlds.get(weight.bone) else {
            continue;
        };
        let posed = world.apply(position);
        blended[0] += weight.weight * posed[0];
        blended[1] += weight.weight * posed[1];
        matched += weight.weight;
    }
    let rest = (1.0 - matched).max(0.0);
    [
        blended[0] + rest * position[0],
        blended[1] + rest * position[1],
    ]
}

fn to_point(point: [f32; 2]) -> Point {
    Point::new(point[0] as f64, point[1] as f64)
}
