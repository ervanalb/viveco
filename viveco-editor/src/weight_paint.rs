//! Weight painting: a brush that edits the bone weights of curve points.
//!
//! The tool is read-only with respect to geometry. It draws every curve anchor
//! and Bézier control point over the document, tinted by its weight for the
//! selected bone (white at zero, red at one), and dragging the cursor over a
//! point repaints its weights.
//!
//! A point's weights always total at most one. Painting a bone grows its share
//! and scales the other bones back to make room; with no bone selected the
//! brush works on the point's unused share `1 - sum`, so painting a high value
//! reduces every bone's influence.
//!
//! The brush always paints a weight of one; how much of it lands per stroke is
//! set by the falloff with distance from the cursor, scaled by an overall
//! strength. [`WeightPaintBrush`] holds the radius, hardness (the fraction of
//! the radius at full falloff), and strength, all adjustable from the bottom
//! panel.

use bevy::prelude::*;
use bevy_vello::prelude::vello;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use std::collections::HashMap;

use crate::render::to_kurbo_point;
use viveco::model::{Document, Path, PathEnds, Point, Shape, Weights};
use viveco::pose::{Pose, Similarity};

/// Smallest brush radius, in screen pixels.
pub const MIN_BRUSH_SIZE: f32 = 2.0;

/// Largest brush radius, in screen pixels.
pub const MAX_BRUSH_SIZE: f32 = 150.0;

/// The weight-paint brush settings.
///
/// `size` is a radius in screen pixels, so the brush keeps its apparent size
/// as the document is zoomed. `hardness` is the fraction of the radius that
/// paints at full falloff; the rest fades to nothing at the edge. `strength`
/// scales the whole brush, so a low value lays down a light wash per stroke.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct WeightPaintBrush {
    pub size: f32,
    pub hardness: f32,
    pub strength: f32,
}

impl Default for WeightPaintBrush {
    fn default() -> Self {
        Self {
            size: 36.0,
            hardness: 0.5,
            strength: 0.1,
        }
    }
}

/// Point overlay dimensions, in screen pixels.
const POINT_RADIUS: f32 = 3.5;
const POINT_OUTLINE_WIDTH: f32 = 1.0;
const BRUSH_OUTLINE_WIDTH: f32 = 1.5;
const BRUSH_INNER_WIDTH: f32 = 1.0;

/// Overlay colors.
const POINT_OUTLINE: Color = Color::from_rgba8(0x0b, 0x2a, 0x40, 0xff);
const BRUSH_OUTLINE: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff);
const BRUSH_INNER: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0x66);

/// Brush falloff `normalized` of the way from the cursor to the rim.
///
/// Inside the hard core, `hardness` of the radius, the falloff is full. From
/// there it fades smoothly to zero at the rim, so repeated passes build up an
/// even gradient.
fn falloff(normalized: f32, hardness: f32) -> f32 {
    if normalized >= 1.0 {
        return 0.0;
    }
    let inner = hardness.clamp(0.0, 1.0);
    if normalized <= inner {
        return 1.0;
    }
    let t = (normalized - inner) / (1.0 - inner);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// Repaints every point of every shape under the brush.
///
/// `radius` is in document units. `selected_bone` is the bone whose weight the
/// brush paints; when it is `None`, the brush paints the point's unused share
/// instead, which reduces the weights that are already there.
pub fn paint_document(
    document: &mut Document,
    center: Point,
    radius: f32,
    selected_bone: Option<&str>,
    brush: &WeightPaintBrush,
    pose: &Pose,
) {
    // Points are painted where they appear on the posed rig page, so the brush
    // hit-tests posed positions while still editing the rest-pose weights.
    let worlds = (!pose.is_identity()).then(|| viveco::pose::worlds_by_name(document, pose));
    let worlds = worlds.as_ref();
    for shape in &mut document.shapes {
        paint_shape(shape, center, radius, selected_bone, brush, worlds);
    }
}

fn paint_shape(
    shape: &mut Shape,
    center: Point,
    radius: f32,
    selected_bone: Option<&str>,
    brush: &WeightPaintBrush,
    worlds: Option<&HashMap<String, Similarity>>,
) {
    for path in &mut shape.paths {
        paint_path(path, center, radius, selected_bone, brush, worlds);
    }
    for child in &mut shape.children {
        paint_shape(child, center, radius, selected_bone, brush, worlds);
    }
}

fn paint_path(
    path: &mut Path,
    center: Point,
    radius: f32,
    selected_bone: Option<&str>,
    brush: &WeightPaintBrush,
    worlds: Option<&HashMap<String, Similarity>>,
) {
    // A closed loop has no separate start anchor: its first anchor is the last
    // segment's end point, whose weights that segment carries.
    if let PathEnds::Open(start) = path.ends {
        paint_weights(
            &mut path.start_weights,
            start,
            center,
            radius,
            selected_bone,
            brush,
            worlds,
        );
    }
    for segment in &mut path.segments {
        if let Some(ctrl1) = segment.ctrl1 {
            paint_weights(
                &mut segment.ctrl1_weights,
                ctrl1,
                center,
                radius,
                selected_bone,
                brush,
                worlds,
            );
        }
        if let Some(ctrl2) = segment.ctrl2 {
            paint_weights(
                &mut segment.ctrl2_weights,
                ctrl2,
                center,
                radius,
                selected_bone,
                brush,
                worlds,
            );
        }
        paint_weights(
            &mut segment.to_weights,
            segment.to,
            center,
            radius,
            selected_bone,
            brush,
            worlds,
        );
    }
}

fn paint_weights(
    weights: &mut Weights,
    position: Point,
    center: Point,
    radius: f32,
    selected_bone: Option<&str>,
    brush: &WeightPaintBrush,
    worlds: Option<&HashMap<String, Similarity>>,
) {
    if radius <= f32::EPSILON {
        return;
    }
    // The brush is measured against where the point appears on the posed rig
    // page, not its rest position.
    let posed = worlds
        .map(|worlds| viveco::pose::deform_point(position, weights, worlds))
        .unwrap_or(position);
    let distance = ((posed[0] - center[0]).powi(2) + (posed[1] - center[1]).powi(2)).sqrt();
    if distance > radius {
        return;
    }
    // Strength scales the falloff, so each stroke lays down only part of the
    // painted weight and repeated passes build it up.
    let alpha = falloff(distance / radius, brush.hardness) * brush.strength.clamp(0.0, 1.0);
    if alpha <= f32::EPSILON {
        return;
    }
    // The brush always paints a full weight; without a selected bone the same
    // value is applied to the point's unused share, which reduces the weights
    // already there instead.
    match selected_bone {
        Some(bone) => weights.paint_bone(bone, 1.0, alpha),
        None => weights.paint_total(1.0, alpha),
    }
}

/// The color shown for a point at `value`: white at zero, red at one.
pub fn weight_color(value: f32) -> Color {
    let t = value.clamp(0.0, 1.0);
    Color::new([1.0, 1.0 - t, 1.0 - t, 1.0])
}

/// The value a point displays, which is its weight on the selected bone, or
/// its unused share when no bone is selected.
fn point_value(weights: &Weights, selected_bone: Option<&str>) -> f32 {
    match selected_bone {
        Some(bone) => weights.get(bone).unwrap_or(0.0),
        None => (1.0 - weights.sum()).clamp(0.0, 1.0),
    }
}

/// Draws every curve point tinted by its weight, followed by the brush ring at
/// `cursor`.
pub fn draw_overlay(
    scene: &mut Scene,
    document: &Document,
    selected_bone: Option<&str>,
    brush: &WeightPaintBrush,
    view_scale: f32,
    cursor: Option<Point>,
) {
    let scale = view_scale.max(f32::EPSILON);
    for shape in &document.shapes {
        draw_shape(scene, shape, selected_bone, scale);
    }
    if let Some(cursor) = cursor {
        draw_brush(scene, cursor, brush, scale);
    }
}

fn draw_shape(scene: &mut Scene, shape: &Shape, selected_bone: Option<&str>, scale: f32) {
    for path in &shape.paths {
        draw_path(scene, path, selected_bone, scale);
    }
    for child in &shape.children {
        draw_shape(scene, child, selected_bone, scale);
    }
}

fn draw_path(scene: &mut Scene, path: &Path, selected_bone: Option<&str>, scale: f32) {
    // The curve outline and its bezier arms stay visible in this mode, so the
    // point colors can be read against the geometry they belong to.
    crate::editor::draw_path_guides(scene, path, scale);

    if let PathEnds::Open(start) = path.ends {
        draw_point(scene, start, &path.start_weights, selected_bone, scale);
    }
    for segment in &path.segments {
        if let Some(ctrl1) = segment.ctrl1 {
            draw_point(scene, ctrl1, &segment.ctrl1_weights, selected_bone, scale);
        }
        if let Some(ctrl2) = segment.ctrl2 {
            draw_point(scene, ctrl2, &segment.ctrl2_weights, selected_bone, scale);
        }
        draw_point(scene, segment.to, &segment.to_weights, selected_bone, scale);
    }
}

fn draw_point(
    scene: &mut Scene,
    position: Point,
    weights: &Weights,
    selected_bone: Option<&str>,
    scale: f32,
) {
    let color = weight_color(point_value(weights, selected_bone));
    let circle = Circle::new(to_kurbo_point(position), f64::from(POINT_RADIUS / scale));
    scene.fill(Fill::NonZero, Affine::IDENTITY, &color, None, &circle);
    scene.stroke(
        &KurboStroke::new(f64::from(POINT_OUTLINE_WIDTH / scale)),
        Affine::IDENTITY,
        &POINT_OUTLINE,
        None,
        &circle,
    );
}

fn draw_brush(scene: &mut Scene, center: Point, brush: &WeightPaintBrush, scale: f32) {
    let radius = brush.size / scale;
    if radius <= 0.0 {
        return;
    }
    let outer = Circle::new(to_kurbo_point(center), f64::from(radius));
    scene.stroke(
        &KurboStroke::new(f64::from(BRUSH_OUTLINE_WIDTH / scale)),
        Affine::IDENTITY,
        &BRUSH_OUTLINE,
        None,
        &outer,
    );

    // The hard core shows where the brush paints at full strength.
    let inner_radius = radius * brush.hardness.clamp(0.0, 1.0);
    if inner_radius > 0.0 {
        let inner = Circle::new(to_kurbo_point(center), f64::from(inner_radius));
        scene.stroke(
            &KurboStroke::new(f64::from(BRUSH_INNER_WIDTH / scale)),
            Affine::IDENTITY,
            &BRUSH_INNER,
            None,
            &inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viveco::model::{Bone, FORMAT_VERSION, Path, Shape};

    fn brush() -> WeightPaintBrush {
        WeightPaintBrush {
            size: 10.0,
            hardness: 1.0,
            strength: 1.0,
        }
    }

    fn document_with(points: &[Point]) -> Document {
        let mut path = Path::new(points[0]);
        for &point in &points[1..] {
            path = path.line_to(point);
        }
        Document {
            version: FORMAT_VERSION,
            animations: Vec::new(),
            bones: Vec::new(),
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
        }
    }

    fn weights_at(document: &Document, index: usize) -> &Weights {
        &document.shapes[0].paths[0].segments[index].to_weights
    }

    #[test]
    fn falloff_is_full_inside_the_hard_core() {
        assert_eq!(falloff(0.0, 0.5), 1.0);
        assert_eq!(falloff(0.5, 0.5), 1.0);
    }

    #[test]
    fn falloff_fades_to_zero_at_the_rim() {
        assert_eq!(falloff(1.0, 0.5), 0.0);
        assert!(falloff(0.75, 0.5) > 0.0 && falloff(0.75, 0.5) < 1.0);
        // A fully hard brush still reaches zero exactly at the rim.
        assert_eq!(falloff(1.0, 1.0), 0.0);
        assert_eq!(falloff(0.999, 1.0), 1.0);
    }

    #[test]
    fn painting_only_affects_points_under_the_brush() {
        let mut document = document_with(&[[0.0, 0.0], [100.0, 0.0], [5.0, 0.0]]);
        paint_document(
            &mut document,
            [0.0, 0.0],
            10.0,
            Some("Arm"),
            &brush(),
            &Pose::default(),
        );

        // The start point and the nearby end point were painted; the far one
        // was not.
        let path = &document.shapes[0].paths[0];
        assert_eq!(path.start_weights.get("Arm"), Some(1.0));
        assert_eq!(weights_at(&document, 0).get("Arm"), None);
        assert_eq!(weights_at(&document, 1).get("Arm"), Some(1.0));
    }

    #[test]
    fn painting_a_bone_shares_the_unit_budget() {
        let mut document = document_with(&[[0.0, 0.0], [10.0, 0.0]]);
        document.shapes[0].paths[0].segments[0]
            .to_weights
            .set("Leg", 1.0);

        paint_document(
            &mut document,
            [10.0, 0.0],
            10.0,
            Some("Arm"),
            &brush(),
            &Pose::default(),
        );

        let weights = weights_at(&document, 0);
        assert_eq!(weights.get("Arm"), Some(1.0));
        // Painting the whole budget onto Arm leaves nothing for Leg.
        assert_eq!(weights.get("Leg"), None);
    }

    #[test]
    fn strength_scales_how_much_weight_lands_in_one_stroke() {
        let mut document = document_with(&[[0.0, 0.0]]);
        let brush = WeightPaintBrush {
            size: 10.0,
            hardness: 1.0,
            strength: 0.1,
        };

        paint_document(
            &mut document,
            [0.0, 0.0],
            10.0,
            Some("Arm"),
            &brush,
            &Pose::default(),
        );

        let weights = &document.shapes[0].paths[0].start_weights;
        assert!((weights.get("Arm").unwrap() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn painting_without_a_bone_reduces_the_weights() {
        let mut document = document_with(&[[0.0, 0.0], [10.0, 0.0]]);
        document.shapes[0].paths[0].segments[0]
            .to_weights
            .set("Leg", 0.5);

        paint_document(
            &mut document,
            [10.0, 0.0],
            10.0,
            None,
            &brush(),
            &Pose::default(),
        );

        // With no bone selected, a brush value of one clears the influence.
        assert!(weights_at(&document, 0).is_empty());
    }

    #[test]
    fn painting_hit_tests_the_posed_position() {
        let mut document = document_with(&[[10.0, 0.0]]);
        document.bones.push(Bone {
            name: "Arm".into(),
            head: Some([0.0, 0.0]),
            tail: [10.0, 0.0],
            children: Vec::new(),
        });
        document.shapes[0].paths[0].start_weights.set("Arm", 1.0);
        let mut pose = Pose::default();
        pose.set(
            "Arm",
            viveco::pose::BonePose {
                scale: 2.0,
                ..Default::default()
            },
        );

        // The point is drawn at [20, 0] under the scale, so a brush there
        // paints it even though its rest position is [10, 0].
        paint_document(
            &mut document,
            [20.0, 0.0],
            2.0,
            Some("Arm"),
            &brush(),
            &pose,
        );
        assert_eq!(
            document.shapes[0].paths[0].start_weights.get("Arm"),
            Some(1.0)
        );

        // A brush at the rest position misses it.
        document.shapes[0].paths[0].start_weights.set("Arm", 0.5);
        paint_document(
            &mut document,
            [10.0, 0.0],
            2.0,
            Some("Arm"),
            &brush(),
            &pose,
        );
        assert_eq!(
            document.shapes[0].paths[0].start_weights.get("Arm"),
            Some(0.5)
        );
    }

    #[test]
    fn weight_color_runs_from_white_to_red() {
        assert_eq!(weight_color(0.0), Color::new([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(weight_color(1.0), Color::new([1.0, 0.0, 0.0, 1.0]));
    }
}
