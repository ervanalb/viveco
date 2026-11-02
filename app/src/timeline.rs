//! Read-only timeline view for animate mode.
//!
//! The bottom animate panel hosts a [`TimelineArea`] rendered with a
//! [`UiVelloScene`]. The view spans time horizontally and animated value
//! vertically, with independent pan and zoom on each axis:
//!
//! * wheel scrolls the value axis, shift-wheel scrolls time, ctrl-wheel zooms
//!   around the cursor;
//! * left-drag pans, right-drag zooms around the cursor.
//!
//! Keyframes are drawn as points, their handles as smaller points a third of
//! the way toward the neighbouring keyframe, and the cubic curve they describe
//! is stroked between them. Cyclic animations repeat the whole pattern with a
//! period equal to the duration, and only the repetitions intersecting the
//! visible window are emitted.

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
use crate::editor::{EditorMode, EditorState};
use crate::model::{Animation, Keyframe};
use crate::theme;

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

const PLAYHEAD_COLOR: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff);
const PLAYHEAD_OUTLINE: Color = Color::from_rgba8(0x0b, 0x2a, 0x40, 0xff);
const PLAYHEAD_WIDTH: f64 = 1.5;
const PLAYHEAD_HANDLE_HALF_WIDTH: f64 = 6.0;
const PLAYHEAD_HANDLE_HEIGHT: f64 = 14.0;

/// How close to the playhead handle a press must be to grab it, in pixels.
const PLAYHEAD_GRAB_HALF_WIDTH: f32 = 10.0;
const PLAYHEAD_GRAB_HEIGHT: f32 = 18.0;

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

/// One color per animated channel, cycled when there are more channels than
/// colors.
const CHANNEL_COLORS: [Color; 6] = [
    Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff),
    Color::from_rgba8(0xf0, 0xb4, 0x4c, 0xff),
    Color::from_rgba8(0x8a, 0xe0, 0x8a, 0xff),
    Color::from_rgba8(0xf0, 0x7a, 0x9c, 0xff),
    Color::from_rgba8(0xb4, 0x8a, 0xf0, 0xff),
    Color::from_rgba8(0x7a, 0xd8, 0xd0, 0xff),
];

pub struct TimelinePlugin;

impl Plugin for TimelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimelineView>().add_systems(
            Update,
            (
                fit_timeline,
                handle_timeline_input,
                draw_timeline,
                sync_grid_labels,
            )
                .chain(),
        );
    }
}

/// Marks the UI node the timeline draws into.
#[derive(Component)]
pub struct TimelineArea;

/// Marks one of the pooled time-label nodes.
#[derive(Component)]
struct GridLabel;

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
    fitted: Option<FitKey>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            time0: 0.0,
            time_per_pixel: 0.01,
            value0: 1.0,
            value_per_pixel: 0.01,
            fitted: None,
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

    /// Frames the animation's duration and values with a small margin.
    fn fit(&mut self, animation: &Animation, size: Vec2) {
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

        let (mut low, mut high) = value_bounds(animation);
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

/// What the current framing was computed for. Re-fitting only when this
/// changes keeps manual panning and zooming intact.
#[derive(Clone, Copy, PartialEq)]
struct FitKey {
    selected: Option<usize>,
    duration: f32,
    cyclic: bool,
    keyframes: usize,
    width: f32,
    height: f32,
    scale: f32,
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
        return;
    };
    let key = FitKey {
        selected: editor.selected_animation,
        duration: animation.duration,
        cyclic: animation.cyclic,
        keyframes: animation.keyframes.len(),
        width: area.x,
        height: area.y,
        scale: node.inverse_scale_factor,
    };
    if view.fitted == Some(key) {
        return;
    }
    view.fit(animation, area);
    view.fitted = Some(key);
}

/// Applies wheel, left-drag, playhead-drag, and right-drag gestures over the
/// timeline.
#[allow(clippy::too_many_arguments)]
fn handle_timeline_input(
    mode: Res<EditorMode>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: MessageReader<MouseWheel>,
    area: Query<(&RelativeCursorPosition, &ComputedNode), With<TimelineArea>>,
    mut editor: ResMut<EditorState>,
    mut view: ResMut<TimelineView>,
    mut left_drag: Local<Option<Vec2>>,
    mut right_drag: Local<Option<Vec2>>,
    mut playhead_drag: Local<bool>,
) {
    if *mode != EditorMode::Animate {
        for _ in wheel.read() {}
        *left_drag = None;
        *right_drag = None;
        *playhead_drag = false;
        return;
    }
    let Some(duration) = selected_animation(&editor).map(|animation| animation.duration) else {
        for _ in wheel.read() {}
        *left_drag = None;
        *right_drag = None;
        *playhead_drag = false;
        return;
    };
    let duration = duration.max(0.0);
    let Ok((relative, node)) = area.single() else {
        for _ in wheel.read() {}
        return;
    };
    let size = logical_size(node);
    if size.x <= 0.0 || size.y <= 0.0 {
        for _ in wheel.read() {}
        return;
    }
    let Some(normalized) = relative.normalized else {
        for _ in wheel.read() {}
        *left_drag = None;
        *right_drag = None;
        *playhead_drag = false;
        return;
    };
    // `normalized` runs from (-0.5, -0.5) at the top-left to (0.5, 0.5) at the
    // bottom-right. Shift the node-local position into track coordinates.
    let local = (normalized + Vec2::splat(0.5)) * size - Vec2::new(TRACK_INSET as f32, 0.0);
    let over = relative.cursor_over();

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);

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

    let playhead = clamp_playhead(editor.playhead, duration);
    let playhead_x = view.screen_x(f64::from(playhead)) as f32;
    let on_playhead = over
        && local.y <= PLAYHEAD_GRAB_HEIGHT
        && (local.x - playhead_x).abs() <= PLAYHEAD_GRAB_HALF_WIDTH;

    if mouse.just_pressed(MouseButton::Left) && over {
        if on_playhead {
            *playhead_drag = true;
            editor.playhead = clamp_playhead(view.time_at(local.x), duration);
        } else {
            *left_drag = Some(local);
        }
    }
    if *playhead_drag {
        if mouse.pressed(MouseButton::Left) {
            editor.playhead = clamp_playhead(view.time_at(local.x), duration);
        } else {
            *playhead_drag = false;
        }
    } else if let Some(last) = *left_drag {
        if mouse.pressed(MouseButton::Left) {
            view.pan(local - last);
            *left_drag = Some(local);
        } else {
            *left_drag = None;
        }
    }

    if mouse.just_pressed(MouseButton::Right) && over {
        *right_drag = Some(local);
    }
    if let Some(last) = *right_drag {
        if mouse.pressed(MouseButton::Right) {
            // Horizontal dragging zooms the time axis and vertical dragging
            // the value axis: right or up zooms in, left or down zooms out.
            let delta = local - last;
            let time_factor = (delta.x * 0.005).exp();
            let value_factor = (-delta.y * 0.005).exp();
            view.zoom_axes_at(local, time_factor, value_factor);
            *right_drag = Some(local);
        } else {
            *right_drag = None;
        }
    }
}

fn draw_timeline(
    mode: Res<EditorMode>,
    editor: Res<EditorState>,
    view: Res<TimelineView>,
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
        );
    }
    *component = UiVelloScene::from(scene);
}

fn draw_frame(
    scene: &mut Scene,
    animation: Option<&Animation>,
    view: &TimelineView,
    size: Vec2,
    playhead: f32,
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
        draw_grid(scene, view, area);

        let groups = curve_groups(animation);
        for (index, group) in groups.iter().enumerate() {
            let color = CHANNEL_COLORS[index % CHANNEL_COLORS.len()];
            if animation.cyclic && duration > 0.0 {
                for cycle in visible_cycles(view, area, duration) {
                    draw_group(scene, group, Some(duration), cycle, color, view);
                }
            } else {
                draw_group(scene, group, None, 0, color, view);
            }
        }

        // Mute the time outside the animation, then mark the playhead on top.
        draw_disabled_spans(scene, view, size, duration);
        draw_playhead(scene, view, size, clamp_playhead(playhead, duration));
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
fn draw_group(
    scene: &mut Scene,
    group: &[&Keyframe],
    period: Option<f32>,
    cycle: i64,
    color: Color,
    view: &TimelineView,
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
        if let (Some(handle), Some(next)) = (keyframe.handle_out, next_time) {
            draw_handle(scene, view, time, value, next, f64::from(handle), true);
        }

        let previous_time = if index > 0 {
            Some(f64::from(group[index - 1].timestamp) + base)
        } else {
            period.map(|period| f64::from(group[count - 1].timestamp) - f64::from(period) + base)
        };
        if let (Some(handle), Some(previous)) = (keyframe.handle_in, previous_time) {
            draw_handle(scene, view, time, value, previous, f64::from(handle), false);
        }

        let center = view.point(time, value);
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
fn draw_handle(
    scene: &mut Scene,
    view: &TimelineView,
    time: f64,
    value: f64,
    neighbour: f64,
    handle_value: f64,
    ahead: bool,
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
        HANDLE_ARM,
        None,
        &arm,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        HANDLE_FILL,
        None,
        &Circle::new(center, HANDLE_RADIUS),
    );
}

/// Lowest and highest value reached by the keyframes and their handles.
fn value_bounds(animation: &Animation) -> (f32, f32) {
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    for keyframe in &animation.keyframes {
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
    use crate::model::{BoneProperty, Keyframe, KeyframeTarget};

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
        view.fit(animation, size);
        view
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
}
