//! Converts [`Asset`]s into a Vello [`Scene`].
//!
//! This module is enabled by the crate's `vello` feature. It is the rendering
//! half of the pipeline: callers decide which [`Pose`](crate::Pose) to apply
//! (for example, with [`Asset::deformed`](crate::Asset::deformed) or the
//! animation sampler) and then paint the result with [`SceneBuilder`] or
//! [`Asset::into_scene`].
//!
//! [`SceneBuilder`] combines several assets into one scene and sorts their
//! top-level shapes by Z together. Each asset may be added with its own
//! [`Affine`] placement, which is baked into the emitted geometry.
//! [`SceneInstance`] bundles a deformed document with that placement and its
//! stroke width.

use std::cmp::Ordering;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::{AnimationInstance, Asset, Path, Rgba8, Shape, Style};

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

/// An instance of a document, sampled and placed for rendering into a scene.
///
/// It owns its deformed geometry, so the animation instances, the scene-space
/// placement, and the stroke width travel together as one value.
/// Build one from animation instances with [`SceneInstance::animated`], then
/// adjust it with the `with_*` methods. Add it to a [`SceneBuilder`] with
/// [`SceneBuilder::add_instance`].
pub struct SceneInstance {
    /// The deformed document to paint.
    pub asset: Asset,
    /// Where the document sits in scene space.
    pub transform: Affine,
    /// Stroke width in the document's local units.
    pub stroke_width: f32,
}

impl SceneInstance {
    /// Samples `asset` at `animations` and deforms it into a new instance.
    ///
    /// The instance starts at the identity placement with one-unit strokes.
    pub fn animated(asset: &Asset, animations: &[AnimationInstance]) -> Self {
        let pose = asset.sample_pose(animations);
        Self {
            asset: asset.deformed(&pose),
            transform: Affine::IDENTITY,
            stroke_width: 1.0,
        }
    }

    /// Sets the scene-space placement.
    pub fn with_transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    /// Sets the stroke width in the deformed document's local units.
    pub fn with_stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }
}

/// One stand-alone piece of a scene, with the Z at which to draw it.
///
/// A fragment owns a complete Vello scene in scene space (including any
/// placement transform), so fragments can be sorted, stored, and appended in
/// any order. [`SceneBuilder::into_fragments`] produces one fragment per
/// top-level shape, which is how the Bevy integration keeps instance artwork
/// Z-sorted against custom fragments.
pub struct SceneFragment {
    /// Sort key: fragments are drawn in ascending Z.
    pub z: f32,
    /// The fragment's complete Vello content.
    pub scene: Scene,
}

/// One shape queued in a [`SceneBuilder`], with the placement that puts it in
/// scene space.
struct Placement<'a> {
    shape: &'a Shape,
    stroke_width: f32,
    transform: Affine,
}

/// Collects top-level shapes from one or more assets and paints them into a
/// [`Scene`], sorted by Z across all of them.
///
/// Shapes are painted in ascending Z; equal Z keeps the order they were added.
/// Children stay grouped with their parent and are not re-sorted against the
/// top-level shapes.
///
/// A placement transform affects only the shapes added with it. This is how
/// callers bake per-asset entity transforms into one shared scene.
#[derive(Default)]
pub struct SceneBuilder<'a>(Vec<Placement<'a>>);

impl<'a> SceneBuilder<'a> {
    /// Starts an empty builder.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds every top-level shape of `instance`.
    pub fn add_instance(&mut self, instance: &'a SceneInstance) -> &mut Self {
        self.add_asset_with_transform(&instance.asset, instance.stroke_width, instance.transform)
    }

    /// Adds every top-level shape of `asset` with an identity placement.
    pub fn add_asset(&mut self, asset: &'a Asset, stroke_width: f32) -> &mut Self {
        self.add_asset_with_transform(asset, stroke_width, Affine::IDENTITY)
    }

    /// Adds every top-level shape of `asset`, placed by `transform`.
    ///
    /// `stroke_width` is in the asset's local units; `transform` is applied to
    /// the geometry afterwards.
    pub fn add_asset_with_transform(
        &mut self,
        asset: &'a Asset,
        stroke_width: f32,
        transform: Affine,
    ) -> &mut Self {
        self.0.extend(asset.shapes.iter().map(|shape| Placement {
            shape,
            stroke_width,
            transform,
        }));
        self
    }

    /// Adds a single top-level shape with an identity placement.
    pub fn add_shape(&mut self, shape: &'a Shape, stroke_width: f32) -> &mut Self {
        self.add_shape_with_transform(shape, stroke_width, Affine::IDENTITY)
    }

    /// Adds a single top-level shape, placed by `transform`.
    pub fn add_shape_with_transform(
        &mut self,
        shape: &'a Shape,
        stroke_width: f32,
        transform: Affine,
    ) -> &mut Self {
        self.0.push(Placement {
            shape,
            stroke_width,
            transform,
        });
        self
    }

    /// Sorts the collected shapes by Z and paints them into `scene`, appending
    /// to whatever the scene already contains.
    pub fn finish(mut self, scene: &mut Scene) {
        self.sort();
        for placement in &self.0 {
            placement
                .shape
                .draw(scene, placement.stroke_width, placement.transform);
        }
    }

    /// Sorts the collected shapes by Z and paints each one into its own
    /// [`SceneFragment`], with the placement baked in.
    ///
    /// One fragment per top-level shape. Children stay grouped with their
    /// parent. Equal Z keeps the order the shapes were added.
    pub fn into_fragments(mut self) -> Vec<SceneFragment> {
        self.sort();
        self.0
            .into_iter()
            .map(|placement| {
                let mut scene = Scene::new();
                placement
                    .shape
                    .draw(&mut scene, placement.stroke_width, placement.transform);
                SceneFragment {
                    z: placement.shape.z,
                    scene,
                }
            })
            .collect()
    }

    /// Stable sort by ascending Z, matching
    /// [`indices_in_paint_order`](crate::indices_in_paint_order).
    fn sort(&mut self) {
        self.0
            .sort_by(|a, b| a.shape.z.partial_cmp(&b.shape.z).unwrap_or(Ordering::Equal));
    }
}

impl Shape {
    /// Paints this shape and its descendants into a standalone fragment.
    ///
    /// `stroke_width` is in this shape's local units; `transform` places it in
    /// scene space. The fragment's Z is this shape's [`z`](Self::z).
    pub fn to_fragment(&self, stroke_width: f32, transform: Affine) -> SceneFragment {
        let mut scene = Scene::new();
        self.draw(&mut scene, stroke_width, transform);
        SceneFragment { z: self.z, scene }
    }

    /// Paints this shape and its descendants into `scene`, through `transform`.
    fn draw(&self, scene: &mut Scene, stroke_width: f32, transform: Affine) {
        let path = self.to_bez_path();
        draw_fill(scene, &self.style, &path, transform);

        if !self.children.is_empty() {
            // Without paths there is nothing to clip against, so the children
            // are drawn unclipped rather than being made invisible.
            let clipped = !self.paths.is_empty();
            if clipped {
                scene.push_clip_layer(Fill::NonZero, transform, &path);
            }
            for index in self.child_indices_in_paint_order() {
                self.children[index].draw(scene, stroke_width, transform);
            }
            if clipped {
                scene.pop_layer();
            }
        }

        // The stroke sits above the children and is not clipped by its own
        // outline, so the hairline stays visible along the whole contour.
        draw_stroke(scene, &self.style, &path, stroke_width, transform);
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

fn draw_fill(scene: &mut Scene, style: &Style, path: &BezPath, transform: Affine) {
    if let Some(fill) = style.fill {
        scene.fill(Fill::NonZero, transform, to_color(fill), None, path);
    }
}

fn draw_stroke(
    scene: &mut Scene,
    style: &Style,
    path: &BezPath,
    stroke_width: f32,
    transform: Affine,
) {
    if let Some(stroke) = style.stroke {
        scene.stroke(
            &KurboStroke::new(stroke_width as f64),
            transform,
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

        let order: Vec<f32> = builder
            .0
            .iter()
            .map(|placement| placement.shape.z)
            .collect();
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
            .map(|placement| placement.shape.name.as_str())
            .collect();
        assert_eq!(order, vec!["low", "added", "high"]);
    }

    #[test]
    fn builder_keeps_each_assets_placement() {
        let shape = |z: f32| Shape {
            z,
            ..Default::default()
        };
        let first = Asset {
            shapes: vec![shape(2.0), shape(1.0)],
            ..Default::default()
        };
        let transform = Affine::translate((10.0, -20.0));

        let mut builder = SceneBuilder::new();
        builder.add_asset(&first, 1.0);
        builder.add_asset_with_transform(&first, 2.0, transform);

        assert_eq!(builder.0.len(), 4);
        assert_eq!(builder.0[0].transform, Affine::IDENTITY);
        assert_eq!(builder.0[2].transform, transform);
        assert_eq!(builder.0[2].stroke_width, 2.0);
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

    #[test]
    fn builder_splits_one_fragment_per_shape_sorted_by_z() {
        let shape = |z: f32, paths: usize| Shape {
            z,
            paths: (0..paths)
                .map(|i| Path::new([i as f32, 0.0]).line_to([i as f32, 1.0]))
                .collect(),
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        };
        let asset = Asset {
            shapes: vec![shape(5.0, 1), shape(1.0, 2)],
            ..Default::default()
        };
        let transform = Affine::translate((10.0, -20.0));

        let mut builder = SceneBuilder::new();
        builder.add_asset_with_transform(&asset, 1.0, transform);
        let fragments = builder.into_fragments();

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].z, 1.0);
        assert_eq!(fragments[0].scene.encoding().n_paths, 1);
        assert_eq!(fragments[1].z, 5.0);
        assert_eq!(fragments[1].scene.encoding().n_paths, 1);
        // The placement is baked into the fragment's encoded geometry.
        assert!(
            fragments[0]
                .scene
                .encoding()
                .transforms
                .iter()
                .any(|t| t.translation == [10.0, -20.0])
        );
    }

    #[test]
    fn shape_to_fragment_uses_its_own_z() {
        let shape = Shape {
            z: 3.5,
            paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        };

        let fragment = shape.to_fragment(1.0, Affine::IDENTITY);
        assert_eq!(fragment.z, 3.5);
        assert_eq!(fragment.scene.encoding().n_paths, 1);
    }
}
