//! Converts a [`Document`] into a Vello [`Scene`].

use bevy_vello::vello::{
    Scene,
    kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke},
    peniko::{Color, Fill},
};

use viveco::model::{Document, Path, Rgba8, Shape, Style};

/// Builds a Vello scene in document coordinates.
///
/// Sibling shapes are painted in ascending Z order. A shape's paths are
/// concatenated into one compound path, which is filled with the non-zero
/// winding rule: contours wound opposite to the outer contour become holes.
/// The fill is painted first, then the children inside a clip layer keyed to
/// the same compound path, then the stroke on top. A shape with children but
/// no paths acts as an unclipped group.
pub fn build_scene(document: &Document) -> Scene {
    let mut scene = Scene::new();
    for index in document.shape_indices_in_paint_order() {
        draw_shape(&mut scene, &document.shapes[index]);
    }
    scene
}

fn draw_shape(scene: &mut Scene, shape: &Shape) {
    let path = shape_to_bez_path(shape);
    draw_fill(scene, &shape.style, &path);

    if !shape.children.is_empty() {
        // Without paths there is nothing to clip against, so the children are
        // drawn unclipped rather than being made invisible.
        let clipped = !shape.paths.is_empty();
        if clipped {
            scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &path);
        }
        for index in shape.child_indices_in_paint_order() {
            draw_shape(scene, &shape.children[index]);
        }
        if clipped {
            scene.pop_layer();
        }
    }

    // The stroke sits above the children and is not clipped by its own
    // outline, so the whole stroke width stays visible.
    draw_stroke(scene, &shape.style, &path);
}

fn draw_fill(scene: &mut Scene, style: &Style, path: &BezPath) {
    if let Some(fill) = style.fill {
        scene.fill(Fill::NonZero, Affine::IDENTITY, &to_color(fill), None, path);
    }
}

fn draw_stroke(scene: &mut Scene, style: &Style, path: &BezPath) {
    if let Some(stroke) = style.stroke {
        scene.stroke(
            &KurboStroke::new(f64::from(stroke.width)),
            Affine::IDENTITY,
            &to_color(stroke.color),
            None,
            path,
        );
    }
}

/// Converts a model path into a kurbo path, closing it only when it is a loop.
///
/// Omitted control points are resolved to their endpoints. Segments with no
/// controls are emitted as lines so strokes keep sharp joins.
pub fn path_to_bez_path(path: &Path) -> BezPath {
    let mut bez = BezPath::new();
    let Some(start) = path.start() else {
        return bez;
    };
    bez.move_to(to_kurbo_point(start));

    let mut previous = start;
    for segment in &path.segments {
        let ctrl1 = segment.ctrl1.unwrap_or(previous);
        let ctrl2 = segment.ctrl2.unwrap_or(segment.to);
        if segment.is_line() {
            bez.line_to(to_kurbo_point(segment.to));
        } else {
            bez.curve_to(
                to_kurbo_point(ctrl1),
                to_kurbo_point(ctrl2),
                to_kurbo_point(segment.to),
            );
        }
        previous = segment.to;
    }
    if path.is_closed() {
        bez.close_path();
    }
    bez
}

/// Concatenates all of a shape's paths into one compound path.
///
/// The closed/open state of each contour is preserved, so an open contour
/// remains open for stroking. Filling and clipping implicitly close open
/// contours, which is what produces holes for oppositely-wound inner contours.
pub fn shape_to_bez_path(shape: &Shape) -> BezPath {
    let mut bez = BezPath::new();
    for path in &shape.paths {
        append_path(&mut bez, path, false);
    }
    bez
}

/// Like [`shape_to_bez_path`], but explicitly closes any open contour.
///
/// This matches the fill semantics used for hit testing, where
/// `kurbo`'s winding rule does not close open subpaths on its own.
pub fn shape_fill_path(shape: &Shape) -> BezPath {
    let mut bez = BezPath::new();
    for path in &shape.paths {
        append_path(&mut bez, path, true);
    }
    bez
}

/// Appends one model path to `bez`, optionally forcing a closing edge.
fn append_path(bez: &mut BezPath, path: &Path, force_close: bool) {
    let start = path.start();
    let Some(start_point) = start else {
        return;
    };
    bez.move_to(to_kurbo_point(start_point));

    let mut previous = start_point;
    for segment in &path.segments {
        let ctrl1 = segment.ctrl1.unwrap_or(previous);
        let ctrl2 = segment.ctrl2.unwrap_or(segment.to);
        if segment.is_line() {
            bez.line_to(to_kurbo_point(segment.to));
        } else {
            bez.curve_to(
                to_kurbo_point(ctrl1),
                to_kurbo_point(ctrl2),
                to_kurbo_point(segment.to),
            );
        }
        previous = segment.to;
    }
    if path.is_closed() || force_close {
        bez.close_path();
    }
}

/// Converts a model point into a kurbo point.
pub(crate) fn to_kurbo_point(point: [f32; 2]) -> KurboPoint {
    KurboPoint::new(f64::from(point[0]), f64::from(point[1]))
}

/// Converts a model color into a peniko color.
pub(crate) fn to_color(color: Rgba8) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_vello::vello::kurbo::{PathEl, Shape as _};

    #[test]
    fn open_contours_only_close_for_fill_tests() {
        let shape = Shape {
            paths: vec![
                Path::new([0.0, 0.0])
                    .line_to([10.0, 0.0])
                    .line_to([10.0, 10.0]),
            ],
            ..Default::default()
        };

        let stroked = shape_to_bez_path(&shape);
        assert!(!matches!(
            stroked.elements().last(),
            Some(PathEl::ClosePath)
        ));

        let filled = shape_fill_path(&shape);
        assert!(matches!(filled.elements().last(), Some(PathEl::ClosePath)));
    }

    #[test]
    fn compound_paths_keep_each_contour() {
        let shape = Shape {
            paths: vec![
                Path::new([0.0, 0.0])
                    .line_to([10.0, 0.0])
                    .line_to([10.0, 10.0])
                    .closed(),
                Path::new([2.0, 2.0])
                    .line_to([2.0, 8.0])
                    .line_to([8.0, 8.0])
                    .closed(),
            ],
            ..Default::default()
        };

        let bez = shape_to_bez_path(&shape);
        let moves = bez
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count();
        assert_eq!(moves, 2);

        // An inner contour wound the opposite way is a hole.
        let inner = Path::new([2.0, 2.0])
            .line_to([2.0, 8.0])
            .line_to([8.0, 8.0])
            .line_to([8.0, 2.0])
            .closed();
        let holed = Shape {
            paths: vec![shape.paths[0].clone(), inner],
            ..Default::default()
        };
        let probe = KurboPoint::new(4.0, 4.0);
        let fill = shape_fill_path(&holed);
        assert!(!fill.contains(probe));
        assert!(shape_fill_path(&shape).contains(probe));
    }
}
