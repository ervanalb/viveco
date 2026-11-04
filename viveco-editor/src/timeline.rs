//! Editable timeline view for animate mode.
//!
//! The bottom animate panel hosts a [`TimelineArea`] rendered with a
//! [`UiVelloScene`]. The view spans time horizontally and animated value
//! vertically, with independent pan and zoom on each axis:
//!
//! * wheel scrolls the value axis, shift-wheel scrolls time, ctrl-wheel zooms
//!   around the cursor;
//! * middle-drag pans (space + left-drag does the same), right-drag zooms
//!   around the cursor.
//!
//! Keyframes are drawn as points, their handles as smaller points a third of
//! the way toward the neighbouring keyframe, and the cubic curve they describe
//! is stroked between them. Cyclic animations repeat the whole pattern with a
//! period equal to the duration, and only the repetitions intersecting the
//! visible window are emitted.
//!
//! Pressing a keyframe snaps the playhead to it. Keyframes drag in time or in
//! value depending on which axis the pointer favours; a value drag carries the
//! keyframe's handles along with it. Explicit handles drag vertically, since
//! their time is fixed a third of the way to the neighbour. Dragging empty
//! space draws a marquee over keyframes, and a multiple selection drags
//! together. Double-clicking a keyframe in a multiple selection aligns the
//! whole selection to its time, merging each curve down to one keyframe there.
//!
//! Clicking an explicit handle selects it. `Delete` removes selected keyframes
//! or handles. Shift-dragging a keyframe creates a handle on the side the
//! pointer moves toward, and shift-dragging an existing handle places the
//! opposite handle on the same tangent, scaling for the different time spans
//! of the two neighbouring segments, so the curve stays smooth through it.
//!
//! Selecting a bone focuses the view on its curves: curves for other bones are
//! dimmed, and the value axis refits to the selected bone's value range while
//! the time axis keeps its current pan and zoom.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::Color as BevyColor;
use bevy::prelude::*;
use bevy::ui::RelativeCursorPosition;
use bevy_vello::prelude::UiVelloScene;
use bevy_vello::vello::{
    Scene,
    kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke},
    peniko::{Color, Fill},
};

use crate::animate::{curve_groups, effective_handle_in, effective_handle_out, selected_animation};
use crate::animate_panel::{DurationEdit, SpeedEdit};
use crate::editor::{EditorMode, EditorState};
use crate::rig::bone_name_at;
use crate::theme;
use viveco::model::{Animation, Keyframe, KeyframeTarget};

/// Amount one wheel notch multiplies the zoom by.
const ZOOM_STEP: f32 = 1.1;

/// Tightest time zoom: seconds represented by one screen pixel.
const MIN_TIME_PER_PIXEL: f32 = 1e-4;

/// Loosest time zoom: seconds represented by one screen pixel.
const MAX_TIME_PER_PIXEL: f32 = 50.0;

/// Tightest value zoom: value units represented by one screen pixel.
const MIN_VALUE_PER_PIXEL: f32 = 1e-4;

/// Loosest value zoom: value units represented by one screen pixel.
const MAX_VALUE_PER_PIXEL: f32 = 1e4;

/// Smallest distance between integer-timestep gridlines, in pixels. When
/// integer seconds would be closer than this the grid steps by 2, 5, 10, ...
const GRID_MIN_SPACING: f32 = 10.0;

/// Cap on how many repeated cycles are emitted at once, so zooming far out of
/// a short cyclic animation cannot stall the frame.
const MAX_VISIBLE_CYCLES: i64 = 256;

/// Inset of the track inside the timeline node, in logical pixels. The top
/// edge is flush; the controls band's bottom padding supplies the gap above.
const TRACK_INSET: f64 = 12.0;

const KEYFRAME_RADIUS: f64 = 4.0;
const HANDLE_RADIUS: f64 = 2.75;
const CURVE_WIDTH: f64 = 1.75;
const HANDLE_ARM_WIDTH: f64 = 1.0;

const GRID_LINE: Color = Color::from_rgba8(0xff, 0xff, 0xff, 0x22);
const KEYFRAME_OUTLINE: Color = Color::from_rgba8(0x0d, 0x10, 0x16, 0xff);
const HANDLE_ARM: Color = Color::from_rgba8(0xff, 0xff, 0xff, 0x50);
const HANDLE_FILL: Color = Color::from_rgba8(0xdd, 0xe1, 0xe8, 0xff);

/// Dark scrim over the parts of the track outside the animation's time range.
const DISABLED_SCRIM: Color = Color::from_rgba8(0x0d, 0x10, 0x16, 0xa0);

/// Ring drawn around selected keyframes.
const SELECTED_RING: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xcc);
const MARQUEE_FILL: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0x24);
const MARQUEE_STROKE: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xb0);

/// Colors for curves that belong to bones other than the selected one, when
/// the timeline is focused on a bone.
const DIMMED_CURVE: Color = Color::from_rgba8(0x6b, 0x73, 0x7d, 0x80);
const DIMMED_HANDLE_ARM: Color = Color::from_rgba8(0xff, 0xff, 0xff, 0x20);
const DIMMED_HANDLE_FILL: Color = Color::from_rgba8(0x8b, 0x93, 0xa1, 0x88);

const PLAYHEAD_COLOR: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff);
const PLAYHEAD_OUTLINE: Color = Color::from_rgba8(0x0b, 0x2a, 0x40, 0xff);
const PLAYHEAD_WIDTH: f64 = 1.5;
const PLAYHEAD_HANDLE_HALF_WIDTH: f64 = 6.0;
const PLAYHEAD_HANDLE_HEIGHT: f64 = 14.0;

/// How close to the playhead handle a press must be to grab it, in pixels.
const PLAYHEAD_GRAB_HALF_WIDTH: f32 = 10.0;
const PLAYHEAD_GRAB_HEIGHT: f32 = 18.0;

/// How close to a keyframe or handle a press must be to grab it, in pixels.
const KEYFRAME_HIT_RADIUS: f32 = 7.0;
const HANDLE_HIT_RADIUS: f32 = 6.0;

/// How far the pointer must travel before a keyframe drag commits to an axis.
const DRAG_AXIS_THRESHOLD: f32 = 3.0;

/// How quickly a second press on the same keyframe must follow the first, in
/// seconds, to count as a double-click that aligns the selection.
const DOUBLE_CLICK_SECONDS: f64 = 0.35;

/// How far the pointer may move between the two presses of a double-click, in
/// pixels.
const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

/// Number of label nodes pre-spawned over the timeline.
///
/// Labels are thinned to at least [`LABEL_MIN_SPACING`] pixels apart, so this
/// comfortably covers even very wide windows; extra nodes stay hidden.
const GRID_LABEL_POOL: usize = 96;

const GRID_LABEL_FONT_SIZE: f32 = 10.0;

/// Smallest distance between labelled gridlines, in pixels.
const LABEL_MIN_SPACING: f32 = 48.0;

/// Color of a time label outside the animation's time range.
const LABEL_DISABLED: BevyColor = BevyColor::srgba_u8(0x8b, 0x93, 0xa1, 0x59);

/// Converts a Bevy palette color into a peniko brush color.
fn brush(color: bevy::prelude::Color) -> Color {
    let color = color.to_srgba();
    Color::from_rgba8(
        (color.red * 255.0).round() as u8,
        (color.green * 255.0).round() as u8,
        (color.blue * 255.0).round() as u8,
        (color.alpha * 255.0).round() as u8,
    )
}

pub struct TimelinePlugin;

impl Plugin for TimelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimelineView>()
            .init_resource::<TimelineEdit>()
            .add_systems(
                Update,
                (
                    fit_timeline,
                    handle_timeline_input.in_set(TimelineInput),
                    draw_timeline,
                    sync_grid_labels,
                )
                    .chain(),
            );
    }
}

/// System set for the timeline's input handling.
///
/// The animation list also reacts to `Delete`; it runs before this set so a
/// timeline selection claims the key before the list can delete an animation.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimelineInput;

/// Marks the UI node the timeline draws into.
#[derive(Component)]
pub struct TimelineArea;

/// Marks one of the pooled time-label nodes.
#[derive(Component)]
struct GridLabel;

/// Which axis a keyframe drag has committed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragAxis {
    /// Horizontal: keyframes move in time.
    Time,
    /// Vertical: keyframes move in value.
    Value,
}

/// One side of a keyframe's Bézier control points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleSide {
    In,
    Out,
}

/// A keyframe drag.
///
/// The animation's keyframes are copied when the drag begins and rebuilt from
/// that copy every frame. Rebuilding rather than mutating in place lets dragged
/// keyframes cross their neighbours and wrap around a cyclic animation without
/// the indices going stale.
struct KeyframeDrag {
    /// The animation's keyframes when the drag began.
    baseline: Vec<Keyframe>,
    /// Indices into `baseline` that the drag moves.
    indices: Vec<usize>,
    /// The keyframe the drag was pressed on, whose time the playhead follows.
    anchor: usize,
    /// Pointer position, in track-local pixels, when the drag began.
    start: Vec2,
    /// The axis chosen after the pointer first moved far enough.
    axis: Option<DragAxis>,
}

/// A control-point drag. Handles store only a value, so they move vertically.
struct HandleDrag {
    index: usize,
    side: HandleSide,
    /// Pointer y, in track-local pixels, when the drag began.
    start_y: f32,
    /// Handle value when the drag began.
    origin_value: f32,
    /// Whether the opposite handle mirrors this one, keeping the curve smooth.
    symmetric: bool,
}

/// A shift-drag that begins on a keyframe and creates or moves a handle on the
/// side the pointer moves toward.
struct KeyframeHandleDrag {
    index: usize,
    /// Pointer position, in track-local pixels, when the drag began.
    start: Vec2,
    /// The side chosen after the pointer first moved far enough, if any.
    side: Option<HandleSide>,
    /// The keyframe's value, used when creating a handle and when mirroring.
    keyframe_value: f32,
    /// Explicit handle values at the start of the drag, so moving an existing
    /// handle does not jump.
    origin_in: Option<f32>,
    origin_out: Option<f32>,
}

/// A box selection over the timeline.
struct MarqueeDrag {
    start: Vec2,
    current: Vec2,
    /// Selection that existed before the marquee, kept while Shift is held.
    baseline: Vec<usize>,
}

/// The gesture currently in progress.
enum Drag {
    Playhead,
    /// The last pointer position and the button that started the pan.
    Pan {
        last: Vec2,
        button: MouseButton,
    },
    Keyframes(KeyframeDrag),
    Handle(HandleDrag),
    KeyframeHandle(KeyframeHandleDrag),
    Marquee(MarqueeDrag),
}

/// Persistent selection and in-progress gesture for the timeline.
#[derive(Resource, Default)]
pub(crate) struct TimelineEdit {
    /// The animation the selection belongs to.
    animation: Option<usize>,
    /// Selected keyframe indices into the animation's keyframe list.
    selected: Vec<usize>,
    /// Selected explicit handles, as `(keyframe index, side)`.
    selected_handles: Vec<(usize, HandleSide)>,
    /// The gesture being applied, if any.
    drag: Option<Drag>,
    /// Right-button drag anchor for zooming, which is a view gesture rather
    /// than a document edit.
    right_drag: Option<Vec2>,
    /// The time, keyframe, and pointer position of the last press on a
    /// keyframe, used to recognise a double-click that aligns the selection.
    last_keyframe_press: Option<(f64, usize, Vec2)>,
}

impl TimelineEdit {
    /// Whether any keyframe or handle is selected, so `Delete` belongs to the
    /// timeline rather than to the animation list.
    pub(crate) fn has_selection(&self) -> bool {
        !self.selected.is_empty() || !self.selected_handles.is_empty()
    }
}

/// Spawns the timeline node inside the animate panel.
pub fn spawn_timeline(parent: &mut ChildSpawnerCommands) {
    // Deliberately no `BackgroundColor`: the vello scene paints the track
    // itself, because UI backgrounds are composited above UiVelloScene. The
    // label pool is normal UI text, so it sits above the scene.
    parent
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                overflow: Overflow::clip(),
                ..default()
            },
            RelativeCursorPosition::default(),
            UiVelloScene::default(),
            TimelineArea,
        ))
        .with_children(|area| {
            for _ in 0..GRID_LABEL_POOL {
                area.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        display: Display::None,
                        ..default()
                    },
                    Text::new(""),
                    TextFont::from_font_size(GRID_LABEL_FONT_SIZE),
                    TextColor(theme::MUTED),
                    GridLabel,
                ));
            }
        });
}

/// Pan and zoom state for the timeline, in node-local logical pixels.
///
/// `time0` is the time at the left edge and `value0` the value at the top
/// edge. Value grows upward, so it decreases as the local y grows.
#[derive(Resource)]
struct TimelineView {
    time0: f32,
    time_per_pixel: f32,
    value0: f32,
    value_per_pixel: f32,
    /// What the last framing of the time axis was computed for.
    fitted: Option<FitKey>,
    /// What the last fit of the value axis was computed for.
    fitted_values: Option<ValueFitKey>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            time0: 0.0,
            time_per_pixel: 0.01,
            value0: 1.0,
            value_per_pixel: 0.01,
            fitted: None,
            fitted_values: None,
        }
    }
}

impl TimelineView {
    fn time_at(&self, x: f32) -> f32 {
        self.time0 + x * self.time_per_pixel
    }

    fn value_at(&self, y: f32) -> f32 {
        self.value0 - y * self.value_per_pixel
    }

    fn screen_x(&self, time: f64) -> f64 {
        (time - f64::from(self.time0)) / f64::from(self.time_per_pixel)
    }

    fn screen_y(&self, value: f64) -> f64 {
        (f64::from(self.value0) - value) / f64::from(self.value_per_pixel)
    }

    fn point(&self, time: f64, value: f64) -> Point {
        Point::new(TRACK_INSET + self.screen_x(time), self.screen_y(value))
    }

    fn visible_time(&self, size: Vec2) -> (f32, f32) {
        (self.time_at(0.0), self.time_at(size.x.max(0.0)))
    }

    /// Frames the animation's duration and values with a small margin,
    /// optionally limiting the value range to one bone's curves.
    fn fit(&mut self, animation: &Animation, size: Vec2, bone: Option<&str>) {
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }

        let duration = if animation.duration.is_finite() {
            animation.duration.max(0.0)
        } else {
            0.0
        };
        // A zero-length animation still gets a visible second so keyframes at
        // t = 0 do not collapse onto the left edge.
        let (span_start, span_end) = if duration > 0.0 {
            (0.0, duration)
        } else {
            (0.0, 1.0)
        };
        let time_pad = (span_end - span_start) * 0.04;
        self.time0 = span_start - time_pad;
        self.time_per_pixel = ((span_end - span_start + 2.0 * time_pad) / size.x)
            .clamp(MIN_TIME_PER_PIXEL, MAX_TIME_PER_PIXEL);

        self.fit_value(animation, size, bone);
    }

    /// Frames only the value axis, leaving the time axis untouched.
    fn fit_value(&mut self, animation: &Animation, size: Vec2, bone: Option<&str>) {
        if size.y <= 0.0 {
            return;
        }
        let (mut low, mut high) = value_bounds(animation, bone);
        if (high - low).abs() < f32::EPSILON {
            low -= 0.5;
            high += 0.5;
        }
        let value_pad = (high - low) * 0.12;
        low -= value_pad;
        high += value_pad;
        self.value0 = high;
        self.value_per_pixel =
            ((high - low) / size.y).clamp(MIN_VALUE_PER_PIXEL, MAX_VALUE_PER_PIXEL);
    }

    /// Scales both axes by `factor`, keeping the data under `anchor` (in
    /// node-local pixels) fixed.
    fn zoom_at(&mut self, anchor: Vec2, factor: f32) {
        self.zoom_axes_at(anchor, factor, factor);
    }

    /// Scales each axis independently, keeping the data under `anchor` (in
    /// node-local pixels) fixed. A factor above one zooms that axis in.
    fn zoom_axes_at(&mut self, anchor: Vec2, time_factor: f32, value_factor: f32) {
        let time = self.time_at(anchor.x);
        let value = self.value_at(anchor.y);
        if time_factor.is_finite() && time_factor > 0.0 {
            self.time_per_pixel =
                (self.time_per_pixel / time_factor).clamp(MIN_TIME_PER_PIXEL, MAX_TIME_PER_PIXEL);
            self.time0 = time - anchor.x * self.time_per_pixel;
        }
        if value_factor.is_finite() && value_factor > 0.0 {
            self.value_per_pixel = (self.value_per_pixel / value_factor)
                .clamp(MIN_VALUE_PER_PIXEL, MAX_VALUE_PER_PIXEL);
            self.value0 = value + anchor.y * self.value_per_pixel;
        }
    }

    /// Moves the content with the cursor.
    fn pan(&mut self, delta: Vec2) {
        self.time0 -= delta.x * self.time_per_pixel;
        self.value0 += delta.y * self.value_per_pixel;
    }
}

/// What the current time-axis framing was computed for. Re-fitting only when
/// this changes keeps manual panning and zooming intact.
#[derive(Clone, Copy, PartialEq)]
struct FitKey {
    selected: Option<usize>,
    duration: f32,
    cyclic: bool,
    width: f32,
    height: f32,
    scale: f32,
}

/// What the current value-axis fit was computed for.
///
/// The value axis depends on the focused bone as well as the curves, so it is
/// tracked separately: selecting a bone refits the values without touching the
/// time axis.
#[derive(Clone, PartialEq)]
struct ValueFitKey {
    /// The focused bone, if any.
    bone: Option<String>,
    /// Number of keyframes whose values the fit covers.
    keyframes: usize,
}

/// The name of the selected bone, when it still resolves to a named bone.
///
/// The timeline focuses on this bone's curves, dimming the rest.
fn selected_bone_name(editor: &EditorState) -> Option<&str> {
    editor
        .selected_bone
        .as_ref()
        .and_then(|address| bone_name_at(&editor.document, address))
        .filter(|name| !name.is_empty())
}

/// The bone a keyframe drives, if it drives a bone at all.
fn keyframe_bone(keyframe: &Keyframe) -> Option<&str> {
    match &keyframe.target {
        KeyframeTarget::Bone { bone, .. } => Some(bone),
        KeyframeTarget::Shape { .. } => None,
    }
}

/// Whether a grouped curve drives `bone`.
fn group_is_bone(group: &[&Keyframe], bone: &str) -> bool {
    group
        .first()
        .is_some_and(|keyframe| keyframe_bone(keyframe) == Some(bone))
}

/// Logical size of a UI node. `ComputedNode` sizes are in physical pixels;
/// Vello UI scenes are laid out in logical pixels.
fn logical_size(node: &ComputedNode) -> Vec2 {
    node.size() * node.inverse_scale_factor
}

/// The drawable area inside the timeline node, excluding the track inset.
fn content_size(size: Vec2) -> Vec2 {
    Vec2::new(
        (size.x - 2.0 * TRACK_INSET as f32).max(1.0),
        (size.y - TRACK_INSET as f32).max(1.0),
    )
}

/// The rounded track rectangle inside the timeline node.
fn track_rect(size: Vec2) -> Rect {
    Rect::new(
        TRACK_INSET,
        0.0,
        (f64::from(size.x) - TRACK_INSET).max(TRACK_INSET),
        (f64::from(size.y) - TRACK_INSET).max(0.0),
    )
}

fn fit_timeline(
    editor: Res<EditorState>,
    mut view: ResMut<TimelineView>,
    area: Query<&ComputedNode, With<TimelineArea>>,
) {
    let Ok(node) = area.single() else {
        return;
    };
    let size = logical_size(node);
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let area = content_size(size);
    let Some(animation) = selected_animation(&editor) else {
        view.fitted = None;
        view.fitted_values = None;
        return;
    };
    let bone = selected_bone_name(&editor).map(str::to_owned);
    let key = FitKey {
        selected: editor.selected_animation,
        duration: animation.duration,
        cyclic: animation.cyclic,
        width: area.x,
        height: area.y,
        scale: node.inverse_scale_factor,
    };
    let values = ValueFitKey {
        keyframes: match bone.as_deref() {
            Some(bone) => animation
                .keyframes
                .iter()
                .filter(|keyframe| keyframe_bone(keyframe) == Some(bone))
                .count(),
            None => animation.keyframes.len(),
        },
        bone: bone.clone(),
    };

    // Re-framing both axes when the animation or view changes; otherwise only
    // the values are refitted, so selecting a bone never rescales time.
    if view.fitted.as_ref() != Some(&key) {
        view.fit(animation, area, bone.as_deref());
        view.fitted = Some(key);
        view.fitted_values = Some(values);
    } else if view.fitted_values.as_ref() != Some(&values) {
        view.fit_value(animation, area, bone.as_deref());
        view.fitted_values = Some(values);
    }
}

/// Discards queued wheel events.
fn drain_wheel(wheel: &mut MessageReader<MouseWheel>) {
    for _ in wheel.read() {}
}

/// Picks the cardinal axis a drag has moved furthest along, or `None` until
/// the pointer has travelled far enough to commit.
fn dominant_axis(delta: Vec2, threshold: f32) -> Option<DragAxis> {
    if delta.length() < threshold {
        return None;
    }
    if delta.x.abs() >= delta.y.abs() {
        Some(DragAxis::Time)
    } else {
        Some(DragAxis::Value)
    }
}

/// The selected animation's keyframes, mutably.
fn selected_animation_mut(editor: &mut EditorState) -> Option<&mut Animation> {
    let index = editor.selected_animation?;
    editor.document.animations.get_mut(index)
}

/// Removes the selected handles and keyframes from the selected animation.
///
/// Selected handles are cleared first, then selected keyframes are removed in
/// descending index order so earlier removals do not shift later ones.
fn delete_selection(editor: &mut EditorState, edit: &mut TimelineEdit) {
    let handles = std::mem::take(&mut edit.selected_handles);
    let mut indices = std::mem::take(&mut edit.selected);
    edit.drag = None;
    edit.last_keyframe_press = None;

    let Some(animation) = selected_animation_mut(editor) else {
        return;
    };

    for (index, side) in handles {
        if let Some(keyframe) = animation.keyframes.get_mut(index) {
            match side {
                HandleSide::In => keyframe.handle_in = None,
                HandleSide::Out => keyframe.handle_out = None,
            }
        }
    }

    indices.sort_unstable();
    indices.dedup();
    for index in indices.into_iter().rev() {
        if index < animation.keyframes.len() {
            animation.keyframes.remove(index);
        }
    }
}

/// Moves a keyframe and its handles by `dv`, keeping the handles attached to
/// the curve at the same relative offset.
fn move_keyframe_value(
    keyframe: &mut Keyframe,
    value: f32,
    handle_in: Option<f32>,
    handle_out: Option<f32>,
    dv: f32,
) {
    keyframe.value = value + dv;
    keyframe.handle_in = handle_in.map(|handle| handle + dv);
    keyframe.handle_out = handle_out.map(|handle| handle + dv);
}

/// Moves every selected keyframe to `target` time, merging keyframes that land
/// on the same curve so each curve keeps only one keyframe there.
///
/// The anchor keyframe survives on its own curve. On every other curve, the
/// selected keyframe closest to its original time to the target survives, with
/// ties going to the lower index. Returns the surviving keyframes' indices in
/// their new document order.
fn align_selection(
    animation: &mut Animation,
    selected: &[usize],
    target: f32,
    anchor: usize,
) -> Vec<usize> {
    let mut order: Vec<usize> = selected.to_vec();
    order.sort_unstable();
    order.dedup();
    order.retain(|&index| index < animation.keyframes.len());

    // Choose one survivor per curve among the selection.
    let mut survivors: Vec<usize> = Vec::new();
    for &index in &order {
        let curve = animation.keyframes[index].target.clone();
        if survivors
            .iter()
            .any(|&survivor| animation.keyframes[survivor].target == curve)
        {
            continue;
        }
        let group = order
            .iter()
            .copied()
            .filter(|&other| animation.keyframes[other].target == curve);
        let survivor = if group.clone().any(|other| other == anchor) {
            anchor
        } else {
            group
                .min_by(|&a, &b| {
                    let da = (animation.keyframes[a].timestamp - target).abs();
                    let db = (animation.keyframes[b].timestamp - target).abs();
                    da.partial_cmp(&db)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.cmp(&b))
                })
                .unwrap_or(index)
        };
        survivors.push(survivor);
    }

    // Remove the merged-away keyframes, then re-time the survivors.
    let removed: Vec<usize> = order
        .iter()
        .copied()
        .filter(|index| !survivors.contains(index))
        .collect();
    let mut survivor_indices: Vec<usize> = survivors
        .iter()
        .map(|&original| original - removed.iter().filter(|&&gone| gone < original).count())
        .collect();
    for &index in removed.iter().rev() {
        animation.keyframes.remove(index);
    }
    for &index in &survivor_indices {
        if let Some(keyframe) = animation.keyframes.get_mut(index) {
            keyframe.timestamp = target;
        }
    }

    // Re-sort by timestamp, tracking where each survivor ends up. Sorting is
    // stable, so keyframes sharing the target time keep their relative order.
    let mut tagged: Vec<(usize, Keyframe)> = animation.keyframes.drain(..).enumerate().collect();
    tagged.sort_by(|a, b| {
        a.1.timestamp
            .partial_cmp(&b.1.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    survivor_indices = tagged
        .iter()
        .enumerate()
        .filter_map(|(position, (original, _))| {
            survivor_indices.contains(original).then_some(position)
        })
        .collect();
    animation.keyframes = tagged.into_iter().map(|(_, keyframe)| keyframe).collect();
    survivor_indices
}

/// The time spans of the same-curve segments on either side of a keyframe.
///
/// Returns `None` when the keyframe lacks a neighbour on either side, such as
/// a non-cyclic endpoint, since there is no opposing segment to match.
fn adjacent_spans(animation: &Animation, index: usize) -> Option<(f32, f32)> {
    let time = f64::from(animation.keyframes.get(index)?.timestamp);
    let previous = previous_same_target(animation, index, 0.0)?;
    let next = next_same_target(animation, index, 0.0)?;
    let incoming = (time - previous) as f32;
    let outgoing = (next - time) as f32;
    (incoming > 0.0 && outgoing > 0.0).then_some((incoming, outgoing))
}

/// The value of the handle opposite `value` that keeps the curve's slope
/// continuous through the keyframe.
///
/// A handle sits a third of the way toward its neighbour, so the slope at the
/// keyframe is the handle's value offset divided by the segment's span in
/// time. With equally spaced neighbours this is a plain reflection; unequal
/// spans scale the offset by their ratio. `span` is the dragged handle's
/// segment length and `opposite_span` the segment on the other side.
fn mirror_handle(keyframe_value: f32, value: f32, span: f32, opposite_span: f32) -> f32 {
    keyframe_value + (keyframe_value - value) * (opposite_span / span)
}

/// The handle side a horizontal drag from a keyframe creates, once the pointer
/// has moved far enough to commit.
fn handle_side_toward(dx: f32) -> Option<HandleSide> {
    if dx.abs() < DRAG_AXIS_THRESHOLD {
        None
    } else if dx > 0.0 {
        Some(HandleSide::Out)
    } else {
        Some(HandleSide::In)
    }
}

/// Whether a press on `index` at `local` repeats a recent keyframe press
/// closely enough in time and place to count as a double-click.
fn repeats_press(last: Option<(f64, usize, Vec2)>, now: f64, index: usize, local: Vec2) -> bool {
    last.is_some_and(|(last_time, last_index, last_position)| {
        last_index == index
            && now - last_time <= DOUBLE_CLICK_SECONDS
            && last_position.distance(local) <= DOUBLE_CLICK_DISTANCE
    })
}

/// Where a keyframe dragged by `delta` seconds lands.
///
/// A cyclic animation wraps around either end, so the stored time always stays
/// in `0..duration` and the curve repeats seamlessly. Other animations clamp
/// to `0..=duration`, since there is nothing to wrap onto.
fn dragged_time(origin: f32, delta: f32, duration: f32, cyclic: bool) -> f32 {
    let duration = if duration.is_finite() {
        duration.max(0.0)
    } else {
        0.0
    };
    let time = origin + delta;
    if cyclic && duration > 0.0 {
        time.rem_euclid(duration)
    } else {
        time.clamp(0.0, duration)
    }
}

/// The largest time shift that keeps every dragged keyframe inside the
/// animation's `0..=duration` range.
///
/// Only non-cyclic drags need this: a cyclic drag wraps instead of stopping at
/// the edge.
fn clamp_drag_delta(baseline: &[Keyframe], selected: &[usize], delta: f32, duration: f32) -> f32 {
    let mut min_dt = f32::NEG_INFINITY;
    let mut max_dt = f32::INFINITY;
    for &index in selected {
        let Some(keyframe) = baseline.get(index) else {
            continue;
        };
        min_dt = min_dt.max(-keyframe.timestamp);
        max_dt = max_dt.min(duration - keyframe.timestamp);
    }
    if min_dt.is_finite() && max_dt.is_finite() && min_dt <= max_dt {
        delta.clamp(min_dt, max_dt)
    } else {
        delta
    }
}

/// Rebuilds the keyframe list for a drag, returning the list and the dragged
/// keyframes' positions in it.
///
/// Rebuilding from the drag's starting keyframes lets dragged points cross
/// their neighbours in time. Two keyframes of the same curve cannot share a
/// timestamp, so a dragged keyframe that lands exactly on another is omitted;
/// if the drag ends there, the omission becomes the deletion.
fn drag_keyframes(
    baseline: &[Keyframe],
    selected: &[usize],
    axis: DragAxis,
    delta: f32,
    duration: f32,
    cyclic: bool,
) -> (Vec<Keyframe>, Vec<usize>) {
    let dragged = |index: usize| selected.contains(&index);

    let mut moved: Vec<(usize, Keyframe)> = baseline.iter().cloned().enumerate().collect();
    for (index, keyframe) in &mut moved {
        if !dragged(*index) {
            continue;
        }
        match axis {
            DragAxis::Time => {
                keyframe.timestamp = dragged_time(keyframe.timestamp, delta, duration, cyclic);
            }
            DragAxis::Value => {
                let (value, handle_in, handle_out) =
                    (keyframe.value, keyframe.handle_in, keyframe.handle_out);
                move_keyframe_value(keyframe, value, handle_in, handle_out, delta);
            }
        }
    }

    // A stationary keyframe always wins a collision. Between two dragged
    // keyframes the one earlier in document order wins, so the outcome does not
    // depend on iteration order.
    let mut kept: Vec<(usize, Keyframe)> = Vec::with_capacity(moved.len());
    for (position, (index, keyframe)) in moved.iter().enumerate() {
        let collides = dragged(*index)
            && moved
                .iter()
                .enumerate()
                .any(|(other, (other_index, candidate))| {
                    other != position
                        && candidate.target == keyframe.target
                        && candidate.timestamp == keyframe.timestamp
                        && (!dragged(*other_index) || other < position)
                });
        if !collides {
            kept.push((*index, keyframe.clone()));
        }
    }

    kept.sort_by(|a, b| {
        a.1.timestamp
            .partial_cmp(&b.1.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let positions = kept
        .iter()
        .enumerate()
        .filter_map(|(position, (index, _))| dragged(*index).then_some(position))
        .collect();
    (
        kept.into_iter().map(|(_, keyframe)| keyframe).collect(),
        positions,
    )
}

/// A keyframe's position in track-local pixels.
fn keyframe_position(view: &TimelineView, keyframe: &Keyframe) -> Vec2 {
    Vec2::new(
        view.screen_x(f64::from(keyframe.timestamp)) as f32,
        view.screen_y(f64::from(keyframe.value)) as f32,
    )
}

/// The time offsets of the repeated cycles intersect the visible window.
fn cycle_offsets(animation: &Animation, view: &TimelineView, size: Vec2) -> Vec<f64> {
    if animation.cyclic && animation.duration > 0.0 {
        let duration = f64::from(animation.duration);
        visible_cycles(view, size, animation.duration)
            .map(|cycle| cycle as f64 * duration)
            .collect()
    } else {
        vec![0.0]
    }
}

/// The keyframe under `local`, if any. Repeated cycles resolve to the base
/// keyframe.
fn keyframe_under(
    animation: &Animation,
    view: &TimelineView,
    size: Vec2,
    local: Vec2,
) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for offset in cycle_offsets(animation, view, size) {
        for (index, keyframe) in animation.keyframes.iter().enumerate() {
            let position = Vec2::new(
                view.screen_x(f64::from(keyframe.timestamp) + offset) as f32,
                view.screen_y(f64::from(keyframe.value)) as f32,
            );
            let distance = position.distance(local);
            if distance <= KEYFRAME_HIT_RADIUS && best.is_none_or(|(best, _)| distance < best) {
                best = Some((distance, index));
            }
        }
    }
    best.map(|(_, index)| index)
}

/// The first keyframe of the same curve after `index`, in the given cycle.
///
/// Wraps to the same curve's first keyframe one period later for cyclic
/// animations, including a single-keyframe curve wrapping onto itself.
fn next_same_target(animation: &Animation, index: usize, offset: f64) -> Option<f64> {
    let target = &animation.keyframes.get(index)?.target;
    let later = animation.keyframes[index + 1..]
        .iter()
        .find(|keyframe| &keyframe.target == target);
    if let Some(next) = later {
        return Some(f64::from(next.timestamp) + offset);
    }
    if animation.cyclic && animation.duration > 0.0 {
        let first = animation.keyframes[..=index]
            .iter()
            .find(|keyframe| &keyframe.target == target)?;
        return Some(f64::from(first.timestamp) + f64::from(animation.duration) + offset);
    }
    None
}

/// The last keyframe of the same curve before `index`, in the given cycle.
///
/// Wraps to the same curve's last keyframe one period earlier for cyclic
/// animations, including a single-keyframe curve wrapping onto itself.
fn previous_same_target(animation: &Animation, index: usize, offset: f64) -> Option<f64> {
    let target = &animation.keyframes.get(index)?.target;
    let earlier = animation.keyframes[..index]
        .iter()
        .rev()
        .find(|keyframe| &keyframe.target == target);
    if let Some(previous) = earlier {
        return Some(f64::from(previous.timestamp) + offset);
    }
    if animation.cyclic && animation.duration > 0.0 {
        let last = animation.keyframes[index..]
            .iter()
            .rev()
            .find(|keyframe| &keyframe.target == target)?;
        return Some(f64::from(last.timestamp) - f64::from(animation.duration) + offset);
    }
    None
}

/// Where an explicit handle is drawn, in track-local pixels.
fn handle_position(
    animation: &Animation,
    view: &TimelineView,
    index: usize,
    side: HandleSide,
    offset: f64,
) -> Option<Vec2> {
    let keyframe = animation.keyframes.get(index)?;
    let value = match side {
        HandleSide::In => keyframe.handle_in?,
        HandleSide::Out => keyframe.handle_out?,
    };
    let neighbour = match side {
        HandleSide::In => previous_same_target(animation, index, offset)?,
        HandleSide::Out => next_same_target(animation, index, offset)?,
    };
    let time = f64::from(keyframe.timestamp) + offset;
    let span = (neighbour - time).abs();
    let handle_time = match side {
        HandleSide::In => time - span / 3.0,
        HandleSide::Out => time + span / 3.0,
    };
    Some(Vec2::new(
        view.screen_x(handle_time) as f32,
        view.screen_y(f64::from(value)) as f32,
    ))
}

/// The explicit handle under `local`, if any. Repetitions resolve to the base
/// handle.
fn handle_under(
    animation: &Animation,
    view: &TimelineView,
    size: Vec2,
    local: Vec2,
) -> Option<(usize, HandleSide)> {
    let mut best: Option<(f32, usize, HandleSide)> = None;
    for offset in cycle_offsets(animation, view, size) {
        for index in 0..animation.keyframes.len() {
            for side in [HandleSide::In, HandleSide::Out] {
                let Some(position) = handle_position(animation, view, index, side, offset) else {
                    continue;
                };
                let distance = position.distance(local);
                if distance <= HANDLE_HIT_RADIUS && best.is_none_or(|(best, _, _)| distance < best)
                {
                    best = Some((distance, index, side));
                }
            }
        }
    }
    best.map(|(_, index, side)| (index, side))
}

/// Applies wheel, pan, zoom, playhead, keyframe, handle, and marquee gestures
/// over the timeline.
#[allow(clippy::too_many_arguments)]
fn handle_timeline_input(
    mode: Res<EditorMode>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    duration_edit: Res<DurationEdit>,
    speed_edit: Res<SpeedEdit>,
    time: Res<Time>,
    mut wheel: MessageReader<MouseWheel>,
    area: Query<(&RelativeCursorPosition, &ComputedNode), With<TimelineArea>>,
    mut editor: ResMut<EditorState>,
    mut view: ResMut<TimelineView>,
    mut edit: ResMut<TimelineEdit>,
) {
    if *mode != EditorMode::Animate {
        drain_wheel(&mut wheel);
        edit.selected.clear();
        edit.selected_handles.clear();
        edit.drag = None;
        edit.right_drag = None;
        edit.last_keyframe_press = None;
        return;
    }

    // The selection belongs to one animation; changing animations drops it.
    // This runs before the cursor checks so a stale selection cannot linger
    // once its animation is gone.
    if edit.animation != editor.selected_animation {
        edit.animation = editor.selected_animation;
        edit.selected.clear();
        edit.selected_handles.clear();
        edit.drag = None;
        edit.last_keyframe_press = None;
    }
    if let Some(animation) = selected_animation(&editor) {
        let len = animation.keyframes.len();
        edit.selected.retain(|&index| index < len);
        edit.selected_handles.retain(|(index, _)| *index < len);
    } else {
        edit.selected.clear();
        edit.selected_handles.clear();
    }

    // `Delete` removes the timeline selection even when the pointer is no
    // longer over the track. The animation list checks `has_selection` and
    // runs first, so it does not delete the whole animation at the same time.
    if keys.just_pressed(KeyCode::Delete)
        && !duration_edit.is_editing()
        && !speed_edit.is_editing()
        && !editor.is_renaming()
    {
        delete_selection(&mut editor, &mut edit);
    }

    let Some(duration) = selected_animation(&editor).map(|animation| animation.duration) else {
        drain_wheel(&mut wheel);
        edit.drag = None;
        edit.right_drag = None;
        return;
    };
    let duration = duration.max(0.0);
    let Ok((relative, node)) = area.single() else {
        drain_wheel(&mut wheel);
        edit.drag = None;
        edit.right_drag = None;
        return;
    };
    let size = logical_size(node);
    if size.x <= 0.0 || size.y <= 0.0 {
        drain_wheel(&mut wheel);
        edit.drag = None;
        edit.right_drag = None;
        return;
    }
    let Some(normalized) = relative.normalized else {
        drain_wheel(&mut wheel);
        edit.drag = None;
        edit.right_drag = None;
        return;
    };
    // `normalized` runs from (-0.5, -0.5) at the top-left to (0.5, 0.5) at the
    // bottom-right. Shift the node-local position into track coordinates.
    let local = (normalized + Vec2::splat(0.5)) * size - Vec2::new(TRACK_INSET as f32, 0.0);
    let over = relative.cursor_over();

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let space = keys.pressed(KeyCode::Space);

    for event in wheel.read() {
        if !over {
            continue;
        }
        let conversion = MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR;
        let (dx, dy) = match event.unit {
            MouseScrollUnit::Line => (event.x * conversion, event.y * conversion),
            MouseScrollUnit::Pixel => (event.x, event.y),
        };

        if ctrl {
            view.zoom_at(local, ZOOM_STEP.powf(dy / conversion));
        } else if shift {
            let horizontal = if dx != 0.0 { dx } else { dy };
            view.time0 -= horizontal * view.time_per_pixel;
        } else {
            view.value0 += dy * view.value_per_pixel;
        }
    }

    let left_press = mouse.just_pressed(MouseButton::Left) && over;
    let middle_press = mouse.just_pressed(MouseButton::Middle) && over;

    if middle_press {
        edit.drag = Some(Drag::Pan {
            last: local,
            button: MouseButton::Middle,
        });
    } else if left_press && space {
        edit.drag = Some(Drag::Pan {
            last: local,
            button: MouseButton::Left,
        });
    } else if left_press {
        // Work out what the press grabbed before mutating the document.
        enum Press {
            Playhead,
            Keyframe(usize),
            Handle(usize, HandleSide),
            Marquee,
        }
        let press = {
            let Some(animation) = selected_animation(&editor) else {
                return;
            };
            let playhead = clamp_playhead(editor.playhead, duration);
            let playhead_x = view.screen_x(f64::from(playhead)) as f32;
            let on_playhead = local.y <= PLAYHEAD_GRAB_HEIGHT
                && (local.x - playhead_x).abs() <= PLAYHEAD_GRAB_HALF_WIDTH;
            if on_playhead {
                Press::Playhead
            } else if let Some(index) = keyframe_under(animation, &view, content_size(size), local)
            {
                Press::Keyframe(index)
            } else if let Some((index, side)) =
                handle_under(animation, &view, content_size(size), local)
            {
                Press::Handle(index, side)
            } else {
                Press::Marquee
            }
        };

        match press {
            Press::Playhead => {
                edit.last_keyframe_press = None;
                edit.drag = Some(Drag::Playhead);
                editor.playhead = clamp_playhead(view.time_at(local.x), duration);
            }
            Press::Keyframe(index) => {
                let timestamp = selected_animation(&editor)
                    .and_then(|animation| animation.keyframes.get(index))
                    .map_or(0.0, |keyframe| keyframe.timestamp);
                editor.playhead = clamp_playhead(timestamp, duration);

                let now = time.elapsed_secs_f64();
                let double_click = !shift
                    && edit.selected.len() > 1
                    && edit.selected.contains(&index)
                    && repeats_press(edit.last_keyframe_press, now, index, local);

                if double_click {
                    // Align the selection to this keyframe, merging each curve
                    // down to a single keyframe at that time.
                    edit.last_keyframe_press = None;
                    edit.selected_handles.clear();
                    edit.drag = None;
                    if let Some(animation) = selected_animation_mut(&mut editor) {
                        let selected = edit.selected.clone();
                        edit.selected = align_selection(animation, &selected, timestamp, index);
                    }
                } else if shift {
                    // Shift-dragging creates a handle; a shift-click that
                    // never moves instead toggles the selection on release.
                    edit.last_keyframe_press = None;
                    if let Some(animation) = selected_animation(&editor)
                        && let Some(keyframe) = animation.keyframes.get(index)
                    {
                        edit.drag = Some(Drag::KeyframeHandle(KeyframeHandleDrag {
                            index,
                            start: local,
                            side: None,
                            keyframe_value: keyframe.value,
                            origin_in: keyframe.handle_in,
                            origin_out: keyframe.handle_out,
                        }));
                    }
                } else {
                    edit.last_keyframe_press = Some((now, index, local));
                    if !edit.selected.contains(&index) {
                        edit.selected.clear();
                        edit.selected.push(index);
                    }
                    edit.selected_handles.clear();

                    let indices = edit.selected.clone();
                    if let Some(animation) = selected_animation(&editor) {
                        edit.drag = Some(Drag::Keyframes(KeyframeDrag {
                            baseline: animation.keyframes.clone(),
                            indices,
                            anchor: index,
                            start: local,
                            axis: None,
                        }));
                    }
                }
            }
            Press::Handle(index, side) => {
                // Selecting a handle replaces any keyframe selection, so the
                // two kinds of selection do not compete for `Delete`.
                edit.last_keyframe_press = None;
                edit.selected.clear();
                edit.selected_handles.clear();
                edit.selected_handles.push((index, side));

                if let Some(animation) = selected_animation(&editor)
                    && let Some(keyframe) = animation.keyframes.get(index)
                {
                    let origin_value = match side {
                        HandleSide::In => keyframe.handle_in,
                        HandleSide::Out => keyframe.handle_out,
                    }
                    .unwrap_or(keyframe.value);
                    edit.drag = Some(Drag::Handle(HandleDrag {
                        index,
                        side,
                        start_y: local.y,
                        origin_value,
                        symmetric: shift,
                    }));
                }
            }
            Press::Marquee => {
                edit.last_keyframe_press = None;
                edit.selected_handles.clear();
                let baseline = if shift {
                    edit.selected.clone()
                } else {
                    edit.selected.clear();
                    Vec::new()
                };
                edit.drag = Some(Drag::Marquee(MarqueeDrag {
                    start: local,
                    current: local,
                    baseline,
                }));
            }
        }
    }

    // Advance the gesture that is now active.
    {
        let TimelineEdit {
            selected,
            selected_handles,
            drag,
            last_keyframe_press,
            ..
        } = &mut *edit;
        let left_held = mouse.pressed(MouseButton::Left);
        let mut clear_drag = false;
        // A drag that actually moves keyframes is not a double-click.
        let mut moved_keyframes = false;

        match drag {
            Some(Drag::Playhead) => {
                if left_held {
                    editor.playhead = clamp_playhead(view.time_at(local.x), duration);
                } else {
                    clear_drag = true;
                }
            }
            Some(Drag::Pan { last, button }) => {
                let held = match *button {
                    MouseButton::Middle => mouse.pressed(MouseButton::Middle),
                    _ => left_held,
                };
                if held {
                    view.pan(local - *last);
                    *last = local;
                } else {
                    clear_drag = true;
                }
            }
            Some(Drag::Keyframes(state)) => {
                if !left_held {
                    clear_drag = true;
                } else {
                    if state.axis.is_none() {
                        state.axis = dominant_axis(local - state.start, DRAG_AXIS_THRESHOLD);
                    }
                    if let Some(axis) = state.axis {
                        let cyclic =
                            selected_animation(&editor).is_some_and(|animation| animation.cyclic);
                        let delta = match axis {
                            DragAxis::Time => {
                                let delta = (local.x - state.start.x) * view.time_per_pixel;
                                if cyclic {
                                    delta
                                } else {
                                    clamp_drag_delta(
                                        &state.baseline,
                                        &state.indices,
                                        delta,
                                        duration,
                                    )
                                }
                            }
                            DragAxis::Value => (state.start.y - local.y) * view.value_per_pixel,
                        };
                        if let Some(animation) = selected_animation_mut(&mut editor) {
                            let (keyframes, positions) = drag_keyframes(
                                &state.baseline,
                                &state.indices,
                                axis,
                                delta,
                                duration,
                                cyclic,
                            );
                            animation.keyframes = keyframes;
                            *selected = positions;
                        }

                        // The playhead follows the grabbed point, so the pose
                        // under it is the one being edited.
                        if let Some(keyframe) = state.baseline.get(state.anchor) {
                            let time = match axis {
                                DragAxis::Time => {
                                    dragged_time(keyframe.timestamp, delta, duration, cyclic)
                                }
                                DragAxis::Value => keyframe.timestamp,
                            };
                            editor.playhead = clamp_playhead(time, duration);
                        }
                        moved_keyframes = true;
                    }
                }
            }
            Some(Drag::KeyframeHandle(state)) => {
                if !left_held {
                    // A shift-click that never moved toggles the selection.
                    if state.side.is_none() {
                        selected_handles.clear();
                        if let Some(position) =
                            selected.iter().position(|&other| other == state.index)
                        {
                            selected.remove(position);
                        } else {
                            selected.push(state.index);
                            selected.sort_unstable();
                        }
                    }
                    clear_drag = true;
                } else if state.side.is_none() {
                    // Commit to a side once the pointer has moved far enough,
                    // but only if that side has a neighbouring keyframe the
                    // handle could point at.
                    if let Some(side) = handle_side_toward(local.x - state.start.x) {
                        let available =
                            selected_animation(&editor).is_some_and(|animation| match side {
                                HandleSide::Out => {
                                    next_same_target(animation, state.index, 0.0).is_some()
                                }
                                HandleSide::In => {
                                    previous_same_target(animation, state.index, 0.0).is_some()
                                }
                            });
                        if available {
                            state.side = Some(side);
                        } else {
                            clear_drag = true;
                        }
                    }
                }
                if let Some(side) = state.side
                    && left_held
                {
                    let origin = match side {
                        HandleSide::In => state.origin_in.unwrap_or(state.keyframe_value),
                        HandleSide::Out => state.origin_out.unwrap_or(state.keyframe_value),
                    };
                    let value = origin + (state.start.y - local.y) * view.value_per_pixel;
                    if let Some(animation) = selected_animation_mut(&mut editor)
                        && let Some(keyframe) = animation.keyframes.get_mut(state.index)
                    {
                        match side {
                            HandleSide::In => keyframe.handle_in = Some(value),
                            HandleSide::Out => keyframe.handle_out = Some(value),
                        }
                    }
                }
            }
            Some(Drag::Handle(state)) => {
                if left_held {
                    let value =
                        state.origin_value + (state.start_y - local.y) * view.value_per_pixel;
                    // Read the neighbouring spans before borrowing the
                    // animation mutably. Without a segment on both sides there
                    // is no opposing slope, so fall back to a plain reflection.
                    let spans = if state.symmetric {
                        selected_animation(&editor)
                            .and_then(|animation| adjacent_spans(animation, state.index))
                    } else {
                        None
                    };
                    if let Some(animation) = selected_animation_mut(&mut editor)
                        && let Some(keyframe) = animation.keyframes.get_mut(state.index)
                    {
                        match state.side {
                            HandleSide::In => keyframe.handle_in = Some(value),
                            HandleSide::Out => keyframe.handle_out = Some(value),
                        }
                        if state.symmetric {
                            let mirrored = match spans {
                                Some((incoming, outgoing)) => match state.side {
                                    HandleSide::In => {
                                        mirror_handle(keyframe.value, value, incoming, outgoing)
                                    }
                                    HandleSide::Out => {
                                        mirror_handle(keyframe.value, value, outgoing, incoming)
                                    }
                                },
                                None => mirror_handle(keyframe.value, value, 1.0, 1.0),
                            };
                            match state.side {
                                HandleSide::In => keyframe.handle_out = Some(mirrored),
                                HandleSide::Out => keyframe.handle_in = Some(mirrored),
                            }
                        }
                    }
                } else {
                    clear_drag = true;
                }
            }
            Some(Drag::Marquee(state)) => {
                if left_held {
                    state.current = local;
                    let min = state.start.min(state.current);
                    let max = state.start.max(state.current);
                    let mut hits = state.baseline.clone();
                    if let Some(animation) = selected_animation(&editor) {
                        for (index, keyframe) in animation.keyframes.iter().enumerate() {
                            let position = keyframe_position(&view, keyframe);
                            if position.x >= min.x
                                && position.x <= max.x
                                && position.y >= min.y
                                && position.y <= max.y
                                && !hits.contains(&index)
                            {
                                hits.push(index);
                            }
                        }
                    }
                    hits.sort_unstable();
                    *selected = hits;
                } else {
                    clear_drag = true;
                }
            }
            None => {}
        }

        if moved_keyframes {
            *last_keyframe_press = None;
        }
        if clear_drag {
            *drag = None;
        }
    }

    if mouse.just_pressed(MouseButton::Right) && over {
        edit.right_drag = Some(local);
    }
    if let Some(last) = edit.right_drag {
        if mouse.pressed(MouseButton::Right) {
            // Horizontal dragging zooms the time axis and vertical dragging
            // the value axis: right or up zooms in, left or down zooms out.
            let delta = local - last;
            let time_factor = (delta.x * 0.005).exp();
            let value_factor = (-delta.y * 0.005).exp();
            view.zoom_axes_at(local, time_factor, value_factor);
            edit.right_drag = Some(local);
        } else {
            edit.right_drag = None;
        }
    }
}

fn draw_timeline(
    mode: Res<EditorMode>,
    editor: Res<EditorState>,
    view: Res<TimelineView>,
    edit: Res<TimelineEdit>,
    mut area: Query<(&ComputedNode, &mut UiVelloScene), With<TimelineArea>>,
) {
    let Ok((node, mut component)) = area.single_mut() else {
        return;
    };
    let size = logical_size(node);
    let mut scene = Scene::new();
    if *mode == EditorMode::Animate && size.x > 0.0 && size.y > 0.0 {
        draw_frame(
            &mut scene,
            selected_animation(&editor),
            &view,
            size,
            editor.playhead,
            &edit,
            selected_bone_name(&editor),
        );
    }
    *component = UiVelloScene::from(scene);
}

#[allow(clippy::too_many_arguments)]
fn draw_frame(
    scene: &mut Scene,
    animation: Option<&Animation>,
    view: &TimelineView,
    size: Vec2,
    playhead: f32,
    edit: &TimelineEdit,
    focus: Option<&str>,
) {
    // The panel and track backgrounds live in the scene because Bevy UI is
    // composited over Vello UI scenes.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        brush(theme::PANEL_BG),
        None,
        &Rect::new(0.0, 0.0, f64::from(size.x), f64::from(size.y)),
    );
    let track = track_rect(size);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        brush(theme::TRACK_BG),
        None,
        &track,
    );
    scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &track);

    if let Some(animation) = animation {
        let area = content_size(size);
        let duration = animation.duration.max(0.0);
        let selected: Vec<&Keyframe> = edit
            .selected
            .iter()
            .filter_map(|&index| animation.keyframes.get(index))
            .collect();
        let selected_handles: Vec<(&Keyframe, HandleSide)> = edit
            .selected_handles
            .iter()
            .filter_map(|&(index, side)| animation.keyframes.get(index).map(|key| (key, side)))
            .collect();
        draw_grid(scene, view, area);

        let groups = curve_groups(animation);
        for (index, group) in groups.iter().enumerate() {
            let dimmed = focus.is_some_and(|bone| !group_is_bone(group, bone));
            // Curves take their bone's color so a bone looks the same in every
            // animation, regardless of how its curves are ordered.
            let color = if dimmed {
                DIMMED_CURVE
            } else {
                group
                    .first()
                    .and_then(|keyframe| keyframe_bone(keyframe))
                    .map_or_else(|| theme::curve_color_at(index), theme::curve_color)
            };
            if animation.cyclic && duration > 0.0 {
                for cycle in visible_cycles(view, area, duration) {
                    draw_group(
                        scene,
                        group,
                        Some(duration),
                        cycle,
                        color,
                        view,
                        &selected,
                        &selected_handles,
                        dimmed,
                    );
                }
            } else {
                draw_group(
                    scene,
                    group,
                    None,
                    0,
                    color,
                    view,
                    &selected,
                    &selected_handles,
                    dimmed,
                );
            }
        }

        // Mute the time outside the animation, then mark the playhead on top.
        draw_disabled_spans(scene, view, size, duration);
        draw_playhead(scene, view, size, clamp_playhead(playhead, duration));
        if let Some(Drag::Marquee(marquee)) = &edit.drag {
            draw_marquee(scene, marquee);
        }
    }

    scene.pop_layer();
}

/// Vertical gridlines at the thinned integer timesteps.
fn draw_grid(scene: &mut Scene, view: &TimelineView, size: Vec2) {
    let height = f64::from(size.y);
    for tick in grid_ticks(view, size) {
        let line = Line::new(Point::new(tick.x, 0.0), Point::new(tick.x, height));
        scene.stroke(&Stroke::new(1.0), Affine::IDENTITY, GRID_LINE, None, &line);
    }
}

/// One visible gridline, with the label it carries (if any).
struct GridTick {
    /// Time at the gridline, in seconds.
    time: f64,
    /// Node-local x coordinate of the line.
    x: f64,
    /// Text shown under the line, thinned so labels do not overlap.
    label: Option<String>,
}

/// Gridlines, and their thinned labels, for the visible window.
fn grid_ticks(view: &TimelineView, size: Vec2) -> Vec<GridTick> {
    let (start, end) = view.visible_time(size);
    if !start.is_finite() || !end.is_finite() {
        return Vec::new();
    }
    let step = grid_step(view.time_per_pixel);
    let first = (f64::from(start) / f64::from(step)).ceil();
    let last = (f64::from(end) / f64::from(step)).floor();
    if !first.is_finite() || !last.is_finite() || last < first || last - first > 10_000.0 {
        return Vec::new();
    }

    let multiple = label_multiple(step, view.time_per_pixel);
    let left = TRACK_INSET;
    let right = TRACK_INSET + f64::from(size.x);
    let mut ticks = Vec::new();
    for index in first as i64..=last as i64 {
        let time = index as f64 * f64::from(step);
        let x = TRACK_INSET + view.screen_x(time);
        if x < left || x > right {
            continue;
        }
        let label = (index % multiple == 0).then(|| format_time(time));
        ticks.push(GridTick { time, x, label });
    }
    ticks
}

/// Number of grid steps between labels, so labels stay at least
/// [`LABEL_MIN_SPACING`] pixels apart.
fn label_multiple(step: f32, time_per_pixel: f32) -> i64 {
    if time_per_pixel.is_nan() || time_per_pixel <= 0.0 || !step.is_finite() {
        return 1;
    }
    let spacing = step / time_per_pixel;
    ((LABEL_MIN_SPACING / spacing).ceil() as i64).max(1)
}

/// Formats a gridline time as whole seconds.
fn format_time(time: f64) -> String {
    (time.round() as i64).to_string()
}

/// Positions the pooled label nodes on the labelled gridlines.
///
/// Labels are Bevy UI text rather than Vello glyphs: the vello text feature is
/// not enabled, and UI text is composited above the scene for free.
fn sync_grid_labels(
    mode: Res<EditorMode>,
    editor: Res<EditorState>,
    view: Res<TimelineView>,
    area: Query<&ComputedNode, With<TimelineArea>>,
    mut labels: Query<(&mut Node, &mut Text, &mut TextColor), With<GridLabel>>,
) {
    // `left`, `top`, text, and whether the line lies inside the animation.
    let mut placed: Vec<(f32, f32, String, bool)> = Vec::new();
    if *mode == EditorMode::Animate
        && let Ok(node) = area.single()
        && let Some(animation) = selected_animation(&editor)
    {
        let size = logical_size(node);
        if size.x > 0.0 && size.y > 0.0 {
            let duration = animation.duration.max(0.0);
            let top = (size.y - TRACK_INSET as f32 - GRID_LABEL_FONT_SIZE - 4.0).max(0.0);
            for tick in grid_ticks(&view, content_size(size)) {
                if let Some(label) = tick.label {
                    let inside = tick.time >= 0.0 && tick.time <= f64::from(duration);
                    placed.push((tick.x as f32 + 3.0, top, label, inside));
                }
            }
        }
    }

    for (index, (mut node, mut text, mut color)) in labels.iter_mut().enumerate() {
        match placed.get(index) {
            Some((left, top, label, inside)) => {
                node.display = Display::Flex;
                node.left = Val::Px(*left);
                node.top = Val::Px(*top);
                if text.0 != *label {
                    text.0.clone_from(label);
                }
                color.0 = if *inside {
                    theme::MUTED
                } else {
                    LABEL_DISABLED
                };
            }
            None => {
                node.display = Display::None;
            }
        }
    }
}

/// Darkens the parts of the track before t = 0 and after t = `duration`.
fn draw_disabled_spans(scene: &mut Scene, view: &TimelineView, size: Vec2, duration: f32) {
    let track = track_rect(size);
    let zero_x = (TRACK_INSET + view.screen_x(0.0)).clamp(track.x0, track.x1);
    let end_x = (TRACK_INSET + view.screen_x(f64::from(duration))).clamp(track.x0, track.x1);

    if zero_x > track.x0 {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            DISABLED_SCRIM,
            None,
            &Rect::new(track.x0, track.y0, zero_x, track.y1),
        );
    }
    if end_x < track.x1 {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            DISABLED_SCRIM,
            None,
            &Rect::new(end_x, track.y0, track.x1, track.y1),
        );
    }
}

/// Draws the playhead line and its top handle, at a clamped time.
fn draw_playhead(scene: &mut Scene, view: &TimelineView, size: Vec2, playhead: f32) {
    let track = track_rect(size);
    let x = (TRACK_INSET + view.screen_x(f64::from(playhead))).clamp(track.x0, track.x1);

    let line = Line::new(Point::new(x, 0.0), Point::new(x, track.y1));
    scene.stroke(
        &Stroke::new(PLAYHEAD_WIDTH),
        Affine::IDENTITY,
        PLAYHEAD_COLOR,
        None,
        &line,
    );

    let handle = RoundedRect::new(
        x - PLAYHEAD_HANDLE_HALF_WIDTH,
        0.0,
        x + PLAYHEAD_HANDLE_HALF_WIDTH,
        PLAYHEAD_HANDLE_HEIGHT,
        3.0,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        PLAYHEAD_COLOR,
        None,
        &handle,
    );
    scene.stroke(
        &Stroke::new(1.0),
        Affine::IDENTITY,
        PLAYHEAD_OUTLINE,
        None,
        &handle,
    );
}

/// Draws the box-selection rectangle. The stored corners are track-local, so
/// the track inset is added when they reach the scene.
fn draw_marquee(scene: &mut Scene, marquee: &MarqueeDrag) {
    let min = marquee.start.min(marquee.current);
    let max = marquee.start.max(marquee.current);
    let rect = Rect::new(
        TRACK_INSET + f64::from(min.x),
        f64::from(min.y),
        TRACK_INSET + f64::from(max.x),
        f64::from(max.y),
    );
    scene.fill(Fill::NonZero, Affine::IDENTITY, MARQUEE_FILL, None, &rect);
    scene.stroke(
        &Stroke::new(1.0),
        Affine::IDENTITY,
        MARQUEE_STROKE,
        None,
        &rect,
    );
}

/// Clamps a playhead time to the animation's `0..=duration` range.
fn clamp_playhead(time: f32, duration: f32) -> f32 {
    let time = if time.is_finite() { time } else { 0.0 };
    if duration.is_finite() && duration > 0.0 {
        time.clamp(0.0, duration)
    } else {
        0.0
    }
}

/// The integer step that keeps gridlines at least [`GRID_MIN_SPACING`] apart.
fn grid_step(time_per_pixel: f32) -> f32 {
    let mut decade = 1.0;
    loop {
        for multiple in [1.0, 2.0, 5.0] {
            let step = decade * multiple;
            if step / time_per_pixel >= GRID_MIN_SPACING {
                return step;
            }
        }
        decade *= 10.0;
        if decade > 1e12 {
            return decade;
        }
    }
}

/// The repeated cycles whose curves can intersect the visible window.
fn visible_cycles(view: &TimelineView, size: Vec2, duration: f32) -> std::ops::RangeInclusive<i64> {
    if !duration.is_finite() || duration <= 0.0 {
        return 0..=0;
    }
    let (start, end) = view.visible_time(size);
    if !start.is_finite() || !end.is_finite() {
        return 0..=0;
    }
    let first = (f64::from(start) / f64::from(duration)).floor() as i64 - 1;
    let last = (f64::from(end) / f64::from(duration)).ceil() as i64 + 1;
    if last - first <= MAX_VISIBLE_CYCLES {
        first..=last
    } else {
        // Keep the window centred when too many repeats would be drawn.
        let center = (first + last) / 2;
        let half = MAX_VISIBLE_CYCLES / 2;
        center - half..=center + half
    }
}

/// Draws one period (or the single non-cyclic curve) of a target's keyframes.
#[allow(clippy::too_many_arguments)]
fn draw_group(
    scene: &mut Scene,
    group: &[&Keyframe],
    period: Option<f32>,
    cycle: i64,
    color: Color,
    view: &TimelineView,
    selected: &[&Keyframe],
    selected_handles: &[(&Keyframe, HandleSide)],
    dimmed: bool,
) {
    let Some(first) = group.first() else {
        return;
    };
    let count = group.len();
    let cyclic = period.is_some();
    let base = period.map_or(0.0, |period| cycle as f64 * f64::from(period));

    // Curves first, so the points and handles sit on top.
    for index in 0..count {
        let next = if index + 1 < count {
            index + 1
        } else if cyclic {
            0
        } else {
            continue;
        };
        let (end_time, end_value) = if next == 0 {
            let period = period.unwrap_or(0.0);
            (
                f64::from(first.timestamp) + f64::from(period) + base,
                f64::from(first.value),
            )
        } else {
            (
                f64::from(group[next].timestamp) + base,
                f64::from(group[next].value),
            )
        };
        draw_segment(
            scene,
            view,
            f64::from(group[index].timestamp) + base,
            f64::from(group[index].value),
            end_time,
            end_value,
            effective_handle_out(group, index, cyclic),
            effective_handle_in(group, next, cyclic),
            color,
        );
    }

    for (index, keyframe) in group.iter().enumerate() {
        let time = f64::from(keyframe.timestamp) + base;
        let value = f64::from(keyframe.value);

        let next_time = if index + 1 < count {
            Some(f64::from(group[index + 1].timestamp) + base)
        } else {
            period.map(|period| f64::from(first.timestamp) + f64::from(period) + base)
        };
        let handle_selected = |side: HandleSide| {
            selected_handles.iter().any(|(candidate, candidate_side)| {
                *candidate_side == side && std::ptr::eq(*candidate, *keyframe)
            })
        };

        if let (Some(handle), Some(next)) = (keyframe.handle_out, next_time) {
            draw_handle(
                scene,
                view,
                time,
                value,
                next,
                f64::from(handle),
                true,
                handle_selected(HandleSide::Out),
                dimmed,
            );
        }

        let previous_time = if index > 0 {
            Some(f64::from(group[index - 1].timestamp) + base)
        } else {
            period.map(|period| f64::from(group[count - 1].timestamp) - f64::from(period) + base)
        };
        if let (Some(handle), Some(previous)) = (keyframe.handle_in, previous_time) {
            draw_handle(
                scene,
                view,
                time,
                value,
                previous,
                f64::from(handle),
                false,
                handle_selected(HandleSide::In),
                dimmed,
            );
        }

        let center = view.point(time, value);
        if selected
            .iter()
            .any(|candidate| std::ptr::eq(*candidate, *keyframe))
        {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                SELECTED_RING,
                None,
                &Circle::new(center, KEYFRAME_RADIUS + 3.0),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            KEYFRAME_OUTLINE,
            None,
            &Circle::new(center, KEYFRAME_RADIUS + 1.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Circle::new(center, KEYFRAME_RADIUS),
        );
    }
}

/// Strokes the cubic between two keyframe positions. For a cyclic wrap the far
/// endpoint is the first keyframe of the following period.
#[allow(clippy::too_many_arguments)]
fn draw_segment(
    scene: &mut Scene,
    view: &TimelineView,
    start_time: f64,
    start_value: f64,
    end_time: f64,
    end_value: f64,
    control_out: Option<f32>,
    control_in: Option<f32>,
    color: Color,
) {
    let gap = end_time - start_time;
    if !gap.is_finite() || gap <= 0.0 {
        return;
    }
    let control_out = control_out.map_or(start_value, f64::from);
    let control_in = control_in.map_or(end_value, f64::from);

    let mut path = BezPath::new();
    path.move_to(view.point(start_time, start_value));
    path.curve_to(
        view.point(start_time + gap / 3.0, control_out),
        view.point(end_time - gap / 3.0, control_in),
        view.point(end_time, end_value),
    );
    scene.stroke(
        &Stroke::new(CURVE_WIDTH),
        Affine::IDENTITY,
        color,
        None,
        &path,
    );
}

/// Draws a handle point a third of the way from `time` toward `neighbour`, and
/// a faint arm joining it to the keyframe.
#[allow(clippy::too_many_arguments)]
fn draw_handle(
    scene: &mut Scene,
    view: &TimelineView,
    time: f64,
    value: f64,
    neighbour: f64,
    handle_value: f64,
    ahead: bool,
    selected: bool,
    dimmed: bool,
) {
    let span = (neighbour - time).abs();
    let handle_time = if ahead {
        time + span / 3.0
    } else {
        time - span / 3.0
    };
    let center = view.point(handle_time, handle_value);
    let arm = Line::new(view.point(time, value), center);
    scene.stroke(
        &Stroke::new(HANDLE_ARM_WIDTH),
        Affine::IDENTITY,
        if dimmed {
            DIMMED_HANDLE_ARM
        } else {
            HANDLE_ARM
        },
        None,
        &arm,
    );
    if selected {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            SELECTED_RING,
            None,
            &Circle::new(center, HANDLE_RADIUS + 2.5),
        );
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        if dimmed {
            DIMMED_HANDLE_FILL
        } else {
            HANDLE_FILL
        },
        None,
        &Circle::new(center, HANDLE_RADIUS),
    );
}

/// Lowest and highest value reached by the keyframes and their handles,
/// optionally limited to one bone's curves.
fn value_bounds(animation: &Animation, bone: Option<&str>) -> (f32, f32) {
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    for keyframe in &animation.keyframes {
        if bone.is_some_and(|bone| keyframe_bone(keyframe) != Some(bone)) {
            continue;
        }
        for value in [
            Some(keyframe.value),
            keyframe.handle_in,
            keyframe.handle_out,
        ]
        .into_iter()
        .flatten()
        {
            if value.is_finite() {
                low = low.min(value);
                high = high.max(value);
            }
        }
    }
    if low > high { (-1.0, 1.0) } else { (low, high) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viveco::model::{BoneProperty, Document, FORMAT_VERSION, Keyframe, KeyframeTarget};

    fn target(bone: &str, property: BoneProperty) -> KeyframeTarget {
        KeyframeTarget::Bone {
            bone: bone.into(),
            property,
        }
    }

    fn animation_with(timestamps: &[f32], values: &[f32], duration: f32) -> Animation {
        let mut animation = Animation::new(duration);
        for (&timestamp, &value) in timestamps.iter().zip(values) {
            animation.insert_keyframe(Keyframe::new(
                timestamp,
                value,
                target("Arm", BoneProperty::X),
            ));
        }
        animation
    }

    fn fitted(animation: &Animation, size: Vec2) -> TimelineView {
        let mut view = TimelineView::default();
        view.fit(animation, size, None);
        view
    }

    fn two_bone_animation() -> Animation {
        let mut animation = Animation::new(4.0);
        animation.insert_keyframe(Keyframe::new(0.0, 0.0, target("Arm", BoneProperty::X)));
        animation.insert_keyframe(Keyframe::new(4.0, 100.0, target("Arm", BoneProperty::X)));
        animation.insert_keyframe(Keyframe::new(0.0, -500.0, target("Leg", BoneProperty::Y)));
        animation.insert_keyframe(Keyframe::new(4.0, 500.0, target("Leg", BoneProperty::Y)));
        animation
    }

    #[test]
    fn value_bounds_can_be_limited_to_one_bone() {
        let animation = two_bone_animation();

        assert_eq!(value_bounds(&animation, None), (-500.0, 500.0));
        assert_eq!(value_bounds(&animation, Some("Arm")), (0.0, 100.0));
        // A bone with no curves has no range to frame.
        assert_eq!(value_bounds(&animation, Some("Tail")), (-1.0, 1.0));
    }

    #[test]
    fn focusing_a_bone_refits_only_the_value_axis() {
        let animation = two_bone_animation();
        let size = Vec2::new(400.0, 200.0);
        let mut view = fitted(&animation, size);
        let time0 = view.time0;
        let time_per_pixel = view.time_per_pixel;
        let whole = view.value_per_pixel;

        view.fit_value(&animation, size, Some("Arm"));

        // The time axis is untouched, while the values frame Arm's range.
        assert_eq!(view.time0, time0);
        assert_eq!(view.time_per_pixel, time_per_pixel);
        assert!(view.value_per_pixel < whole);
        assert!(view.value_at(0.0) > 100.0);
        assert!(view.value_at(size.y) < 0.0);
    }

    #[test]
    fn groups_are_matched_to_their_bone() {
        let animation = two_bone_animation();
        let groups = curve_groups(&animation);

        assert!(group_is_bone(&groups[0], "Arm"));
        assert!(!group_is_bone(&groups[0], "Leg"));
        assert!(!group_is_bone(&groups[1], "Arm"));
        assert!(group_is_bone(&groups[1], "Leg"));
        assert!(!group_is_bone(&[], "Arm"));
    }

    #[test]
    fn fit_frames_duration_and_values() {
        let animation = animation_with(&[0.0, 4.0], &[0.0, 100.0], 4.0);
        let view = fitted(&animation, Vec2::new(400.0, 200.0));

        // The whole duration sits inside the view with only a small margin.
        assert!(view.screen_x(0.0) > 0.0);
        assert!(view.screen_x(4.0) < 400.0);
        assert!(view.screen_x(0.0) < 25.0);
        assert!(view.screen_x(4.0) > 375.0);

        // Larger values are higher on screen (smaller y).
        assert!(view.screen_y(100.0) < view.screen_y(0.0));
        assert!(view.screen_y(100.0) > 0.0);
        assert!(view.screen_y(0.0) < 200.0);
    }

    #[test]
    fn zoom_keeps_the_anchor_point_fixed() {
        let animation = animation_with(&[0.0, 4.0], &[0.0, 100.0], 4.0);
        let mut view = fitted(&animation, Vec2::new(400.0, 200.0));
        let anchor = Vec2::new(130.0, 70.0);
        let time = view.time_at(anchor.x);
        let value = view.value_at(anchor.y);

        view.zoom_at(anchor, 2.5);

        assert!((view.time_at(anchor.x) - time).abs() < 1e-3);
        assert!((view.value_at(anchor.y) - value).abs() < 1e-2);
        // Zooming in reduces both units-per-pixel scales.
        assert!(view.time_per_pixel < 4.0 / 400.0);
        assert!(view.value_per_pixel < 100.0 / 200.0);
    }

    #[test]
    fn zoom_axes_scale_independently() {
        let animation = animation_with(&[0.0, 4.0], &[0.0, 100.0], 4.0);
        let mut view = fitted(&animation, Vec2::new(400.0, 200.0));
        let time_scale = view.time_per_pixel;
        let value_scale = view.value_per_pixel;

        // A time-only zoom leaves the value axis untouched.
        view.zoom_axes_at(Vec2::ZERO, 2.0, 1.0);
        assert!((view.time_per_pixel - time_scale / 2.0).abs() < 1e-9);
        assert!((view.value_per_pixel - value_scale).abs() < 1e-9);

        // And a value-only zoom leaves the time axis untouched.
        view.zoom_axes_at(Vec2::ZERO, 1.0, 4.0);
        assert!((view.time_per_pixel - time_scale / 2.0).abs() < 1e-9);
        assert!((view.value_per_pixel - value_scale / 4.0).abs() < 1e-9);
    }

    #[test]
    fn pan_moves_content_with_the_cursor() {
        let animation = animation_with(&[0.0, 4.0], &[0.0, 100.0], 4.0);
        let mut view = fitted(&animation, Vec2::new(400.0, 200.0));
        let time = view.time_at(0.0);
        let value = view.value_at(0.0);

        view.pan(Vec2::new(10.0, 5.0));

        assert!((view.time_at(10.0) - time).abs() < 1e-4);
        assert!((view.value_at(5.0) - value).abs() < 1e-4);
    }

    #[test]
    fn grid_step_keeps_lines_apart() {
        for time_per_pixel in [0.0001, 0.001, 0.01, 0.1, 1.0, 50.0] {
            let step = grid_step(time_per_pixel);
            assert!(
                step / time_per_pixel >= GRID_MIN_SPACING,
                "step {step} too dense for {time_per_pixel}"
            );
        }
    }

    #[test]
    fn visible_cycles_cover_the_window() {
        let animation = animation_with(&[0.0, 0.5], &[0.0, 1.0], 1.0);
        let mut view = fitted(&animation, Vec2::new(400.0, 200.0));
        view.time0 = 0.0;
        view.time_per_pixel = 0.01;

        let cycles = visible_cycles(&view, Vec2::new(400.0, 200.0), 1.0);

        // The window spans t = 0..4, so repetitions 0 through 4 all intersect.
        for cycle in 0..=4 {
            assert!(cycles.contains(&cycle));
        }
        assert!(!cycles.contains(&100));
    }

    #[test]
    fn curve_groups_split_by_target() {
        let mut animation = Animation::new(4.0);
        let x = target("Arm", BoneProperty::X);
        let angle = target("Arm", BoneProperty::Angle);
        animation.insert_keyframe(Keyframe::new(0.0, 0.0, x.clone()));
        animation.insert_keyframe(Keyframe::new(1.0, 1.0, angle.clone()));
        animation.insert_keyframe(Keyframe::new(2.0, 2.0, x));
        animation.insert_keyframe(Keyframe::new(3.0, 3.0, angle));

        let groups = curve_groups(&animation);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[1].len(), 2);
        assert_eq!(groups[0][0].timestamp, 0.0);
        assert_eq!(groups[1][0].timestamp, 1.0);
    }

    #[test]
    fn labels_are_thinned_to_the_minimum_spacing() {
        // Gridlines 100 px apart can all be labelled.
        assert_eq!(label_multiple(1.0, 0.01), 1);
        // 20 px apart, labels skip three lines to reach 60 px.
        assert_eq!(label_multiple(1.0, 0.05), 3);
        // 10 px apart, five lines put labels 50 px apart.
        assert_eq!(label_multiple(1.0, 0.1), 5);
    }

    #[test]
    fn grid_ticks_cover_the_window_and_label_sparsely() {
        let animation = animation_with(&[0.0, 4.0], &[0.0, 100.0], 4.0);
        let mut view = fitted(&animation, Vec2::new(400.0, 200.0));
        view.time0 = 0.0;
        // One second per 30 pixels: lines 30 px apart, so labels skip two.
        view.time_per_pixel = 1.0 / 30.0;

        let ticks = grid_ticks(&view, Vec2::new(400.0, 200.0));

        // The window spans t = 0..13, so every integer is a gridline...
        assert_eq!(ticks.first().map(|tick| tick.time), Some(0.0));
        assert!(ticks.iter().any(|tick| (tick.time - 13.0).abs() < 1e-6));
        // ...but only every second one carries a label.
        let labelled: Vec<&str> = ticks
            .iter()
            .filter_map(|tick| tick.label.as_deref())
            .collect();
        assert!(labelled.contains(&"0"));
        assert!(labelled.contains(&"2"));
        assert!(!labelled.contains(&"1"));
    }

    #[test]
    fn playhead_and_duration_are_clamped() {
        assert_eq!(clamp_playhead(-2.0, 5.0), 0.0);
        assert_eq!(clamp_playhead(7.0, 5.0), 5.0);
        assert_eq!(clamp_playhead(3.0, 5.0), 3.0);
        // A zero-length animation pins the playhead to the start.
        assert_eq!(clamp_playhead(3.0, 0.0), 0.0);
        assert_eq!(clamp_playhead(f32::NAN, 5.0), 0.0);
    }

    #[test]
    fn drag_axis_is_the_most_aligned_cardinal_direction() {
        assert_eq!(dominant_axis(Vec2::new(1.0, 0.0), 3.0), None);
        assert_eq!(
            dominant_axis(Vec2::new(10.0, 2.0), 3.0),
            Some(DragAxis::Time)
        );
        assert_eq!(
            dominant_axis(Vec2::new(2.0, -10.0), 3.0),
            Some(DragAxis::Value)
        );
        // Ties go to time.
        assert_eq!(
            dominant_axis(Vec2::new(5.0, 5.0), 3.0),
            Some(DragAxis::Time)
        );
    }

    #[test]
    fn keyframe_hit_testing_finds_the_nearest_point() {
        let animation = animation_with(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], 2.0);
        let view = fitted(&animation, Vec2::new(400.0, 200.0));
        let size = content_size(Vec2::new(400.0, 200.0));
        let position = keyframe_position(&view, &animation.keyframes[1]);

        assert_eq!(keyframe_under(&animation, &view, size, position), Some(1));
        assert_eq!(
            keyframe_under(&animation, &view, size, position + Vec2::splat(100.0)),
            None
        );
    }

    #[test]
    fn explicit_handles_sit_a_third_toward_their_neighbour() {
        let mut animation = animation_with(&[0.0, 3.0], &[0.0, 30.0], 3.0);
        animation.keyframes[0].handle_out = Some(15.0);
        animation.keyframes[1].handle_in = Some(20.0);
        let view = fitted(&animation, Vec2::new(300.0, 200.0));

        let position = handle_position(&animation, &view, 0, HandleSide::Out, 0.0).unwrap();
        let expected = Vec2::new(view.screen_x(1.0) as f32, view.screen_y(15.0) as f32);
        assert!((position - expected).length() < 0.01);
        assert_eq!(
            handle_under(
                &animation,
                &view,
                content_size(Vec2::new(300.0, 200.0)),
                position
            ),
            Some((0, HandleSide::Out))
        );

        // Omitted handles have no position and cannot be grabbed.
        assert!(handle_position(&animation, &view, 0, HandleSide::In, 0.0).is_none());
    }

    #[test]
    fn cyclic_handles_wrap_across_the_seam() {
        let mut animation = animation_with(&[0.5, 1.5], &[0.0, 100.0], 2.0);
        animation.cyclic = true;
        animation.keyframes[0].handle_in = Some(10.0);
        animation.keyframes[1].handle_out = Some(90.0);
        let view = fitted(&animation, Vec2::new(400.0, 200.0));

        // The first keyframe's incoming handle looks back into the previous
        // period, and the last keyframe's outgoing handle forward into the
        // next one.
        let incoming = handle_position(&animation, &view, 0, HandleSide::In, 0.0).unwrap();
        let outgoing = handle_position(&animation, &view, 1, HandleSide::Out, 0.0).unwrap();
        assert!((incoming.x - view.screen_x(0.5 - 1.0 / 3.0) as f32).abs() < 0.01);
        assert!((outgoing.x - view.screen_x(1.5 + 1.0 / 3.0) as f32).abs() < 0.01);
    }

    #[test]
    fn time_drags_move_points_past_their_neighbours() {
        let animation = animation_with(&[0.0, 1.0, 2.0, 3.0], &[0.0, 1.0, 2.0, 3.0], 4.0);

        // The keyframe at 1 s moves to 2.5 s, crossing the one at 2 s.
        let (keyframes, selected) =
            drag_keyframes(&animation.keyframes, &[1], DragAxis::Time, 1.5, 4.0, false);
        let times: Vec<f32> = keyframes
            .iter()
            .map(|keyframe| keyframe.timestamp)
            .collect();
        assert_eq!(times, vec![0.0, 2.0, 2.5, 3.0]);
        // The selection follows the dragged keyframe to its new position.
        assert_eq!(selected, vec![2]);
    }

    #[test]
    fn non_cyclic_drags_stop_at_the_edges() {
        assert_eq!(dragged_time(1.0, -5.0, 4.0, false), 0.0);
        assert_eq!(dragged_time(1.0, 5.0, 4.0, false), 4.0);
        assert_eq!(dragged_time(1.0, 1.5, 4.0, false), 2.5);
    }

    #[test]
    fn non_cyclic_drag_delta_keeps_the_group_in_range() {
        let animation = animation_with(&[1.0, 3.0], &[0.0, 1.0], 4.0);

        assert_eq!(clamp_drag_delta(&animation.keyframes, &[0], 10.0, 4.0), 3.0);
        assert_eq!(
            clamp_drag_delta(&animation.keyframes, &[1], -10.0, 4.0),
            -3.0
        );
        // The group stops when its furthest keyframe reaches the edge.
        assert_eq!(
            clamp_drag_delta(&animation.keyframes, &[0, 1], 10.0, 4.0),
            1.0
        );
        assert_eq!(
            clamp_drag_delta(&animation.keyframes, &[0, 1], 0.5, 4.0),
            0.5
        );
    }

    #[test]
    fn cyclic_drags_wrap_around_the_animation() {
        let animation = animation_with(&[0.5, 2.0], &[0.0, 1.0], 4.0);

        // Dragging the keyframe at 0.5 s back by a second wraps it to 3.5 s.
        let (keyframes, selected) =
            drag_keyframes(&animation.keyframes, &[0], DragAxis::Time, -1.0, 4.0, true);
        let times: Vec<f32> = keyframes
            .iter()
            .map(|keyframe| keyframe.timestamp)
            .collect();
        assert_eq!(times, vec![2.0, 3.5]);
        assert_eq!(selected, vec![1]);

        // Wrapping is also how a keyframe crosses the end of the window.
        assert!((dragged_time(0.5, 5.0, 4.0, true) - 1.5).abs() < 1e-6);
    }

    #[test]
    fn dragged_points_landing_on_a_shared_time_are_removed() {
        let animation = animation_with(&[1.0, 2.0], &[0.0, 1.0], 4.0);

        // The dragged keyframe lands exactly on the stationary one, so it
        // vanishes while overlapping; releasing here deletes it.
        let (keyframes, selected) =
            drag_keyframes(&animation.keyframes, &[0], DragAxis::Time, 1.0, 4.0, false);
        assert_eq!(keyframes.len(), 1);
        assert_eq!(keyframes[0].timestamp, 2.0);
        assert!(selected.is_empty());
    }

    #[test]
    fn different_curves_may_share_a_time() {
        let mut animation = Animation::new(10.0);
        animation.insert_keyframe(Keyframe::new(1.0, 0.0, target("Arm", BoneProperty::X)));
        animation.insert_keyframe(Keyframe::new(2.0, 0.0, target("Arm", BoneProperty::Y)));

        // The Arm X keyframe may slide onto the Arm Y keyframe's time.
        let (keyframes, _) =
            drag_keyframes(&animation.keyframes, &[0], DragAxis::Time, 1.0, 10.0, false);
        assert_eq!(keyframes.len(), 2);
        assert_eq!(keyframes[0].timestamp, 2.0);
        assert_eq!(keyframes[1].timestamp, 2.0);
    }

    fn document_with(animation: Animation) -> Document {
        Document {
            version: FORMAT_VERSION,
            shapes: Vec::new(),
            bones: Vec::new(),
            animations: vec![animation],
        }
    }

    #[test]
    fn delete_removes_selected_handles_and_keyframes() {
        let mut animation = animation_with(&[0.0, 1.0, 2.0], &[0.0, 10.0, 20.0], 2.0);
        animation.keyframes[1].handle_out = Some(15.0);
        let mut editor = EditorState::new(document_with(animation));
        editor.selected_animation = Some(0);

        let mut edit = TimelineEdit {
            animation: Some(0),
            selected: vec![0, 2],
            selected_handles: vec![(1, HandleSide::Out)],
            ..default()
        };

        delete_selection(&mut editor, &mut edit);

        let animation = &editor.document.animations[0];
        // Only the unselected middle keyframe survives.
        assert_eq!(animation.keyframes.len(), 1);
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
        // Its selected handle was cleared as well.
        assert_eq!(animation.keyframes[0].handle_out, None);
        assert!(!edit.has_selection());
    }

    #[test]
    fn symmetric_handles_mirror_across_the_keyframe_value() {
        // With equally spaced neighbours the offset is simply reflected.
        assert_eq!(mirror_handle(10.0, 14.0, 1.0, 1.0), 6.0);
        assert_eq!(mirror_handle(10.0, 6.0, 1.0, 1.0), 14.0);
        // A flat handle stays flat.
        assert_eq!(mirror_handle(10.0, 10.0, 1.0, 1.0), 10.0);
    }

    #[test]
    fn symmetric_handles_match_slopes_across_uneven_spans() {
        let value = 0.7;
        let incoming = 0.3;
        let gap_in = 0.4;
        let gap_out = 1.3;

        let outgoing = mirror_handle(value, incoming, gap_in, gap_out);

        // The value slope per second is the same on both sides of the pivot.
        let left_slope = (value - incoming) / gap_in;
        let right_slope = (outgoing - value) / gap_out;
        assert!((left_slope - right_slope).abs() < 1e-5);
    }

    #[test]
    fn adjacent_spans_measure_the_surrounding_segments() {
        let animation = animation_with(&[0.0, 1.0, 3.0], &[0.0, 1.0, 2.0], 3.0);
        // The middle keyframe has a one-second incoming and two-second
        // outgoing segment.
        assert_eq!(adjacent_spans(&animation, 1), Some((1.0, 2.0)));

        // Cyclic endpoints wrap across the seam.
        let mut cyclic = animation_with(&[1.0, 2.0, 3.0], &[0.0, 1.0, 2.0], 4.0);
        cyclic.cyclic = true;
        assert_eq!(adjacent_spans(&cyclic, 0), Some((2.0, 1.0)));
        assert_eq!(adjacent_spans(&cyclic, 2), Some((1.0, 2.0)));

        // Non-cyclic endpoints have no opposite segment.
        let open = animation_with(&[1.0, 2.0], &[0.0, 1.0], 2.0);
        assert_eq!(adjacent_spans(&open, 0), None);
        assert_eq!(adjacent_spans(&open, 1), None);
    }

    #[test]
    fn shift_drag_side_follows_the_horizontal_direction() {
        // Movement below the commit threshold does not choose a side.
        assert_eq!(handle_side_toward(1.0), None);
        assert_eq!(handle_side_toward(10.0), Some(HandleSide::Out));
        assert_eq!(handle_side_toward(-10.0), Some(HandleSide::In));
    }

    #[test]
    fn double_clicks_repeat_a_recent_press_on_the_same_keyframe() {
        let last = Some((10.0, 3, Vec2::new(40.0, 20.0)));
        // A second press soon after, on the same keyframe and spot, counts.
        assert!(repeats_press(last, 10.2, 3, Vec2::new(41.0, 21.0)));
        // Too late, a different keyframe, or too far away does not.
        assert!(!repeats_press(last, 10.6, 3, Vec2::new(40.0, 20.0)));
        assert!(!repeats_press(last, 10.2, 4, Vec2::new(40.0, 20.0)));
        assert!(!repeats_press(last, 10.2, 3, Vec2::new(60.0, 20.0)));
        // Without a previous press there is nothing to repeat.
        assert!(!repeats_press(None, 10.2, 3, Vec2::new(40.0, 20.0)));
    }

    #[test]
    fn value_drags_carry_handles_along() {
        let mut keyframe = Keyframe::new(1.0, 10.0, target("Arm", BoneProperty::X));
        keyframe.handle_in = Some(8.0);
        keyframe.handle_out = Some(12.0);

        move_keyframe_value(&mut keyframe, 10.0, Some(8.0), Some(12.0), 5.0);

        assert_eq!(keyframe.value, 15.0);
        assert_eq!(keyframe.handle_in, Some(13.0));
        assert_eq!(keyframe.handle_out, Some(17.0));
        // A value drag leaves the keyframe's time alone.
        assert_eq!(keyframe.timestamp, 1.0);
    }

    #[test]
    fn value_drags_leave_omitted_handles_omitted() {
        let mut keyframe = Keyframe::new(1.0, 10.0, target("Arm", BoneProperty::X));

        move_keyframe_value(&mut keyframe, 10.0, None, None, -2.0);

        assert_eq!(keyframe.value, 8.0);
        assert_eq!(keyframe.handle_in, None);
        assert_eq!(keyframe.handle_out, None);
    }

    #[test]
    fn aligning_merges_same_curve_keyframes_at_the_target_time() {
        let mut animation = Animation::new(4.0);
        let x = target("Arm", BoneProperty::X);
        animation.insert_keyframe(Keyframe::new(1.0, 0.0, x.clone()));
        animation.insert_keyframe(Keyframe::new(1.4, 1.0, x.clone()));
        animation.insert_keyframe(Keyframe::new(2.5, 2.0, target("Arm", BoneProperty::Y)));
        animation.insert_keyframe(Keyframe::new(3.5, 3.0, x.clone()));

        // Align the two Arm X points and the Arm Y point to t = 1, anchored on
        // the second Arm X point.
        let selected = align_selection(&mut animation, &[0, 1, 2], 1.0, 1);

        // One Arm X point and one Arm Y point survive.
        assert_eq!(selected.len(), 2);
        assert_eq!(animation.keyframes.len(), 3);
        let values: Vec<f32> = selected
            .iter()
            .map(|&index| animation.keyframes[index].value)
            .collect();
        // The anchor (value 1) and the Arm Y point (value 2) survive; the
        // other Arm X point was merged away.
        assert!(values.contains(&1.0));
        assert!(values.contains(&2.0));
        // The unselected keyframe kept its time, and the list stays sorted.
        assert_eq!(animation.keyframes[0].timestamp, 1.0);
        assert_eq!(animation.keyframes[1].timestamp, 1.0);
        assert_eq!(animation.keyframes[2].timestamp, 3.5);
        assert_eq!(animation.keyframes[2].value, 3.0);
    }

    #[test]
    fn alignment_keeps_the_keyframe_nearest_the_target_on_other_curves() {
        let mut animation = Animation::new(6.0);
        let x = target("Arm", BoneProperty::X);
        let y = target("Arm", BoneProperty::Y);
        animation.insert_keyframe(Keyframe::new(0.0, 0.0, x.clone()));
        animation.insert_keyframe(Keyframe::new(1.2, 1.0, y.clone()));
        animation.insert_keyframe(Keyframe::new(2.6, 2.0, y.clone()));
        animation.insert_keyframe(Keyframe::new(3.0, 3.0, x.clone()));

        // Align to the Arm X point at t = 3. Of the two Arm Y points, the one
        // at t = 2.6 was closer to the target than the one at t = 1.2.
        let selected = align_selection(&mut animation, &[0, 1, 2, 3], 3.0, 3);

        assert_eq!(selected.len(), 2);
        assert_eq!(animation.keyframes.len(), 2);
        let values: Vec<f32> = selected
            .iter()
            .map(|&index| animation.keyframes[index].value)
            .collect();
        assert!(values.contains(&2.0));
        assert!(values.contains(&3.0));
        assert!(!values.contains(&1.0));
        for keyframe in &animation.keyframes {
            assert_eq!(keyframe.timestamp, 3.0);
        }
    }
}
