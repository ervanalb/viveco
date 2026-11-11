//! Converts [`Asset`]s into a Vello [`Scene`].
//!
//! This module is enabled by the crate's `vello` feature. It is the rendering
//! half of the pipeline: callers decide which [`Pose`](crate::Pose) to apply,
//! resolve it once with [`WorldPose`], and then paint the untouched asset
//! with [`SceneBuilder`] or [`Asset::into_scene`].
//!
//! Skinning happens while the geometry is converted to Vello paths, not by
//! cloning the asset and rewriting every point. A `SceneInstance` therefore
//! borrows its asset and only carries the small list of bone world
//! transforms.
//!
//! [`SceneBuilder`] combines several assets into one scene and sorts their
//! top-level shapes by Z together. Each asset may be added with its own
//! [`Affine`] placement, which is baked into the emitted geometry.
//! [`SceneInstance`] bundles a borrowed asset, its optional world pose, that
//! placement, and its stroke width.

use std::cmp::Ordering;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke};
use vello::peniko::{Color, Fill};

use crate::pose::{Pose, ShapePose, WorldPose};
use crate::{AnimationInstance, Asset, Path, PathEnds, Point, Rgba8, Shape, Style, Weights};

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
    /// top-level shapes against each other. To paint a posed asset, resolve its
    /// [`Pose`] with [`WorldPose`] and add it with
    /// [`SceneBuilder::add_asset_with_world_pose`].
    pub fn into_scene(&self, stroke_width: f32) -> Scene {
        let mut builder = SceneBuilder::new();
        builder.add_asset(self, stroke_width);
        let mut scene = Scene::new();
        builder.finish(&mut scene);
        scene
    }

    /// Builds a Vello scene containing this asset with animated shape offsets.
    ///
    /// `shape_pose` is indexed like the shape tree in depth-first pre-order,
    /// and its Z offsets are added to each shape's authored Z while sibling
    /// shapes are ordered. Every other part of the asset is painted exactly as
    /// [`Self::into_scene`] paints it.
    pub fn into_scene_with_shape_pose(&self, shape_pose: &ShapePose, stroke_width: f32) -> Scene {
        let mut builder = SceneBuilder::new();
        builder.add_asset_with_shape_pose(self, shape_pose, stroke_width);
        let mut scene = Scene::new();
        builder.finish(&mut scene);
        scene
    }
}

/// An instance of a document, posed and placed for rendering into a scene.
///
/// It borrows the asset and owns only the resolved bone transforms, so posing
/// an instance never clones its shapes or paths. Build one with
/// [`SceneInstance::new`], [`SceneInstance::posed`], or
/// [`SceneInstance::animated`], then adjust it with the `with_*` methods. Add
/// it to a [`SceneBuilder`] with [`SceneBuilder::add_instance`].
pub struct SceneInstance<'a> {
    /// The document to paint.
    pub asset: &'a Asset,
    /// The resolved bone transforms to skin the document with.
    ///
    /// `None` paints the asset's rest geometry without skinning.
    pub world_pose: Option<WorldPose>,
    /// Animated shape offsets, added to the document's authored values.
    ///
    /// `None` paints the shapes at their authored Z.
    pub shape_pose: Option<ShapePose>,
    /// Where the document sits in scene space.
    pub transform: Affine,
    /// Stroke width in the document's local units.
    pub stroke_width: f32,
}

impl<'a> SceneInstance<'a> {
    /// Renders `asset` at its rest pose.
    ///
    /// The instance starts at the identity placement with one-unit strokes.
    pub fn new(asset: &'a Asset) -> Self {
        Self {
            asset,
            world_pose: None,
            shape_pose: None,
            transform: Affine::IDENTITY,
            stroke_width: 1.0,
        }
    }

    /// Renders `asset` under `pose`.
    ///
    /// The pose is resolved once, here. An identity pose stores no skin at all,
    /// so the asset is painted directly from its rest geometry.
    pub fn posed(asset: &'a Asset, pose: &Pose) -> Self {
        Self {
            asset,
            world_pose: (!pose.is_identity()).then(|| pose.to_world(asset)),
            shape_pose: None,
            transform: Affine::IDENTITY,
            stroke_width: 1.0,
        }
    }

    /// Samples `asset` with `animations` and renders it under the blended pose
    /// and shape offsets.
    ///
    /// The instance starts at the identity placement with one-unit strokes.
    pub fn animated(asset: &'a Asset, animations: &[AnimationInstance]) -> Self {
        if animations.is_empty() {
            return Self::new(asset);
        }
        let shape_pose = asset.sample_shape_pose(animations);
        Self {
            shape_pose: (!shape_pose.is_identity()).then_some(shape_pose),
            ..Self::posed(asset, &asset.sample_pose(animations))
        }
    }

    /// Sets the resolved bone transforms directly.
    ///
    /// `None` returns the instance to the asset's rest geometry.
    pub fn with_world_pose(mut self, world_pose: Option<WorldPose>) -> Self {
        self.world_pose = world_pose;
        self
    }

    /// Sets the animated shape offsets.
    ///
    /// `None` returns the shapes to their authored values.
    pub fn with_shape_pose(mut self, shape_pose: Option<ShapePose>) -> Self {
        self.shape_pose = shape_pose;
        self
    }

    /// Sets the scene-space placement.
    pub fn with_transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    /// Sets the stroke width in the document's local units.
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
/// Z-sorted against custom fragments. When a shape has animated offsets, its
/// fragment's Z is the effective Z, not the authored one.
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
    world_pose: Option<&'a WorldPose>,
    shape_pose: Option<&'a ShapePose>,
    /// The shape's flat depth-first pre-order index, used to index `shape_pose`.
    flat_index: usize,
    /// The shape's effective Z, including its animated offset.
    z: f32,
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
    pub fn add_instance(&mut self, instance: &'a SceneInstance<'a>) -> &mut Self {
        self.add_asset_with_pose(
            instance.asset,
            instance.world_pose.as_ref(),
            instance.shape_pose.as_ref(),
            instance.stroke_width,
            instance.transform,
        )
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
        self.add_asset_with_pose(asset, None, None, stroke_width, transform)
    }

    /// Adds every top-level shape of `asset` with animated shape offsets and
    /// an identity placement.
    ///
    /// The offsets are added to the shapes' authored Z values, including the
    /// Z values that order nested children.
    pub fn add_asset_with_shape_pose(
        &mut self,
        asset: &'a Asset,
        shape_pose: &'a ShapePose,
        stroke_width: f32,
    ) -> &mut Self {
        self.add_asset_with_pose(
            asset,
            None,
            Some(shape_pose),
            stroke_width,
            Affine::IDENTITY,
        )
    }

    /// Adds every top-level shape of `asset`, skinned by `world_pose` and
    /// placed by `transform`.
    ///
    /// The pose is resolved by the caller (usually with
    /// [`Pose::to_world`](crate::Pose::to_world)) and shared by every
    /// shape added in this call.
    pub fn add_asset_with_world_pose(
        &mut self,
        asset: &'a Asset,
        world_pose: &'a WorldPose,
        stroke_width: f32,
        transform: Affine,
    ) -> &mut Self {
        self.add_asset_with_pose(asset, Some(world_pose), None, stroke_width, transform)
    }

    fn add_asset_with_pose(
        &mut self,
        asset: &'a Asset,
        world_pose: Option<&'a WorldPose>,
        shape_pose: Option<&'a ShapePose>,
        stroke_width: f32,
        transform: Affine,
    ) -> &mut Self {
        let mut flat_index = 0;
        for shape in &asset.shapes {
            let z = shape.z + shape_pose.map_or(0.0, |pose| pose.get(flat_index));
            self.0.push(Placement {
                shape,
                world_pose,
                shape_pose,
                flat_index,
                z,
                stroke_width,
                transform,
            });
            flat_index += shape.subtree_len();
        }
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
            world_pose: None,
            shape_pose: None,
            flat_index: 0,
            z: shape.z,
            stroke_width,
            transform,
        });
        self
    }

    /// Adds a single top-level shape, skinned by `world_pose` and placed by
    /// `transform`.
    pub fn add_shape_with_world_pose(
        &mut self,
        shape: &'a Shape,
        world_pose: &'a WorldPose,
        stroke_width: f32,
        transform: Affine,
    ) -> &mut Self {
        self.0.push(Placement {
            shape,
            world_pose: Some(world_pose),
            shape_pose: None,
            flat_index: 0,
            z: shape.z,
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
            placement.shape.draw(
                scene,
                placement.stroke_width,
                placement.transform,
                placement.world_pose,
                placement.shape_pose,
                placement.flat_index,
            );
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
                placement.shape.draw(
                    &mut scene,
                    placement.stroke_width,
                    placement.transform,
                    placement.world_pose,
                    placement.shape_pose,
                    placement.flat_index,
                );
                SceneFragment {
                    z: placement.z,
                    scene,
                }
            })
            .collect()
    }

    /// Stable sort by ascending effective Z, matching
    /// [`indices_in_paint_order`](crate::indices_in_paint_order).
    fn sort(&mut self) {
        self.0
            .sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(Ordering::Equal));
    }
}

impl Shape {
    /// Paints this shape and its descendants into a standalone fragment.
    ///
    /// `stroke_width` is in this shape's local units; `transform` places it in
    /// scene space. The fragment's Z is this shape's [`z`](Self::z).
    pub fn to_fragment(&self, stroke_width: f32, transform: Affine) -> SceneFragment {
        self.to_fragment_with_pose(None, stroke_width, transform)
    }

    /// Paints this shape and its descendants into a standalone fragment,
    /// skinned by `world_pose`.
    ///
    /// The pose moves every weighted point while the paths are emitted, so the
    /// shape itself is never copied or modified.
    pub fn to_fragment_with_world_pose(
        &self,
        world_pose: &WorldPose,
        stroke_width: f32,
        transform: Affine,
    ) -> SceneFragment {
        self.to_fragment_with_pose(Some(world_pose), stroke_width, transform)
    }

    fn to_fragment_with_pose(
        &self,
        world_pose: Option<&WorldPose>,
        stroke_width: f32,
        transform: Affine,
    ) -> SceneFragment {
        let mut scene = Scene::new();
        self.draw(&mut scene, stroke_width, transform, world_pose, None, 0);
        SceneFragment { z: self.z, scene }
    }

    /// Paints this shape and its descendants into `scene`, through `transform`.
    ///
    /// `flat_index` is this shape's depth-first pre-order index, which anchors
    /// the indices passed to its children for `shape_pose`.
    fn draw(
        &self,
        scene: &mut Scene,
        stroke_width: f32,
        transform: Affine,
        world_pose: Option<&WorldPose>,
        shape_pose: Option<&ShapePose>,
        flat_index: usize,
    ) {
        let path = self.to_bez_path_with_pose(world_pose);
        draw_fill(scene, &self.style, &path, transform);

        if !self.children.is_empty() {
            // Without paths there is nothing to clip against, so the children
            // are drawn unclipped rather than being made invisible.
            let clipped = !self.paths.is_empty();
            if clipped {
                scene.push_clip_layer(Fill::NonZero, transform, &path);
            }
            let child_flat = child_flat_indices(self, flat_index);
            let mut order: Vec<usize> = (0..self.children.len()).collect();
            order.sort_by(|&a, &b| {
                let z = |index: usize| {
                    self.children[index].z
                        + shape_pose.map_or(0.0, |pose| pose.get(child_flat[index]))
                };
                z(a).partial_cmp(&z(b)).unwrap_or(Ordering::Equal)
            });
            for index in order {
                self.children[index].draw(
                    scene,
                    stroke_width,
                    transform,
                    world_pose,
                    shape_pose,
                    child_flat[index],
                );
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
        self.to_bez_path_with_pose(None)
    }

    /// Like [`Self::to_bez_path`], but every weighted point is skinned by
    /// `world_pose` as it is emitted.
    pub fn to_bez_path_with_world_pose(&self, world_pose: &WorldPose) -> BezPath {
        self.to_bez_path_with_pose(Some(world_pose))
    }

    fn to_bez_path_with_pose(&self, world_pose: Option<&WorldPose>) -> BezPath {
        let mut bez = BezPath::new();
        for path in &self.paths {
            path.append_to(&mut bez, false, world_pose);
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
            path.append_to(&mut bez, true, None);
        }
        bez
    }
}

/// The flat pre-order index of each child of `shape`, given the parent's own
/// flat index.
///
/// Children are numbered in vector order, matching
/// [`ShapeIndex`](crate::ShapeIndex): the first child follows its parent, and
/// each subsequent child follows the previous child's whole subtree.
fn child_flat_indices(shape: &Shape, flat_index: usize) -> Vec<usize> {
    let mut indices = Vec::with_capacity(shape.children.len());
    let mut next = flat_index + 1;
    for child in &shape.children {
        indices.push(next);
        next += child.subtree_len();
    }
    indices
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
        self.to_bez_path_with_pose(None)
    }

    /// Like [`Self::to_bez_path`], but every weighted point is skinned by
    /// `world_pose` as it is emitted.
    pub fn to_bez_path_with_world_pose(&self, world_pose: &WorldPose) -> BezPath {
        self.to_bez_path_with_pose(Some(world_pose))
    }

    fn to_bez_path_with_pose(&self, world_pose: Option<&WorldPose>) -> BezPath {
        let mut bez = BezPath::new();
        self.append_to(&mut bez, false, world_pose);
        bez
    }

    /// Appends this path to `bez`, optionally forcing a closing edge.
    fn append_to(&self, bez: &mut BezPath, force_close: bool, world_pose: Option<&WorldPose>) {
        let Some(start_point) = self.start() else {
            return;
        };
        let start_point = match self.ends {
            PathEnds::Open(_) => deform_point(world_pose, &self.start_weights, start_point),
            // A closed loop has no separate start anchor; its start is the last
            // segment's end point, and that is where its weights live.
            PathEnds::Closed => match self.segments.last() {
                Some(last) => deform_point(world_pose, &last.to_weights, start_point),
                None => start_point,
            },
        };
        bez.move_to(to_kurbo_point(start_point));

        let mut previous = start_point;
        for segment in &self.segments {
            let to = deform_point(world_pose, &segment.to_weights, segment.to);
            let ctrl1 = segment.ctrl1.map_or(previous, |ctrl1| {
                deform_point(world_pose, &segment.ctrl1_weights, ctrl1)
            });
            let ctrl2 = match segment.ctrl2 {
                Some(ctrl2) => deform_point(world_pose, &segment.ctrl2_weights, ctrl2),
                None => to,
            };
            if segment.is_line() {
                bez.line_to(to_kurbo_point(to));
            } else {
                bez.curve_to(
                    to_kurbo_point(ctrl1),
                    to_kurbo_point(ctrl2),
                    to_kurbo_point(to),
                );
            }
            previous = to;
        }
        if self.is_closed() || force_close {
            bez.close_path();
        }
    }
}

/// Skins one point when a pose is present, leaving rest geometry untouched
/// otherwise.
fn deform_point(world_pose: Option<&WorldPose>, weights: &Weights, point: Point) -> Point {
    match world_pose {
        Some(pose) => pose.deform(weights, point),
        None => point,
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
    use crate::{Bone, BonePose, Pose};
    use vello::kurbo::{PathEl, Shape as _};

    fn assert_point(actual: KurboPoint, expected: KurboPoint) {
        assert!(
            (actual.x - expected.x).abs() < 1e-4 && (actual.y - expected.y).abs() < 1e-4,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// A root bone from the origin to [10, 0] with a connected child ending at
    /// [10, 10]. The path's start is weighted to the root and its end to the
    /// child.
    fn rigged_asset() -> Asset {
        let child = Bone {
            name: "Child".into(),
            head: None,
            tail: [10.0, 10.0],
            children: Vec::new(),
        };
        let parent = Bone {
            name: "Parent".into(),
            head: Some([0.0, 0.0]),
            tail: [10.0, 0.0],
            children: vec![child],
        };
        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 10.0]);
        path.start_weights.set(0, 1.0);
        path.segments[0].to_weights.set(1, 1.0);
        Asset {
            shapes: vec![Shape {
                paths: vec![path],
                ..Default::default()
            }],
            bones: vec![parent],
            animations: Vec::new(),
        }
    }

    #[test]
    fn a_rest_instance_needs_no_world_pose() {
        let asset = rigged_asset();

        let rest = SceneInstance::animated(&asset, &[]);
        assert!(rest.world_pose.is_none());

        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: 0.5,
                ..Default::default()
            },
        );
        let posed = SceneInstance::posed(&asset, &pose);
        assert!(posed.world_pose.is_some());
    }

    #[test]
    fn world_pose_skins_points_while_drawing() {
        let asset = rigged_asset();
        let mut pose = Pose::default();
        pose.set(
            0,
            BonePose {
                rotation: std::f32::consts::FRAC_PI_2,
                ..Default::default()
            },
        );

        let world = WorldPose::new(&asset, &pose);
        let drawn = asset.shapes[0].to_bez_path_with_world_pose(&world);

        // The deformed asset produces the same geometry, but by cloning and
        // rewriting every point first.
        let deformed = asset.deformed(&pose);
        let expected = deformed.shapes[0].to_bez_path();

        let drawn: Vec<PathEl> = drawn.elements().to_vec();
        let expected: Vec<PathEl> = expected.elements().to_vec();
        assert_eq!(drawn.len(), expected.len());
        for (drawn, expected) in drawn.iter().zip(&expected) {
            match (drawn, expected) {
                (PathEl::MoveTo(a), PathEl::MoveTo(b)) | (PathEl::LineTo(a), PathEl::LineTo(b)) => {
                    assert_point(*a, *b)
                }
                _ => panic!("unexpected path difference: {drawn:?} vs {expected:?}"),
            }
        }

        // A quarter turn about the root head maps [10, 10] to [-10, 10].
        assert!(matches!(drawn[0], PathEl::MoveTo(p) if p == KurboPoint::new(0.0, 0.0)));
        assert!(
            matches!(drawn[1], PathEl::LineTo(p) if (p.x + 10.0).abs() < 1e-4 && (p.y - 10.0).abs() < 1e-4)
        );
    }

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

        let order: Vec<f32> = builder.0.iter().map(|placement| placement.z).collect();
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

    #[test]
    fn shape_z_offsets_reorder_top_level_fragments() {
        let shape = |name: &str, z: f32| Shape {
            name: name.into(),
            z,
            paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        };
        let asset = Asset {
            shapes: vec![shape("low", 0.0), shape("high", 1.0)],
            ..Default::default()
        };
        // Lift the first shape above the second.
        let mut pose = ShapePose::new(2);
        pose.set(0, 10.0);

        let fragments = {
            let mut builder = SceneBuilder::new();
            builder.add_asset_with_shape_pose(&asset, &pose, 1.0);
            builder.into_fragments()
        };

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].z, 1.0);
        assert_eq!(fragments[1].z, 10.0);
    }

    #[test]
    fn animated_instances_apply_shape_z_curves() {
        use crate::animation::{Animation, Keyframe, KeyframeTarget, ShapeProperty};

        let shape = |z: f32| Shape {
            z,
            paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        };
        let mut animation = Animation::new(1.0);
        animation.insert_keyframe(Keyframe::new(
            0.0,
            10.0,
            KeyframeTarget::Shape {
                shape: 0,
                property: ShapeProperty::Z,
            },
        ));
        let asset = Asset {
            shapes: vec![shape(0.0), shape(1.0)],
            animations: vec![animation],
            ..Default::default()
        };

        let instance = SceneInstance::animated(&asset, &[(0, 0.0, 1.0)]);
        assert!(instance.shape_pose.is_some());
        let mut builder = SceneBuilder::new();
        builder.add_instance(&instance);
        let fragments = builder.into_fragments();

        assert_eq!(fragments[0].z, 1.0);
        assert_eq!(fragments[1].z, 10.0);
    }

    #[test]
    fn shape_z_offsets_reorder_nested_children() {
        let shape = |name: &str, z: f32| Shape {
            name: name.into(),
            z,
            ..Default::default()
        };
        let asset = Asset {
            shapes: vec![Shape {
                z: 0.0,
                children: vec![shape("a", 0.0), shape("b", 1.0)],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Index 0 is the parent, 1 is "a", and 2 is "b".
        let mut pose = ShapePose::new(3);
        pose.set(1, 10.0);

        let mut builder = SceneBuilder::new();
        builder.add_asset_with_shape_pose(&asset, &pose, 1.0);
        let placements = &builder.0;
        let child_flat = child_flat_indices(placements[0].shape, 0);
        assert_eq!(child_flat, vec![1, 2]);

        let order: Vec<usize> = {
            let mut order: Vec<usize> = (0..2).collect();
            order.sort_by(|&a, &b| {
                let z = |index: usize| {
                    placements[0].shape.children[index].z + pose.get(child_flat[index])
                };
                z(a).partial_cmp(&z(b)).unwrap()
            });
            order
        };
        // "b" is animated past "a", so it is painted first (further back).
        assert_eq!(order, vec![1, 0]);
    }
}
