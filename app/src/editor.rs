//! Selection and control-point editing.
//!
//! The rendered document lives on the GPU as a Vello scene, so interaction is
//! done against the CPU-side [`Document`]: clicks are mapped from screen pixels
//! into document space and tested against the path geometry in painting order.

use std::cmp::Ordering;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::prelude::{VelloScene2d, vello};
use vello::Scene;
use vello::kurbo::{
    Affine, Circle, Line as KurboLine, ParamCurveNearest, Point as KurboPoint, Shape,
    Stroke as KurboStroke,
};
use vello::peniko::{Color, Fill};

use crate::model::{Bounds, ClipGroup, Document, Item, Path, PathItem, Segment, Style};
use crate::render::{build_scene, path_to_bez_path};

/// Bounds of the loaded document, used to fit it to the window.
///
/// Kept fixed for the session so that dragging a point does not rescale the
/// whole image under the cursor.
#[derive(Resource, Clone, Copy)]
pub struct DocumentBounds(pub Bounds);

/// Marks the entity holding the rendered document.
#[derive(Component)]
pub struct DocumentScene;

/// The path currently selected for editing, addressed by tree position.
///
/// `items` is a chain of item indices: `[i]` addresses `layer.items[i]`, while
/// `[i, j]` addresses `layer.items[i]` (a clip group)'s `targets[j]`, and so
/// on. [`PathAddress::Clip`] uses the same chain but selects the clip path of
/// the clip group at that position instead of one of its targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathAddress {
    /// A painted path stored in a [`PathItem`].
    Item { layer: usize, items: Vec<usize> },
    /// The clip path of the [`ClipGroup`] at the given position.
    Clip { layer: usize, items: Vec<usize> },
}

impl PathAddress {
    /// Descends one level: `index` becomes the first step of the chain.
    fn push_item(&mut self, index: usize) {
        match self {
            Self::Item { items, .. } | Self::Clip { items, .. } => items.insert(0, index),
        }
    }
}

/// A draggable point of a [`Path`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Start,
    LineTo(usize),
    CubicCtrl1(usize),
    CubicCtrl2(usize),
    CubicTo(usize),
}

#[derive(Clone, Debug)]
struct Drag {
    address: PathAddress,
    handle: Handle,
}

/// Editor state: the mutable document plus the current selection.
#[derive(Resource)]
pub struct EditorState {
    pub document: Document,
    pub selected: Option<PathAddress>,
    drag: Option<Drag>,
}

impl EditorState {
    pub fn new(document: Document) -> Self {
        Self {
            document,
            selected: None,
            drag: None,
        }
    }
}

/// Systems for selection, dragging, fitting, and redrawing.
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (handle_input, refresh_scene, fit_document).chain());
    }
}

/// Centers the document in the window and scales it to fill 90% of the view.
pub fn fit_transform(bounds: Bounds, viewport_width: f32, viewport_height: f32) -> Transform {
    let doc_width = bounds.width().max(f32::EPSILON);
    let doc_height = bounds.height().max(f32::EPSILON);
    let scale = 0.9 * (viewport_width / doc_width).min(viewport_height / doc_height);

    // Document Y points down, so shifting the document's centre to the screen
    // centre means moving the entity *up* by `scale * centre.y`.
    let center = bounds.center();
    Transform::from_xyz(-scale * center[0], scale * center[1], 0.0).with_scale(Vec3::splat(scale))
}

fn fit_document(
    window: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    mut scenes: Query<&mut Transform, With<DocumentScene>>,
) {
    let Ok(window) = window.single() else {
        return;
    };
    let transform = fit_transform(bounds.0, window.width(), window.height());
    for mut scene_transform in &mut scenes {
        *scene_transform = transform;
    }
}

/// Maps between window pixels and document coordinates for the current fit.
#[derive(Clone, Copy)]
struct ViewMapping {
    scale: f32,
    tx: f32,
    ty: f32,
    half_width: f32,
    half_height: f32,
}

impl ViewMapping {
    fn new(bounds: Bounds, width: f32, height: f32) -> Self {
        let transform = fit_transform(bounds, width, height);
        Self {
            scale: transform.scale.x,
            tx: transform.translation.x,
            ty: transform.translation.y,
            half_width: width * 0.5,
            half_height: height * 0.5,
        }
    }

    fn screen_to_document(self, screen: Vec2) -> [f32; 2] {
        [
            (screen.x - self.half_width - self.tx) / self.scale,
            (screen.y - self.half_height + self.ty) / self.scale,
        ]
    }

    /// Length of one screen pixel in document units.
    fn document_per_pixel(self) -> f32 {
        1.0 / self.scale
    }
}

fn handle_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    mut editor: ResMut<EditorState>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        editor.selected = None;
        editor.drag = None;
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    let view = ViewMapping::new(bounds.0, window.width(), window.height());
    let point = view.screen_to_document(cursor);

    if mouse.just_pressed(MouseButton::Left) {
        // Control points of the current selection take priority over picking a
        // different path, otherwise clicking a handle next to another shape
        // would re-select that shape.
        let mut started_drag = false;
        if let Some(address) = editor.selected.clone()
            && let Some(path) = path_at(&editor.document, &address)
            && let Some(handle) = find_handle(path, point, view.document_per_pixel() * 8.0)
        {
            editor.drag = Some(Drag { address, handle });
            started_drag = true;
        }

        if !started_drag {
            editor.selected = pick_path(&editor.document, point, view.document_per_pixel() * 3.0);
            editor.drag = None;
        }
    }

    if let Some(drag) = editor.drag.clone() {
        if mouse.pressed(MouseButton::Left)
            && let Some(path) = path_at_mut(&mut editor.document, &drag.address)
        {
            set_handle(path, drag.handle, point);
        }
        if mouse.just_released(MouseButton::Left) {
            editor.drag = None;
        }
    }
}

fn refresh_scene(
    editor: Res<EditorState>,
    bounds: Res<DocumentBounds>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut scenes: Query<&mut VelloScene2d, With<DocumentScene>>,
) {
    let scale = windows
        .single()
        .map(|window| {
            fit_transform(bounds.0, window.width(), window.height())
                .scale
                .x
        })
        .unwrap_or(1.0);

    let mut scene = build_scene(&editor.document);
    if let Some(address) = &editor.selected
        && let Some(path) = path_at(&editor.document, address)
    {
        draw_control_overlay(&mut scene, path, scale);
    }

    if let Ok(mut scene_component) = scenes.single_mut() {
        *scene_component = VelloScene2d::from(scene);
    }
}

// ---------------------------------------------------------------------------
// Tree addressing
// ---------------------------------------------------------------------------

/// Resolves an address to the path it names.
fn path_at<'a>(document: &'a Document, address: &PathAddress) -> Option<&'a Path> {
    match address {
        PathAddress::Item { layer, items } => {
            path_item_at(document, *layer, items).map(|item| &item.path)
        }
        PathAddress::Clip { layer, items } => {
            clip_group_at(document, *layer, items).map(|group| &group.clip)
        }
    }
}

fn path_at_mut<'a>(document: &'a mut Document, address: &PathAddress) -> Option<&'a mut Path> {
    match address {
        PathAddress::Item { layer, items } => {
            path_item_at_mut(document, *layer, items).map(|item| &mut item.path)
        }
        PathAddress::Clip { layer, items } => {
            clip_group_at_mut(document, *layer, items).map(|group| &mut group.clip)
        }
    }
}

/// Resolves a chain of item indices to the list of items it selects.
///
/// Every index except the last must address a clip group, since clip groups are
/// the only items that contain further items.
fn items_at<'a>(mut items: &'a [Item], indices: &[usize]) -> Option<&'a [Item]> {
    for &index in indices {
        match items.get(index)? {
            Item::Clip(group) => items = &group.targets,
            Item::Path(_) => return None,
        }
    }
    Some(items)
}

fn items_at_mut<'a>(mut items: &'a mut [Item], indices: &[usize]) -> Option<&'a mut [Item]> {
    for &index in indices {
        match items.get_mut(index)? {
            Item::Clip(group) => items = &mut group.targets,
            Item::Path(_) => return None,
        }
    }
    Some(items)
}

fn path_item_at<'a>(
    document: &'a Document,
    layer: usize,
    indices: &[usize],
) -> Option<&'a PathItem> {
    path_item_in(&document.layers.get(layer)?.items, indices)
}

fn path_item_in<'a>(items: &'a [Item], indices: &[usize]) -> Option<&'a PathItem> {
    let (last, parents) = indices.split_last()?;
    match items_at(items, parents)?.get(*last)? {
        Item::Path(path_item) => Some(path_item),
        Item::Clip(_) => None,
    }
}

fn path_item_at_mut<'a>(
    document: &'a mut Document,
    layer: usize,
    indices: &[usize],
) -> Option<&'a mut PathItem> {
    let layer = document.layers.get_mut(layer)?;
    path_item_in_mut(&mut layer.items, indices)
}

fn path_item_in_mut<'a>(items: &'a mut [Item], indices: &[usize]) -> Option<&'a mut PathItem> {
    let (last, parents) = indices.split_last()?;
    match items_at_mut(items, parents)?.get_mut(*last)? {
        Item::Path(path_item) => Some(path_item),
        Item::Clip(_) => None,
    }
}

fn clip_group_at<'a>(
    document: &'a Document,
    layer: usize,
    indices: &[usize],
) -> Option<&'a ClipGroup> {
    clip_group_in(&document.layers.get(layer)?.items, indices)
}

fn clip_group_in<'a>(items: &'a [Item], indices: &[usize]) -> Option<&'a ClipGroup> {
    let (last, parents) = indices.split_last()?;
    match items_at(items, parents)?.get(*last)? {
        Item::Clip(group) => Some(group),
        Item::Path(_) => None,
    }
}

fn clip_group_at_mut<'a>(
    document: &'a mut Document,
    layer: usize,
    indices: &[usize],
) -> Option<&'a mut ClipGroup> {
    let layer = document.layers.get_mut(layer)?;
    clip_group_in_mut(&mut layer.items, indices)
}

fn clip_group_in_mut<'a>(items: &'a mut [Item], indices: &[usize]) -> Option<&'a mut ClipGroup> {
    let (last, parents) = indices.split_last()?;
    match items_at_mut(items, parents)?.get_mut(*last)? {
        Item::Clip(group) => Some(group),
        Item::Path(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

/// Returns the topmost paintable path under `point`.
fn pick_path(document: &Document, point: [f32; 2], slack: f32) -> Option<PathAddress> {
    // Later indices are painted later, so descend through layers by Z and
    // reverse document order within each layer.
    let mut layer_indices: Vec<usize> = (0..document.layers.len()).rev().collect();
    layer_indices.sort_by(|&a, &b| {
        document.layers[b]
            .z
            .partial_cmp(&document.layers[a].z)
            .unwrap_or(Ordering::Equal)
    });

    for layer_index in layer_indices {
        let layer = &document.layers[layer_index];
        if let Some(address) = pick_items(&layer.items, point, slack, layer_index) {
            return Some(address);
        }
    }
    None
}

/// Returns the topmost path under `point`, including clip paths themselves.
///
/// The order mirrors painting: a clip's stroke is over its targets, and its
/// fill is underneath them.
fn pick_items(items: &[Item], point: [f32; 2], slack: f32, layer: usize) -> Option<PathAddress> {
    for (index, item) in items.iter().enumerate().rev() {
        match item {
            Item::Path(path_item) => {
                if path_hit(&path_item.path, &path_item.style, point, slack) {
                    return Some(PathAddress::Item {
                        layer,
                        items: vec![index],
                    });
                }
            }
            Item::Clip(group) => {
                let clip_address = || PathAddress::Clip {
                    layer,
                    items: vec![index],
                };

                // The clip's stroke is painted over its targets.
                if let Some(stroke) = group.style.stroke {
                    let tolerance = stroke.width * 0.5 + slack;
                    if path_distance(&group.clip, point) <= tolerance {
                        return Some(clip_address());
                    }
                }

                // A clip group's targets are only visible inside the clip.
                if path_contains(&group.clip, point)
                    && let Some(mut target) = pick_items(&group.targets, point, slack, layer)
                {
                    target.push_item(index);
                    return Some(target);
                }

                // The clip's fill is painted underneath its targets.
                if group.style.fill.is_some() && path_contains(&group.clip, point) {
                    return Some(clip_address());
                }
            }
        }
    }
    None
}

fn path_hit(path: &Path, style: &Style, point: [f32; 2], slack: f32) -> bool {
    if style.fill.is_some() && path_contains(path, point) {
        return true;
    }
    if let Some(stroke) = style.stroke {
        let tolerance = stroke.width * 0.5 + slack;
        if path_distance(path, point) <= tolerance {
            return true;
        }
    }
    false
}

fn path_contains(path: &Path, point: [f32; 2]) -> bool {
    let mut bez = path_to_bez_path(path);
    if !path.closed {
        // Fills implicitly close open subpaths.
        bez.close_path();
    }
    bez.contains(to_kurbo_point(point))
}

fn path_distance(path: &Path, point: [f32; 2]) -> f32 {
    let point = to_kurbo_point(point);
    path_to_bez_path(path)
        .segments()
        .map(|segment| segment.nearest(point, 0.01).distance_sq)
        .fold(f64::INFINITY, f64::min)
        .sqrt() as f32
}

// ---------------------------------------------------------------------------
// Control points
// ---------------------------------------------------------------------------

/// Tolerance for treating the end of the last segment as the start anchor.
const JOIN_EPSILON: f32 = 1e-3;

/// Index of the last segment when it ends exactly at the start of a closed
/// path.
///
/// Such a segment closes the loop with no gap, so its endpoint is the same
/// anchor as [`Path::start`].
fn closing_segment(path: &Path) -> Option<usize> {
    if !path.closed {
        return None;
    }
    let index = path.segments.len().checked_sub(1)?;
    let last = path.segments.last()?;
    (distance(segment_to(last), path.start) <= JOIN_EPSILON).then_some(index)
}

/// All draggable points of a path, including cubic control handles.
fn handles(path: &Path) -> Vec<(Handle, [f32; 2])> {
    let mut handles = vec![(Handle::Start, path.start)];
    let closing = closing_segment(path);
    for (index, segment) in path.segments.iter().enumerate() {
        // The closing segment's endpoint is the start anchor and is already
        // listed above; do not offer a second handle on top of it.
        let duplicate = closing == Some(index);
        match segment {
            Segment::Line { to } => {
                if !duplicate {
                    handles.push((Handle::LineTo(index), *to));
                }
            }
            Segment::Cubic { ctrl1, ctrl2, to } => {
                handles.push((Handle::CubicCtrl1(index), *ctrl1));
                handles.push((Handle::CubicCtrl2(index), *ctrl2));
                if !duplicate {
                    handles.push((Handle::CubicTo(index), *to));
                }
            }
        }
    }
    handles
}

fn find_handle(path: &Path, point: [f32; 2], radius: f32) -> Option<Handle> {
    handles(path)
        .into_iter()
        .filter_map(|(handle, position)| {
            let distance = distance(position, point);
            (distance <= radius).then_some((distance, handle))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, handle)| handle)
}

fn set_handle(path: &mut Path, handle: Handle, point: [f32; 2]) {
    match handle {
        Handle::Start => {
            let delta = offset(path.start, point);
            let closing = closing_segment(path);
            path.start = point;

            // The control handles attached to the anchor follow it.
            if let Some(Segment::Cubic { ctrl1, .. }) = path.segments.first_mut() {
                *ctrl1 = translate(*ctrl1, delta);
            }
            if let Some(index) = closing
                && let Some(segment) = path.segments.get_mut(index)
            {
                match segment {
                    Segment::Line { to } => *to = point,
                    Segment::Cubic { ctrl2, to, .. } => {
                        *ctrl2 = translate(*ctrl2, delta);
                        *to = point;
                    }
                }
            }
        }
        Handle::LineTo(index) | Handle::CubicTo(index) => {
            let Some(segment) = path.segments.get(index) else {
                return;
            };
            let delta = offset(segment_to(segment), point);
            let closing = closing_segment(path);

            match &mut path.segments[index] {
                Segment::Line { to } => *to = point,
                Segment::Cubic { to, .. } => *to = point,
            }
            // Incoming control of this segment and outgoing control of the
            // following one travel with the anchor.
            if let Segment::Cubic { ctrl2, .. } = &mut path.segments[index] {
                *ctrl2 = translate(*ctrl2, delta);
            }
            if let Some(Segment::Cubic { ctrl1, .. }) = path.segments.get_mut(index + 1) {
                *ctrl1 = translate(*ctrl1, delta);
            }

            // If this endpoint is the start anchor of a closed loop, move the
            // start point and the loop's outgoing control with it.
            if closing == Some(index) {
                path.start = point;
                if let Some(Segment::Cubic { ctrl1, .. }) = path.segments.first_mut() {
                    *ctrl1 = translate(*ctrl1, delta);
                }
            }
        }
        Handle::CubicCtrl1(index) => {
            if let Some(Segment::Cubic { ctrl1, .. }) = path.segments.get_mut(index) {
                *ctrl1 = point;
            }
        }
        Handle::CubicCtrl2(index) => {
            if let Some(Segment::Cubic { ctrl2, .. }) = path.segments.get_mut(index) {
                *ctrl2 = point;
            }
        }
    }
}

fn segment_to(segment: &Segment) -> [f32; 2] {
    match segment {
        Segment::Line { to } => *to,
        Segment::Cubic { to, .. } => *to,
    }
}

fn offset(from: [f32; 2], to: [f32; 2]) -> [f32; 2] {
    [to[0] - from[0], to[1] - from[1]]
}

fn translate(point: [f32; 2], delta: [f32; 2]) -> [f32; 2] {
    [point[0] + delta[0], point[1] + delta[1]]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn to_kurbo_point(point: [f32; 2]) -> KurboPoint {
    KurboPoint::new(f64::from(point[0]), f64::from(point[1]))
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

/// Draws the selected path's outline, bezier arms, and draggable points.
///
/// Sizes are given in screen pixels and converted to document units so the
/// overlay stays legible at any zoom.
fn draw_control_overlay(scene: &mut Scene, path: &Path, scale: f32) {
    let scale = scale.max(f32::EPSILON);
    let pixels = |amount: f32| f64::from(amount / scale);

    let accent = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff);
    let handle_fill = Color::from_rgba8(0xff, 0xff, 0xff, 0xff);
    let handle_outline = Color::from_rgba8(0x0b, 0x2a, 0x40, 0xff);

    scene.stroke(
        &KurboStroke::new(pixels(2.0)),
        Affine::IDENTITY,
        &accent,
        None,
        &path_to_bez_path(path),
    );

    // Arms from cubic control points to the anchors they influence.
    let mut previous = path.start;
    for segment in &path.segments {
        match segment {
            Segment::Line { to } => previous = *to,
            Segment::Cubic { ctrl1, ctrl2, to } => {
                stroke_segment(scene, previous, *ctrl1, pixels(1.0), &accent);
                stroke_segment(scene, *ctrl2, *to, pixels(1.0), &accent);
                previous = *to;
            }
        }
    }

    let radius = pixels(4.0);
    for (_, position) in handles(path) {
        let circle = Circle::new(to_kurbo_point(position), radius);
        scene.fill(Fill::NonZero, Affine::IDENTITY, &handle_fill, None, &circle);
        scene.stroke(
            &KurboStroke::new(pixels(1.5)),
            Affine::IDENTITY,
            &handle_outline,
            None,
            &circle,
        );
    }
}

fn stroke_segment(scene: &mut Scene, from: [f32; 2], to: [f32; 2], width: f64, color: &Color) {
    scene.stroke(
        &KurboStroke::new(width),
        Affine::IDENTITY,
        color,
        None,
        &KurboLine::new(to_kurbo_point(from), to_kurbo_point(to)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipGroup, Layer, Rgba8, Style};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Path {
        Path::new([x0, y0])
            .line_to([x1, y0])
            .line_to([x1, y1])
            .line_to([x0, y1])
            .closed()
    }

    fn filled(color: Rgba8) -> Style {
        Style {
            fill: Some(color),
            stroke: None,
        }
    }

    #[test]
    fn picks_topmost_layer() {
        let document = Document {
            version: 1,
            layers: vec![
                Layer {
                    z: 0.0,
                    items: vec![Item::Path(PathItem {
                        path: rect(0.0, 0.0, 100.0, 100.0),
                        style: filled(Rgba8::rgb(255, 0, 0)),
                    })],
                },
                Layer {
                    z: 10.0,
                    items: vec![Item::Path(PathItem {
                        path: rect(50.0, 50.0, 150.0, 150.0),
                        style: filled(Rgba8::rgb(0, 0, 255)),
                    })],
                },
            ],
        };

        assert_eq!(
            pick_path(&document, [75.0, 75.0], 0.0),
            Some(PathAddress::Item {
                layer: 1,
                items: vec![0]
            })
        );
        assert_eq!(
            pick_path(&document, [10.0, 10.0], 0.0),
            Some(PathAddress::Item {
                layer: 0,
                items: vec![0]
            })
        );
        assert_eq!(pick_path(&document, [200.0, 200.0], 0.0), None);
    }

    #[test]
    fn clip_group_limits_hits() {
        let document = Document {
            version: 1,
            layers: vec![Layer {
                z: 0.0,
                items: vec![Item::Clip(ClipGroup {
                    clip: rect(0.0, 0.0, 100.0, 100.0),
                    style: Style::default(),
                    targets: vec![Item::Path(PathItem {
                        path: rect(0.0, 0.0, 200.0, 200.0),
                        style: filled(Rgba8::rgb(0, 255, 0)),
                    })],
                })],
            }],
        };

        assert_eq!(
            pick_path(&document, [50.0, 50.0], 0.0),
            Some(PathAddress::Item {
                layer: 0,
                items: vec![0, 0]
            })
        );
        // Inside the target rectangle but clipped away.
        assert_eq!(pick_path(&document, [150.0, 150.0], 0.0), None);
    }

    #[test]
    fn clip_stroke_is_picked_over_targets() {
        let document = Document {
            version: 1,
            layers: vec![Layer {
                z: 0.0,
                items: vec![Item::Clip(ClipGroup {
                    clip: rect(0.0, 0.0, 100.0, 100.0),
                    style: Style {
                        fill: None,
                        stroke: Some(crate::model::StrokeStyle {
                            color: Rgba8::rgb(0, 0, 0),
                            width: 4.0,
                        }),
                    },
                    targets: vec![Item::Path(PathItem {
                        path: rect(0.0, 0.0, 100.0, 100.0),
                        style: filled(Rgba8::rgb(255, 0, 0)),
                    })],
                })],
            }],
        };

        // Away from the outline the target is picked.
        assert_eq!(
            pick_path(&document, [50.0, 50.0], 0.0),
            Some(PathAddress::Item {
                layer: 0,
                items: vec![0, 0]
            })
        );
        // The clip's stroke is painted over the target, so it wins there.
        assert_eq!(
            pick_path(&document, [1.0, 50.0], 0.0),
            Some(PathAddress::Clip {
                layer: 0,
                items: vec![0]
            })
        );
    }

    #[test]
    fn clip_fill_is_picked_under_targets() {
        let document = Document {
            version: 1,
            layers: vec![Layer {
                z: 0.0,
                items: vec![Item::Clip(ClipGroup {
                    clip: rect(0.0, 0.0, 100.0, 100.0),
                    style: filled(Rgba8::rgb(0, 0, 255)),
                    targets: vec![Item::Path(PathItem {
                        path: rect(20.0, 20.0, 80.0, 80.0),
                        style: filled(Rgba8::rgb(255, 0, 0)),
                    })],
                })],
            }],
        };

        // The target is painted over the clip's fill.
        assert_eq!(
            pick_path(&document, [50.0, 50.0], 0.0),
            Some(PathAddress::Item {
                layer: 0,
                items: vec![0, 0]
            })
        );
        // Where the target does not cover, the clip's fill is picked.
        assert_eq!(
            pick_path(&document, [10.0, 50.0], 0.0),
            Some(PathAddress::Clip {
                layer: 0,
                items: vec![0]
            })
        );
    }

    #[test]
    fn nested_clip_paths_can_be_addressed_and_edited() {
        let mut document = Document {
            version: 1,
            layers: vec![Layer {
                z: 0.0,
                items: vec![Item::Clip(ClipGroup {
                    clip: rect(0.0, 0.0, 100.0, 100.0),
                    style: filled(Rgba8::rgb(0, 0, 255)),
                    targets: vec![Item::Clip(ClipGroup {
                        clip: rect(10.0, 10.0, 90.0, 90.0),
                        style: filled(Rgba8::rgb(0, 255, 0)),
                        targets: Vec::new(),
                    })],
                })],
            }],
        };

        let address = PathAddress::Clip {
            layer: 0,
            items: vec![0, 0],
        };
        assert!(path_at(&document, &address).is_some());

        *path_at_mut(&mut document, &address).unwrap() = rect(20.0, 20.0, 80.0, 80.0);

        let Item::Clip(outer) = &document.layers[0].items[0] else {
            panic!("expected an outer clip group");
        };
        let Item::Clip(inner) = &outer.targets[0] else {
            panic!("expected a nested clip group");
        };
        assert_eq!(inner.clip, rect(20.0, 20.0, 80.0, 80.0));
    }

    #[test]
    fn example_red_area_selects_the_clip_path() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/example.vecart.json");
        let document = Document::load_from_file(path).unwrap();

        // The clip's fill is the only thing covering this part of the circle.
        assert_eq!(
            pick_path(&document, [330.0, 210.0], 0.0),
            Some(PathAddress::Clip {
                layer: 2,
                items: vec![0]
            })
        );
    }

    #[test]
    fn strokes_are_hit_by_distance() {
        let document = Document {
            version: 1,
            layers: vec![Layer {
                z: 0.0,
                items: vec![Item::Path(PathItem {
                    path: Path::new([0.0, 50.0]).line_to([100.0, 50.0]),
                    style: Style {
                        fill: None,
                        stroke: Some(crate::model::StrokeStyle {
                            color: Rgba8::rgb(0, 0, 0),
                            width: 4.0,
                        }),
                    },
                })],
            }],
        };

        assert!(pick_path(&document, [50.0, 52.0], 0.0).is_some());
        assert!(pick_path(&document, [50.0, 60.0], 0.0).is_none());
    }

    #[test]
    fn screen_mapping_matches_fit() {
        let bounds = Bounds {
            min: [0.0, 0.0],
            max: [400.0, 200.0],
        };
        let view = ViewMapping::new(bounds, 900.0, 900.0);

        let center = view.screen_to_document(Vec2::new(450.0, 450.0));
        assert!((center[0] - 200.0).abs() < 1e-4);
        assert!((center[1] - 100.0).abs() < 1e-4);

        let top_left = view.screen_to_document(Vec2::new(
            0.9 * 900.0 * 0.0 + view.tx + 450.0,
            0.9 * 900.0 * 0.0 - view.ty + 450.0,
        ));
        assert!(top_left[0].abs() < 1e-4);
        assert!(top_left[1].abs() < 1e-4);
    }

    #[test]
    fn set_handle_updates_the_right_point() {
        let mut path = Path::new([0.0, 0.0]).cubic_to([10.0, 0.0], [20.0, 0.0], [30.0, 0.0]);
        set_handle(&mut path, Handle::CubicCtrl2(0), [12.0, 34.0]);
        assert_eq!(
            path.segments[0],
            Segment::Cubic {
                ctrl1: [10.0, 0.0],
                ctrl2: [12.0, 34.0],
                to: [30.0, 0.0]
            }
        );
    }

    #[test]
    fn closed_join_has_a_single_handle() {
        let path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 0.0], [30.0, 0.0])
            .cubic_to([20.0, 30.0], [10.0, 30.0], [0.0, 0.0])
            .closed();

        let at_join = handles(&path)
            .into_iter()
            .filter(|(_, position)| distance(*position, path.start) <= JOIN_EPSILON)
            .count();
        assert_eq!(at_join, 1);
    }

    #[test]
    fn dragging_an_anchor_moves_its_controls() {
        let mut path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 10.0], [30.0, 0.0])
            .cubic_to([40.0, 10.0], [50.0, 0.0], [60.0, 0.0]);

        set_handle(&mut path, Handle::CubicTo(0), [40.0, 0.0]);

        // The anchor's incoming and outgoing controls both moved by the
        // anchor's delta, while the far ends of the segments stayed put.
        assert_eq!(
            path.segments[0],
            Segment::Cubic {
                ctrl1: [10.0, 0.0],
                ctrl2: [30.0, 10.0],
                to: [40.0, 0.0]
            }
        );
        assert_eq!(
            path.segments[1],
            Segment::Cubic {
                ctrl1: [50.0, 10.0],
                ctrl2: [50.0, 0.0],
                to: [60.0, 0.0]
            }
        );
    }

    #[test]
    fn dragging_the_closed_join_keeps_the_loop_shut() {
        let mut path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 0.0], [30.0, 0.0])
            .cubic_to([20.0, 30.0], [10.0, 30.0], [0.0, 0.0])
            .closed();

        set_handle(&mut path, Handle::Start, [5.0, 5.0]);

        assert_eq!(path.start, [5.0, 5.0]);
        assert_eq!(
            path.segments[0],
            Segment::Cubic {
                ctrl1: [15.0, 5.0],
                ctrl2: [20.0, 0.0],
                to: [30.0, 0.0]
            }
        );
        assert_eq!(
            path.segments[1],
            Segment::Cubic {
                ctrl1: [20.0, 30.0],
                ctrl2: [15.0, 35.0],
                to: [5.0, 5.0]
            }
        );
    }
}
