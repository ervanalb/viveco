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
use crate::{
    AnimationInstance, Asset, Path, PathEnds, Point, Rgba8, Shape, ShapeIndex, Style, Weights,
};

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
#[derive(Clone)]
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

/// A placed node in a document tree.
///
/// A node owns an optional document and a list of children. Each child is
/// content grafted at a context: the node's root paint level (`shape: None`) or
/// one shape of the node's document (`shape: Some(index)`). Root content shares
/// this node's paint level, unclipped relative to it; shape content is painted
/// inside that shape's clip and competes with its children by Z. Children nest
/// to any depth.
///
/// [`placement`](Self::placement) is the scene-space placement of this node's
/// content and already includes the node's own local transform. It is not
/// composed with any Viveco parent; each node is placed independently. The
/// [`SceneInstance::transform`] of a stored instance is ignored; use the node's
/// placement instead.
///
/// Add nodes to a [`SceneBuilder`] with [`SceneBuilder::add_node`].
pub struct DrawNode<'a> {
    /// Scene-space placement of this node's content.
    pub placement: Affine,
    /// The node's own document, if it has one.
    pub instance: Option<SceneInstance<'a>>,
    /// Content attached to this node, by context.
    pub children: Vec<DrawChild<'a>>,
}

impl<'a> DrawNode<'a> {
    /// Starts an empty node placed at `placement` in scene space.
    pub fn new(placement: Affine) -> Self {
        Self {
            placement,
            instance: None,
            children: Vec::new(),
        }
    }

    /// Sets this node's document.
    pub fn with_instance(mut self, instance: SceneInstance<'a>) -> Self {
        self.instance = Some(instance);
        self
    }

    /// Adds one child.
    pub fn with_child(mut self, child: DrawChild<'a>) -> Self {
        self.children.push(child);
        self
    }

    /// Adds custom scenes at this node's root paint level.
    pub fn with_fragments(mut self, fragments: impl IntoIterator<Item = SceneFragment>) -> Self {
        self.children
            .extend(fragments.into_iter().map(DrawChild::root_fragment));
        self
    }

    /// Adds a node attached at this node's root.
    pub fn with_root_child(self, child: DrawNode<'a>) -> Self {
        self.with_child(DrawChild::root_node(child))
    }

    /// Adds a node attached inside `shape` of this node's document.
    pub fn with_shape_child(self, shape: ShapeIndex, child: DrawNode<'a>) -> Self {
        self.with_child(DrawChild::shape_node(shape, child))
    }

    /// Adds custom content inside `shape` of this node's document.
    pub fn with_shape_fragment(self, shape: ShapeIndex, fragment: SceneFragment) -> Self {
        self.with_child(DrawChild::shape_fragment(shape, fragment))
    }

    /// Sorts every child list in this subtree by context.
    ///
    /// Root-context children (`None`) come first, then shape-context children by
    /// shape index. The sort is stable, so equal contexts keep the order they
    /// were added. The renderer's per-shape lookup relies on this order.
    pub fn sort_children(&mut self) {
        self.children.sort_by_key(|child| child.shape);
        for child in &mut self.children {
            if let DrawContent::Node(node) = &mut child.content {
                node.sort_children();
            }
        }
    }
}

/// One piece of content attached to a node.
pub struct DrawChild<'a> {
    /// `None` attaches at the node's root paint level; `Some(shape)` attaches
    /// inside that shape of the node's document.
    pub shape: Option<ShapeIndex>,
    /// What is attached.
    pub content: DrawContent<'a>,
}

impl<'a> DrawChild<'a> {
    /// A nested node at the parent's root level.
    pub fn root_node(node: DrawNode<'a>) -> Self {
        Self {
            shape: None,
            content: DrawContent::Node(node),
        }
    }

    /// A nested node inside `shape` of the parent's document.
    pub fn shape_node(shape: ShapeIndex, node: DrawNode<'a>) -> Self {
        Self {
            shape: Some(shape),
            content: DrawContent::Node(node),
        }
    }

    /// Custom content at the parent's root level.
    pub fn root_fragment(fragment: SceneFragment) -> Self {
        Self {
            shape: None,
            content: DrawContent::Fragment(fragment),
        }
    }

    /// Custom content inside `shape` of the parent's document.
    pub fn shape_fragment(shape: ShapeIndex, fragment: SceneFragment) -> Self {
        Self {
            shape: Some(shape),
            content: DrawContent::Fragment(fragment),
        }
    }
}

/// Content attached at one context.
pub enum DrawContent<'a> {
    /// A nested node subtree.
    Node(DrawNode<'a>),
    /// Custom Vello content, authored in the parent node's local space.
    Fragment(SceneFragment),
}

/// One stand-alone piece of a scene, with the Z at which to draw it.
///
/// A fragment owns a complete Vello scene and the placement to append it
/// through, so fragments can be sorted, stored, and appended in any order.
/// [`SceneBuilder::into_fragments`] produces one fragment per top-level shape,
/// which is how the Bevy integration keeps instance artwork Z-sorted against
/// custom fragments. When a shape has animated offsets, its fragment's Z is the
/// effective Z, not the authored one.
pub struct SceneFragment {
    /// Sort key: fragments are drawn in ascending Z.
    pub z: f32,
    /// Placement applied when the fragment is appended.
    pub transform: Affine,
    /// The fragment's complete Vello content.
    pub scene: Scene,
}

impl SceneFragment {
    /// A fragment drawn through the identity transform.
    pub fn new(z: f32, scene: Scene) -> Self {
        Self {
            z,
            transform: Affine::IDENTITY,
            scene,
        }
    }
}

/// One scene-level item queued in a [`SceneBuilder`].
enum SceneItem<'a> {
    /// One top-level shape of a node's document.
    Shape {
        node: usize,
        top_level: usize,
        flat_index: usize,
        z: f32,
    },
    /// One stand-alone shape added directly to the builder.
    StandaloneShape {
        shape: &'a Shape,
        world_pose: Option<&'a WorldPose>,
        shape_pose: Option<&'a ShapePose>,
        stroke_width: f32,
        transform: Affine,
        z: f32,
    },
    /// One custom scene, already placed relative to its node.
    Fragment {
        z: f32,
        transform: Affine,
        /// Boxed so the item enum stays sized by the common shape variants
        /// rather than by `Scene`. Custom fragments are usually few.
        scene: Box<Scene>,
    },
}

impl SceneItem<'_> {
    fn z(&self) -> f32 {
        match self {
            Self::Shape { z, .. } | Self::StandaloneShape { z, .. } | Self::Fragment { z, .. } => {
                *z
            }
        }
    }
}

/// A root-closure node retained by a [`SceneBuilder`] for drawing its shapes.
struct SceneNode<'a> {
    placement: Affine,
    instance: Option<SceneInstance<'a>>,
    /// The node's shape-context children, retained for shape draws.
    children: Vec<DrawChild<'a>>,
}

/// Collects top-level shapes and custom scenes from one or more nodes and
/// paints them into a [`Scene`], sorted by Z across all of them.
///
/// Shapes are painted in ascending Z; equal Z keeps the order they were added.
/// A node's root-context children share its paint level, while its shape-context
/// children are painted inside the shape's clip layer.
#[derive(Default)]
pub struct SceneBuilder<'a> {
    nodes: Vec<SceneNode<'a>>,
    items: Vec<SceneItem<'a>>,
}

impl<'a> SceneBuilder<'a> {
    /// Starts an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node and everything attached at its root.
    ///
    /// Root-context children are flattened into this builder's paint level.
    /// Shape-context children stay with the node and are resolved while its
    /// shapes are drawn.
    pub fn add_node(&mut self, mut node: DrawNode<'a>) -> &mut Self {
        node.sort_children();
        self.add_node_at_root(node);
        self
    }

    fn add_node_at_root(&mut self, node: DrawNode<'a>) {
        let DrawNode {
            placement,
            instance,
            children,
        } = node;

        let node_index = self.nodes.len();
        self.nodes.push(SceneNode {
            placement,
            instance,
            children: Vec::new(),
        });

        if let Some(instance) = &self.nodes[node_index].instance {
            let mut flat_index = 0;
            for (top_level, shape) in instance.asset.shapes.iter().enumerate() {
                let z = shape.z
                    + instance
                        .shape_pose
                        .as_ref()
                        .map_or(0.0, |pose| pose.get(flat_index));
                self.items.push(SceneItem::Shape {
                    node: node_index,
                    top_level,
                    flat_index,
                    z,
                });
                flat_index += shape.subtree_len();
            }
        }

        // Shape-context children stay with the node for its shape draws; root
        // children are expanded here in the same pass.
        let mut shape_children = Vec::new();
        for child in children {
            match child.shape {
                Some(_) => shape_children.push(child),
                None => match child.content {
                    DrawContent::Fragment(fragment) => {
                        let SceneFragment {
                            z,
                            transform,
                            scene,
                        } = fragment;
                        self.items.push(SceneItem::Fragment {
                            z,
                            transform: placement * transform,
                            scene: Box::new(scene),
                        });
                    }
                    DrawContent::Node(child_node) => self.add_node_at_root(child_node),
                },
            }
        }
        self.nodes[node_index].children = shape_children;
    }

    /// Adds every top-level shape of `instance` as a root node.
    pub fn add_instance(&mut self, instance: &'a SceneInstance<'a>) -> &mut Self {
        let mut instance = instance.clone();
        let placement = instance.transform;
        instance.transform = Affine::IDENTITY;
        self.add_node(DrawNode::new(placement).with_instance(instance))
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
        let instance = SceneInstance {
            asset,
            world_pose: world_pose.cloned(),
            shape_pose: shape_pose.cloned(),
            transform: Affine::IDENTITY,
            stroke_width,
        };
        self.add_node(DrawNode::new(transform).with_instance(instance))
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
        self.items.push(SceneItem::StandaloneShape {
            shape,
            world_pose: None,
            shape_pose: None,
            stroke_width,
            transform,
            z: shape.z,
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
        self.items.push(SceneItem::StandaloneShape {
            shape,
            world_pose: Some(world_pose),
            shape_pose: None,
            stroke_width,
            transform,
            z: shape.z,
        });
        self
    }

    /// Sorts the collected items by Z and paints them into `scene`, appending
    /// to whatever the scene already contains.
    pub fn finish(mut self, scene: &mut Scene) {
        self.sort();
        let nodes = &self.nodes;
        for item in &self.items {
            match item {
                SceneItem::Shape {
                    node,
                    top_level,
                    flat_index,
                    ..
                } => {
                    let node = &nodes[*node];
                    if let Some(instance) = &node.instance {
                        instance.asset.shapes[*top_level].draw(
                            scene,
                            instance.stroke_width,
                            node.placement,
                            instance.world_pose.as_ref(),
                            instance.shape_pose.as_ref(),
                            *flat_index,
                            &node.children,
                        );
                    }
                }
                SceneItem::StandaloneShape {
                    shape,
                    world_pose,
                    shape_pose,
                    stroke_width,
                    transform,
                    ..
                } => {
                    shape.draw(
                        scene,
                        *stroke_width,
                        *transform,
                        *world_pose,
                        *shape_pose,
                        0,
                        &[],
                    );
                }
                SceneItem::Fragment {
                    transform,
                    scene: fragment,
                    ..
                } => {
                    scene.append(fragment, Some(*transform));
                }
            }
        }
    }

    /// Sorts the collected items by Z and paints each one into its own
    /// [`SceneFragment`].
    ///
    /// One fragment per top-level shape or custom scene. A node's root-context
    /// children join this level, so their shapes and custom scenes are separate
    /// fragments too.
    pub fn into_fragments(mut self) -> Vec<SceneFragment> {
        self.sort();
        let Self { nodes, items } = self;
        items
            .into_iter()
            .map(|item| match item {
                SceneItem::Shape {
                    node,
                    top_level,
                    flat_index,
                    z,
                } => {
                    let node = &nodes[node];
                    let mut scene = Scene::new();
                    if let Some(instance) = &node.instance {
                        instance.asset.shapes[top_level].draw(
                            &mut scene,
                            instance.stroke_width,
                            node.placement,
                            instance.world_pose.as_ref(),
                            instance.shape_pose.as_ref(),
                            flat_index,
                            &node.children,
                        );
                    }
                    SceneFragment::new(z, scene)
                }
                SceneItem::StandaloneShape {
                    shape,
                    world_pose,
                    shape_pose,
                    stroke_width,
                    transform,
                    z,
                } => {
                    let mut scene = Scene::new();
                    shape.draw(
                        &mut scene,
                        stroke_width,
                        transform,
                        world_pose,
                        shape_pose,
                        0,
                        &[],
                    );
                    SceneFragment::new(z, scene)
                }
                SceneItem::Fragment {
                    z,
                    transform,
                    scene,
                } => SceneFragment {
                    z,
                    transform,
                    scene: *scene,
                },
            })
            .collect()
    }

    /// Stable sort by ascending effective Z, matching
    /// [`indices_in_paint_order`](crate::indices_in_paint_order).
    fn sort(&mut self) {
        self.items
            .sort_by(|a, b| a.z().partial_cmp(&b.z()).unwrap_or(Ordering::Equal));
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
        self.draw(
            &mut scene,
            stroke_width,
            transform,
            world_pose,
            None,
            0,
            &[],
        );
        SceneFragment::new(self.z, scene)
    }

    /// Paints this shape and its descendants into `scene`, through `transform`.
    ///
    /// `flat_index` is this shape's depth-first pre-order index, which anchors
    /// the indices passed to its children for `shape_pose`. `children` are the
    /// owning node's children: those attached to this shape contribute their
    /// content to this shape's clip layer, where it competes with the shape's
    /// children by Z. `children` must be sorted by context;
    /// [`DrawNode::sort_children`] maintains that order.
    #[allow(clippy::too_many_arguments)]
    fn draw<'n, 'a>(
        &self,
        scene: &mut Scene,
        stroke_width: f32,
        transform: Affine,
        world_pose: Option<&WorldPose>,
        shape_pose: Option<&ShapePose>,
        flat_index: usize,
        children: &'n [DrawChild<'a>],
    ) {
        let path = self.to_bez_path_with_pose(world_pose);
        draw_fill(scene, &self.style, &path, transform);

        let mut nodes = Vec::new();
        let painters = painters_in_paint_order(
            self, flat_index, shape_pose, transform, children, &mut nodes,
        );

        if !painters.is_empty() {
            // Without paths there is nothing to clip against, so the children
            // are drawn unclipped rather than being made invisible.
            let clipped = !self.paths.is_empty();
            if clipped {
                scene.push_clip_layer(Fill::NonZero, transform, &path);
            }
            for (_, painter) in painters {
                match painter {
                    Painter::Child { index, flat_index } => self.children[index].draw(
                        scene,
                        stroke_width,
                        transform,
                        world_pose,
                        shape_pose,
                        flat_index,
                        children,
                    ),
                    Painter::NodeShape {
                        node,
                        top_level,
                        flat_index,
                    } => {
                        let node = nodes[node];
                        if let Some(instance) = &node.instance {
                            instance.asset.shapes[top_level].draw(
                                scene,
                                instance.stroke_width,
                                node.placement,
                                instance.world_pose.as_ref(),
                                instance.shape_pose.as_ref(),
                                flat_index,
                                &node.children,
                            );
                        }
                    }
                    Painter::Fragment {
                        fragment,
                        transform,
                    } => {
                        scene.append(&fragment.scene, Some(transform));
                    }
                }
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
/// each subsequent child follows the previous child's whole subtree. The
/// renderer computes these inline; this helper exists for tests.
#[cfg(test)]
fn child_flat_indices(shape: &Shape, flat_index: usize) -> Vec<usize> {
    let mut indices = Vec::with_capacity(shape.children.len());
    let mut next = flat_index + 1;
    for child in &shape.children {
        indices.push(next);
        next += child.subtree_len();
    }
    indices
}

/// One painter queued inside a shape.
enum Painter<'n> {
    /// One of the shape's own children.
    Child {
        /// Index into the shape's children.
        index: usize,
        /// The child's flat depth-first pre-order index.
        flat_index: usize,
    },
    /// A top-level shape of an attached node.
    NodeShape {
        /// Index into the node arena built for this draw.
        node: usize,
        /// Index into the node's document top-level shapes.
        top_level: usize,
        /// The top-level shape's flat index in the node's document.
        flat_index: usize,
    },
    /// Custom content attached to this shape.
    Fragment {
        /// The custom scene.
        fragment: &'n SceneFragment,
        /// Its scene-space placement.
        transform: Affine,
    },
}

/// Collects the painters inside `shape` in paint order.
///
/// Child shapes and every child attached to this shape share one Z space,
/// matching [`SceneBuilder`]'s ordering across documents. The sort is stable, so
/// equal Z keeps the children's tree order, then the attached content in the
/// order it was added.
fn painters_in_paint_order<'n, 'a>(
    shape: &Shape,
    flat_index: usize,
    shape_pose: Option<&ShapePose>,
    transform: Affine,
    children: &'n [DrawChild<'a>],
    nodes: &mut Vec<&'n DrawNode<'a>>,
) -> Vec<(f32, Painter<'n>)> {
    let mut painters = Vec::with_capacity(shape.children.len());

    // Flat indices follow the children in vector order, so a running counter
    // avoids allocating an index Vec per shape per frame.
    let mut child_flat = flat_index + 1;
    for (index, child) in shape.children.iter().enumerate() {
        let z = child.z + shape_pose.map_or(0.0, |pose| pose.get(child_flat));
        painters.push((
            z,
            Painter::Child {
                index,
                flat_index: child_flat,
            },
        ));
        child_flat += child.subtree_len();
    }

    // Children are sorted by context, so two binary searches find the ones
    // attached to this shape instead of scanning all of them.
    let context = Some(flat_index);
    let first = children.partition_point(|child| child.shape < context);
    let past_last = children.partition_point(|child| child.shape <= context);
    for child in &children[first..past_last] {
        match &child.content {
            DrawContent::Fragment(fragment) => painters.push((
                fragment.z,
                Painter::Fragment {
                    fragment,
                    transform: transform * fragment.transform,
                },
            )),
            DrawContent::Node(node) => push_node_painters(node, nodes, &mut painters),
        }
    }

    painters.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    painters
}

/// Adds the root-level content of `node` and its root-context children to
/// `painters`.
///
/// Content attached to the node's shapes is not added here: it is resolved
/// while each of the node's top-level shapes is drawn.
fn push_node_painters<'n, 'a>(
    node: &'n DrawNode<'a>,
    nodes: &mut Vec<&'n DrawNode<'a>>,
    painters: &mut Vec<(f32, Painter<'n>)>,
) {
    let node_index = nodes.len();
    nodes.push(node);

    if let Some(instance) = &node.instance {
        let mut flat_index = 0;
        for (top_level, top_shape) in instance.asset.shapes.iter().enumerate() {
            let z = top_shape.z
                + instance
                    .shape_pose
                    .as_ref()
                    .map_or(0.0, |pose| pose.get(flat_index));
            painters.push((
                z,
                Painter::NodeShape {
                    node: node_index,
                    top_level,
                    flat_index,
                },
            ));
            flat_index += top_shape.subtree_len();
        }
    }

    for child in &node.children {
        if child.shape.is_some() {
            continue;
        }
        match &child.content {
            DrawContent::Fragment(fragment) => painters.push((
                fragment.z,
                Painter::Fragment {
                    fragment,
                    transform: node.placement * fragment.transform,
                },
            )),
            DrawContent::Node(child_node) => push_node_painters(child_node, nodes, painters),
        }
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

        let order: Vec<f32> = builder.items.iter().map(SceneItem::z).collect();
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
            .items
            .iter()
            .map(|item| match item {
                SceneItem::Shape {
                    node, top_level, ..
                } => builder.nodes[*node].instance.as_ref().unwrap().asset.shapes[*top_level]
                    .name
                    .as_str(),
                SceneItem::StandaloneShape { shape, .. } => shape.name.as_str(),
                SceneItem::Fragment { .. } => "",
            })
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

        assert_eq!(builder.items.len(), 4);
        assert_eq!(builder.nodes[0].placement, Affine::IDENTITY);
        assert_eq!(builder.nodes[1].placement, transform);
        assert_eq!(
            builder.nodes[1].instance.as_ref().unwrap().stroke_width,
            2.0
        );
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
        let shape = &builder.nodes[0].instance.as_ref().unwrap().asset.shapes[0];
        let child_flat = child_flat_indices(shape, 0);
        assert_eq!(child_flat, vec![1, 2]);

        let order: Vec<usize> = {
            let mut order: Vec<usize> = (0..2).collect();
            order.sort_by(|&a, &b| {
                let z = |index: usize| shape.children[index].z + pose.get(child_flat[index]);
                z(a).partial_cmp(&z(b)).unwrap()
            });
            order
        };
        // "b" is animated past "a", so it is painted first (further back).
        assert_eq!(order, vec![1, 0]);
    }

    fn filled_rect(min: [f32; 2], max: [f32; 2]) -> Shape {
        Shape {
            paths: vec![
                Path::new(min)
                    .line_to([max[0], min[1]])
                    .line_to(max)
                    .line_to([min[0], max[1]])
                    .closed(),
            ],
            style: Style {
                fill: Some(Rgba8::rgb(255, 0, 0)),
                stroke: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn an_attachment_is_clipped_by_its_host_shape() {
        let host = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };
        let child = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [20.0, 20.0])],
            ..Default::default()
        };
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_shape_child(
                0,
                DrawNode::new(Affine::translate((2.0, 3.0)))
                    .with_instance(SceneInstance::new(&child)),
            );

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        let encoding = scene.encoding();
        // Two fills plus the clip layer's begin/end bookkeeping paths.
        assert_eq!(
            encoding.n_paths, 4,
            "the host and the child are both filled"
        );
        assert_eq!(encoding.n_clips, 2, "the child is clipped by the host");
        assert_eq!(encoding.n_open_clips, 0);
        assert!(
            encoding
                .transforms
                .iter()
                .any(|transform| transform.translation == [2.0, 3.0]),
            "the child's placement is baked into the geometry"
        );
    }

    #[test]
    fn an_attachment_can_target_a_nested_shape() {
        let host = Asset {
            shapes: vec![Shape {
                paths: filled_rect([0.0, 0.0], [10.0, 10.0]).paths,
                children: vec![filled_rect([2.0, 2.0], [8.0, 8.0])],
                ..Default::default()
            }],
            ..Default::default()
        };
        let child = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [4.0, 4.0])],
            ..Default::default()
        };
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_shape_child(
                1,
                DrawNode::new(Affine::IDENTITY).with_instance(SceneInstance::new(&child)),
            );

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        let encoding = scene.encoding();
        // The top-level shape has no fill; the nested shape and the attachment
        // contribute one fill each, plus two clip layers' bookkeeping paths.
        assert_eq!(encoding.n_paths, 6);
        // The top-level shape clips its child, and that child clips the
        // attachment: two layers, each with a begin and an end.
        assert_eq!(encoding.n_clips, 4);
        assert_eq!(encoding.n_open_clips, 0);
    }

    #[test]
    fn attachments_nest_to_arbitrary_depth() {
        // Three documents, each attached inside shape 0 of the previous one.
        let bottom = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [4.0, 4.0])],
            ..Default::default()
        };
        let middle = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [8.0, 8.0])],
            ..Default::default()
        };
        let top = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };

        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&top))
            .with_shape_child(
                0,
                DrawNode::new(Affine::IDENTITY)
                    .with_instance(SceneInstance::new(&middle))
                    .with_shape_child(
                        0,
                        DrawNode::new(Affine::IDENTITY).with_instance(SceneInstance::new(&bottom)),
                    ),
            );

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        let encoding = scene.encoding();
        // Three fills plus two nested clip layers' bookkeeping paths.
        assert_eq!(encoding.n_paths, 7);
        assert_eq!(encoding.n_clips, 4);
        assert_eq!(encoding.n_open_clips, 0);
    }

    #[test]
    fn an_attachment_for_a_missing_shape_is_not_drawn() {
        let host = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };
        let child = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [4.0, 4.0])],
            ..Default::default()
        };
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_shape_child(
                99,
                DrawNode::new(Affine::IDENTITY).with_instance(SceneInstance::new(&child)),
            );

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        assert_eq!(scene.encoding().n_paths, 1);
        assert_eq!(scene.encoding().n_clips, 0);
    }

    #[test]
    fn an_attachments_top_level_shapes_compete_with_the_hosts_children_by_z() {
        // The host has no paths, so its interior is an unclipped group.
        let host = Shape {
            children: vec![
                Shape {
                    name: "back".into(),
                    z: 0.0,
                    ..Default::default()
                },
                Shape {
                    name: "front".into(),
                    z: 3.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let child = Asset {
            shapes: vec![
                Shape {
                    name: "low".into(),
                    z: 1.0,
                    ..Default::default()
                },
                Shape {
                    name: "high".into(),
                    z: 2.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let children = [DrawChild::shape_node(
            0,
            DrawNode::new(Affine::IDENTITY).with_instance(SceneInstance::new(&child)),
        )];

        let mut nodes = Vec::new();
        let painters =
            painters_in_paint_order(&host, 0, None, Affine::IDENTITY, &children, &mut nodes);

        let names: Vec<&str> = painters
            .iter()
            .map(|(_, painter)| match painter {
                Painter::Child { index, .. } => host.children[*index].name.as_str(),
                Painter::NodeShape {
                    node, top_level, ..
                } => nodes[*node].instance.as_ref().unwrap().asset.shapes[*top_level]
                    .name
                    .as_str(),
                Painter::Fragment { .. } => "",
            })
            .collect();

        // The child's shapes are interleaved with the host shape's children by
        // their own Z, exactly like top-level shapes of separate assets.
        assert_eq!(names, vec!["back", "low", "high", "front"]);
    }

    #[test]
    fn an_attachment_can_draw_custom_scenes_inside_the_clip() {
        let host = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };
        let mut custom = Scene::new();
        custom.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 255, 255),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0),
        );
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_shape_child(
                0,
                DrawNode::new(Affine::translate((2.0, 3.0)))
                    .with_fragments([SceneFragment::new(1.0, custom)]),
            );

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        let encoding = scene.encoding();
        // The host fill, the custom fill, and the clip layer's bookkeeping.
        assert_eq!(encoding.n_paths, 4);
        assert_eq!(encoding.n_clips, 2);
        assert_eq!(encoding.n_open_clips, 0);
        assert!(
            encoding
                .transforms
                .iter()
                .any(|transform| transform.translation == [2.0, 3.0]),
            "the custom scene is placed inside the host document"
        );
    }

    #[test]
    fn a_shape_fragment_is_drawn_inside_its_shape() {
        let host = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };
        let mut custom = Scene::new();
        custom.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 255, 255),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0),
        );
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_shape_fragment(0, SceneFragment::new(1.0, custom));

        let mut scene = Scene::new();
        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        builder.finish(&mut scene);

        let encoding = scene.encoding();
        // The host fill, the custom fill, and the clip layer's bookkeeping.
        assert_eq!(encoding.n_paths, 4);
        assert_eq!(encoding.n_clips, 2);
        assert_eq!(encoding.n_open_clips, 0);
    }

    #[test]
    fn shape_fragments_compete_with_the_shapes_children_by_z() {
        let host = Shape {
            children: vec![
                Shape {
                    name: "back".into(),
                    z: 0.0,
                    ..Default::default()
                },
                Shape {
                    name: "front".into(),
                    z: 3.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let children = [DrawChild::shape_fragment(
            0,
            SceneFragment::new(1.0, Scene::new()),
        )];
        let mut nodes = Vec::new();
        let painters =
            painters_in_paint_order(&host, 0, None, Affine::IDENTITY, &children, &mut nodes);

        let order: Vec<&str> = painters
            .iter()
            .map(|(_, painter)| match painter {
                Painter::Child { index, .. } => host.children[*index].name.as_str(),
                Painter::Fragment { .. } => "fragment",
                Painter::NodeShape { .. } => "node",
            })
            .collect();
        assert_eq!(order, vec!["back", "fragment", "front"]);
    }

    #[test]
    fn root_children_join_the_scene_level() {
        let host = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [10.0, 10.0])],
            ..Default::default()
        };
        let child = Asset {
            shapes: vec![filled_rect([0.0, 0.0], [4.0, 4.0])],
            ..Default::default()
        };
        let node = DrawNode::new(Affine::IDENTITY)
            .with_instance(SceneInstance::new(&host))
            .with_root_child(
                DrawNode::new(Affine::translate((5.0, 6.0)))
                    .with_instance(SceneInstance::new(&child)),
            );

        let mut builder = SceneBuilder::new();
        builder.add_node(node);
        let fragments = builder.into_fragments();

        // One fragment per top-level shape across the root closure.
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].z, 0.0);
        assert_eq!(fragments[1].z, 0.0);
        assert!(
            fragments[1]
                .scene
                .encoding()
                .transforms
                .iter()
                .any(|transform| transform.translation == [5.0, 6.0]),
            "the root child is placed relative to its parent"
        );
    }
}
