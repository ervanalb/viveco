//! Converts a [`Document`] into a Vello [`Scene`].

use bevy_vello::vello::{
    Scene,
    kurbo::{Affine, BezPath, Point as KurboPoint, Stroke as KurboStroke},
    peniko::{Color, Fill},
};

use crate::model::{ClipGroup, Document, Item, Path, PathItem, Rgba8, Segment, Style};

/// Builds a Vello scene in document coordinates.
///
/// Layers are painted in ascending Z order. Within a path the fill is painted
/// before the stroke. For a clip group the clip's fill is painted first, then
/// the targets inside a clip layer, then the clip's stroke on top, so nested
/// clip groups intersect naturally while their outlines stay visible.
pub fn build_scene(document: &Document) -> Scene {
    let mut scene = Scene::new();
    for layer in document.sorted_layers() {
        for item in &layer.items {
            draw_item(&mut scene, item);
        }
    }
    scene
}

fn draw_item(scene: &mut Scene, item: &Item) {
    match item {
        Item::Path(path_item) => draw_path(scene, path_item),
        Item::Clip(group) => draw_clip_group(scene, group),
    }
}

fn draw_path(scene: &mut Scene, path_item: &PathItem) {
    let path = path_to_bez_path(&path_item.path);
    draw_fill(scene, &path_item.style, &path);
    draw_stroke(scene, &path_item.style, &path);
}

fn draw_clip_group(scene: &mut Scene, group: &ClipGroup) {
    let clip = path_to_bez_path(&group.clip);

    // The clip's fill sits underneath its targets.
    draw_fill(scene, &group.style, &clip);

    scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
    for target in &group.targets {
        draw_item(scene, target);
    }
    scene.pop_layer();

    // The clip's stroke sits on top of its targets, and is not clipped by its
    // own outline, so the whole stroke width stays visible.
    draw_stroke(scene, &group.style, &clip);
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

/// Converts a model path into a kurbo path (closing it when `path.closed`).
pub fn path_to_bez_path(path: &Path) -> BezPath {
    let mut bez = BezPath::new();
    bez.move_to(to_kurbo_point(path.start));
    for segment in &path.segments {
        match segment {
            Segment::Line { to } => bez.line_to(to_kurbo_point(*to)),
            Segment::Cubic { ctrl1, ctrl2, to } => bez.curve_to(
                to_kurbo_point(*ctrl1),
                to_kurbo_point(*ctrl2),
                to_kurbo_point(*to),
            ),
        }
    }
    if path.closed {
        bez.close_path();
    }
    bez
}

fn to_kurbo_point(point: [f32; 2]) -> KurboPoint {
    KurboPoint::new(f64::from(point[0]), f64::from(point[1]))
}

fn to_color(color: Rgba8) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}
