//! Converts [`Asset`]s into a Vello [`Scene`].
//!
//! This module is enabled by the crate's `vello` feature. It is the rendering
//! half of the pipeline: callers decide which [`Pose`](crate::Pose) to apply
//! (for example, with [`Asset::deformed`](crate::Asset::deformed) or the
//! animation sampler) and then paint the result with [`SceneBuilder`] or
//! [`Asset::into_scene`].

use std::cmp::Ordering;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::{Asset, Path, Rgba8, Shape, Style};

impl Asset {
    /// Builds a Vello scene containing this asset.
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
    pub fn into_scene(&self, stroke_width: f32) -> Scene {
        let mut builder = SceneBuilder::new();
        builder.add_asset(self, stroke_width);
        let mut scene = Scene::new();
        builder.finish(&mut scene);
        scene
    }
}

/// Collects top-level shapes from one or more assets and paints them into a
/// [`Scene`], sorted by Z across all of them.
///
/// Shapes are painted in ascending Z; equal Z keeps the order they were added.
/// Children stay grouped with their parent and are not re-sorted against the
/// top-level shapes.
#[derive(Default)]
pub struct SceneBuilder<'a>(Vec<(&'a Shape, f32)>);

impl<'a> SceneBuilder<'a> {
    /// Starts an empty builder.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds every top-level shape of `asset`.
    pub fn add_asset(&mut self, asset: &'a Asset, stroke_width: f32) -> &mut Self {
        self.0
            .extend(asset.shapes.iter().map(|shape| (shape, stroke_width)));
        self
    }

    /// Adds a single top-level shape.
    pub fn add_shape(&mut self, shape: &'a Shape, stroke_width: f32) -> &mut Self {
        self.0.push((shape, stroke_width));
        self
    }

    /// Sorts the collected shapes by Z and paints them into `scene`, appending
    /// to whatever the scene already contains.
    pub fn finish(mut self, scene: &mut Scene) {
        self.sort();
        for (shape, stroke_width) in &self.0 {
            shape.draw(scene, *stroke_width);
        }
    }

    /// Stable sort by ascending Z, matching
    /// [`indices_in_paint_order`](crate::indices_in_paint_order).
    fn sort(&mut self) {
        self.0
            .sort_by(|(a, _), (b, _)| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));
    }
}

impl Shape {
    /// Paints this shape and its descendants into `scene`.
    fn draw(&self, scene: &mut Scene, stroke_width: f32) {
        let path = self.to_bez_path();
        draw_fill(scene, &self.style, &path);

        if !self.children.is_empty() {
            // Without paths there is nothing to clip against, so the children
            // are drawn unclipped rather than being made invisible.
            let clipped = !self.paths.is_empty();
            if clipped {
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &path);
            }
            for index in self.child_indices_in_paint_order() {
                self.children[index].draw(scene, stroke_width);
            }
            if clipped {
                scene.pop_layer();
            }
        }

        // The stroke sits above the children and is not clipped by its own
        // outline, so the hairline stays visible along the whole contour.
        draw_stroke(scene, &self.style, &path, stroke_width);
    }

    /// Concatenates all of this shape's paths into one compound path.
    ///
    /// The closed/open state of each contour is preserved, so an open contour
    /// remains open for stroking. Filling and clipping implicitly close open
    /// contours, which is what produces holes for oppositely-wound inner
    /// contours.
    pub fn to_bez_path(&self) -> BezPath {
        let mut bez = BezPath::new();
        for path in &self.paths {
            path.append_to(&mut bez, false);
        }
        bez
    }

    /// Like [`Self::to_bez_path`], but explicitly closes any open contour.
    ///
    /// This matches the fill semantics used for hit testing, where `kurbo`'s
    /// winding rule does not close open subpaths on its own.
    pub fn to_fill_bez_path(&self) -> BezPath {
        let mut bez = BezPath::new();
        for path in &self.paths {
            path.append_to(&mut bez, true);
        }
        bez
    }
}

fn draw_fill(scene: &mut Scene, style: &Style, path: &BezPath) {
    if let Some(fill) = style.fill {
        scene.fill(Fill::NonZero, Affine::IDENTITY, to_color(fill), None, path);
    }
}

fn draw_stroke(scene: &mut Scene, style: &Style, path: &BezPath, stroke_width: f32) {
    if let Some(stroke) = style.stroke {
        scene.stroke(
            &KurboStroke::new(stroke_width as f64),
            Affine::IDENTITY,
            to_color(stroke.color),
            None,
            path,
        );
    }
}

impl Path {
    /// Converts this model path into a kurbo path, closing it only when it is a
    /// loop.
    ///
    /// Omitted control points are resolved to their endpoints. Segments with no
    /// controls are emitted as lines so strokes keep sharp joins.
    pub fn to_bez_path(&self) -> BezPath {
        let mut bez = BezPath::new();
        self.append_to(&mut bez, false);
        bez
    }

    /// Appends this path to `bez`, optionally forcing a closing edge.
    fn append_to(&self, bez: &mut BezPath, force_close: bool) {
        let Some(start_point) = self.start() else {
            return;
        };
        bez.move_to(to_kurbo_point(start_point));

        let mut previous = start_point;
        for segment in &self.segments {
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
        if self.is_closed() || force_close {
            bez.close_path();
        }
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

        let stroked = shape.to_bez_path();
        assert!(!matches!(
            stroked.elements().last(),
            Some(PathEl::ClosePath)
        ));

        let filled = shape.to_fill_bez_path();
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

        let bez = shape.to_bez_path();
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
        let fill = holed.to_fill_bez_path();
        assert!(!fill.contains(probe));
        assert!(shape.to_fill_bez_path().contains(probe));
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

        let _ = asset.into_scene(1.0);
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

        let mut builder = SceneBuilder::new();
        builder.add_asset(&first, 1.).add_asset(&second, 2.);
        builder.sort();

        let order: Vec<f32> = builder.0.iter().map(|(shape, _)| shape.z).collect();
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

        let mut builder = SceneBuilder::new();
        builder.add_asset(&asset, 1.).add_shape(&added, 1.);
        builder.sort();

        let order: Vec<&str> = builder
            .0
            .iter()
            .map(|(shape, _)| shape.name.as_str())
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
        let mut builder = SceneBuilder::new();
        builder.add_asset(&asset, 1.).add_asset(&asset, 1.);
        builder.finish(&mut scene);
    }
}
