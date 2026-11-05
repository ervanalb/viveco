//! Converts [`Asset`]s into a Vello [`Scene`].
//!
//! This module is enabled by the crate's `vello` feature. It is the rendering
//! half of the pipeline: callers decide which [`Pose`](crate::Pose) to apply
//! (for example, with [`Asset::deformed`](crate::Asset::deformed) or the
//! animation sampler) and then paint the result with [`SceneBuilder`] or the
//! single-asset [`build_scene`] helper.

use std::cmp::Ordering;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::{Asset, Path, Rgba8, Shape, Style};

/// Builds a Vello scene containing a single asset.
///
/// `stroke_width` is in document units; pass `1.0 / scale` to paint every
/// stroke as one screen pixel wide regardless of zoom.
///
/// Sibling shapes are painted in ascending Z order. A shape's paths are
/// concatenated into one compound path, which is filled with the non-zero
/// winding rule: contours wound opposite to the outer contour become holes.
/// The fill is painted first, then the children inside a clip layer keyed to
/// the same compound path, then the stroke on top. A shape with children but
/// no paths acts as an unclipped group.
///
/// Use [`SceneBuilder`] to combine several assets in one scene and sort their
/// top-level shapes against each other.
pub fn build_scene(document: &Asset, stroke_width: f64) -> Scene {
    let mut builder = SceneBuilder::new(stroke_width);
    builder.add_asset(document);
    let mut scene = Scene::new();
    builder.finish(&mut scene);
    scene
}

/// Collects top-level shapes from one or more assets and paints them into a
/// [`Scene`], sorted by Z across all of them.
///
/// Shapes are painted in ascending Z; equal Z keeps the order they were added.
/// Children stay grouped with their parent and are not re-sorted against the
/// top-level shapes.
pub struct SceneBuilder<'a> {
    stroke_width: f64,
    shapes: Vec<&'a Shape>,
}

impl<'a> SceneBuilder<'a> {
    /// Starts a builder that paints every stroke `stroke_width` document units
    /// wide.
    pub fn new(stroke_width: f64) -> Self {
        Self {
            stroke_width,
            shapes: Vec::new(),
        }
    }

    /// Adds every top-level shape of `asset`.
    pub fn add_asset(&mut self, asset: &'a Asset) -> &mut Self {
        self.shapes.extend(asset.shapes.iter());
        self
    }

    /// Adds a single top-level shape.
    pub fn add_shape(&mut self, shape: &'a Shape) -> &mut Self {
        self.shapes.push(shape);
        self
    }

    /// Sorts the collected shapes by Z and paints them into `scene`, appending
    /// to whatever the scene already contains.
    pub fn finish(mut self, scene: &mut Scene) {
        self.sort();
        let stroke_width = self.stroke_width;
        for &shape in &self.shapes {
            draw_shape(scene, shape, stroke_width);
        }
    }

    /// Stable sort by ascending Z, matching
    /// [`indices_in_paint_order`](crate::indices_in_paint_order).
    fn sort(&mut self) {
        self.shapes
            .sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));
    }
}

fn draw_shape(scene: &mut Scene, shape: &Shape, stroke_width: f64) {
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
            draw_shape(scene, &shape.children[index], stroke_width);
        }
        if clipped {
            scene.pop_layer();
        }
    }

    // The stroke sits above the children and is not clipped by its own
    // outline, so the hairline stays visible along the whole contour.
    draw_stroke(scene, &shape.style, &path, stroke_width);
}

fn draw_fill(scene: &mut Scene, style: &Style, path: &BezPath) {
    if let Some(fill) = style.fill {
        scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(fill), None, path);
    }
}

fn draw_stroke(scene: &mut Scene, style: &Style, path: &BezPath, stroke_width: f64) {
    if let Some(stroke) = style.stroke {
        scene.stroke(
            &KurboStroke::new(stroke_width),
            Affine::IDENTITY,
            to_color(stroke.color),
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
/// This matches the fill semantics used for hit testing, where `kurbo`'s
/// winding rule does not close open subpaths on its own.
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
pub fn to_kurbo_point(point: [f32; 2]) -> KurboPoint {
    KurboPoint::new(f64::from(point[0]), f64::from(point[1]))
}

/// Converts a model color into a peniko color.
pub fn to_color(color: Rgba8) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::{PathEl, Shape as _};

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

    #[test]
    fn scenes_paint_every_shape_without_panicking() {
        let asset = Asset {
            shapes: vec![Shape {
                paths: vec![
                    Path::new([0.0, 0.0])
                        .line_to([10.0, 0.0])
                        .line_to([10.0, 10.0])
                        .closed(),
                ],
                style: Style {
                    fill: Some(Rgba8::rgb(255, 0, 0)),
                    stroke: Some(crate::StrokeStyle {
                        color: Rgba8::rgb(0, 0, 0),
                    }),
                },
                children: vec![Shape {
                    paths: vec![Path::new([2.0, 2.0]).line_to([8.0, 8.0])],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let _ = build_scene(&asset, 1.0);
    }

    #[test]
    fn builder_sorts_shapes_across_assets() {
        let shape = |z: f32| Shape {
            z,
            ..Default::default()
        };
        let first = Asset {
            shapes: vec![shape(5.0), shape(1.0)],
            ..Default::default()
        };
        let second = Asset {
            shapes: vec![shape(3.0), shape(2.0)],
            ..Default::default()
        };

        let mut builder = SceneBuilder::new(1.0);
        builder.add_asset(&first).add_asset(&second);
        builder.sort();

        let order: Vec<f32> = builder.shapes.iter().map(|shape| shape.z).collect();
        assert_eq!(order, vec![1.0, 2.0, 3.0, 5.0]);
    }

    #[test]
    fn builder_keeps_insertion_order_for_equal_z() {
        let named = |name: &str, z: f32| Shape {
            name: name.to_string(),
            z,
            ..Default::default()
        };
        let asset = Asset {
            shapes: vec![named("high", 2.0), named("low", 1.0)],
            ..Default::default()
        };
        let added = named("added", 1.0);

        let mut builder = SceneBuilder::new(1.0);
        builder.add_asset(&asset).add_shape(&added);
        builder.sort();

        let order: Vec<&str> = builder
            .shapes
            .iter()
            .map(|shape| shape.name.as_str())
            .collect();
        assert_eq!(order, vec!["low", "added", "high"]);
    }

    #[test]
    fn builder_paints_multiple_assets_into_one_scene() {
        let asset = Asset {
            shapes: vec![Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new(1.0);
        builder.add_asset(&asset).add_asset(&asset);
        builder.finish(&mut scene);
    }
}
