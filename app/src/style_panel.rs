//! Bottom style panel: fill and stroke color pickers, stroke width, and
//! enable toggles.
//!
//! The panel edits the style of the currently selected shape. A path selection
//! resolves to its owning shape, since paths are painted with their parent's
//! style. Values are kept in [`StylePanelState`] and written back into the
//! document when they change, so the canvas refreshes through the usual editor
//! pipeline.
//!
//! Sliders are built from ordinary UI nodes: a track (`Button` +
//! [`RelativeCursorPosition`]) with a fill bar and a thumb. Dragging the track
//! maps the cursor position onto the slider's range.

use bevy::prelude::*;
use bevy::ui::{RelativeCursorPosition, Val2};

use crate::editor::{
    BOTTOM_BAR_HEIGHT, EditorMode, EditorState, SIDEBAR_WIDTH, TOOLBAR_WIDTH, TreeAddress,
    path_closed_at, set_path_closed, style_at, style_at_mut,
};
use crate::model::{DEFAULT_STROKE_WIDTH, Rgba8, StrokeStyle, Style};
use crate::theme;

const TOGGLE_OFF: Color = Color::NONE;

/// Maximum stroke width exposed by the width slider, in document units.
const MAX_STROKE_WIDTH: f32 = 24.0;

/// Value range of the 8-bit color sliders.
const CHANNEL_MAX: f32 = 255.0;

/// Smallest slider movement that counts as an edit, in channel units.
const DIRTY_EPSILON: f32 = 1e-3;

pub struct StylePanelPlugin;

impl Plugin for StylePanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StylePanelState>()
            .init_resource::<ActiveSlider>()
            .add_systems(Startup, spawn_panel)
            .add_systems(
                Update,
                (
                    sync_from_selection,
                    drag_sliders,
                    handle_toggles,
                    handle_closed_checkbox,
                    apply_edits,
                    update_visuals,
                    sync_visibility,
                )
                    .chain(),
            );
    }
}

/// The root of the bottom style panel.
#[derive(Component)]
struct StylePanelRoot;

/// Which of the two color rows a widget belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorTarget {
    Fill,
    Stroke,
}

impl ColorTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Stroke => "Stroke",
        }
    }

    fn channels(self) -> [SliderKind; 4] {
        match self {
            Self::Fill => [
                SliderKind::FillR,
                SliderKind::FillG,
                SliderKind::FillB,
                SliderKind::FillA,
            ],
            Self::Stroke => [
                SliderKind::StrokeR,
                SliderKind::StrokeG,
                SliderKind::StrokeB,
                SliderKind::StrokeA,
            ],
        }
    }
}

/// One editable value in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SliderKind {
    FillR,
    FillG,
    FillB,
    FillA,
    StrokeR,
    StrokeG,
    StrokeB,
    StrokeA,
    StrokeWidth,
}

impl SliderKind {
    fn range(self) -> (f32, f32) {
        match self {
            Self::StrokeWidth => (0.0, MAX_STROKE_WIDTH),
            _ => (0.0, CHANNEL_MAX),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::FillR | Self::StrokeR => "R",
            Self::FillG | Self::StrokeG => "G",
            Self::FillB | Self::StrokeB => "B",
            Self::FillA | Self::StrokeA => "A",
            Self::StrokeWidth => "W",
        }
    }

    fn is_integer(self) -> bool {
        !matches!(self, Self::StrokeWidth)
    }
}

/// A color held in floating point so slider drags stay smooth.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ColorState {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl ColorState {
    const WHITE: Self = Self {
        r: CHANNEL_MAX,
        g: CHANNEL_MAX,
        b: CHANNEL_MAX,
        a: CHANNEL_MAX,
    };

    fn from_rgba8(color: Rgba8) -> Self {
        Self {
            r: f32::from(color.r),
            g: f32::from(color.g),
            b: f32::from(color.b),
            a: f32::from(color.a),
        }
    }

    fn to_rgba8(self) -> Rgba8 {
        Rgba8::rgba(
            channel_round(self.r),
            channel_round(self.g),
            channel_round(self.b),
            channel_round(self.a),
        )
    }

    fn color(self) -> Color {
        Color::srgba(
            self.r / CHANNEL_MAX,
            self.g / CHANNEL_MAX,
            self.b / CHANNEL_MAX,
            self.a / CHANNEL_MAX,
        )
    }
}

fn channel_round(value: f32) -> u8 {
    value.clamp(0.0, CHANNEL_MAX).round() as u8
}

/// Editor-side state of the style panel for the current selection.
#[derive(Resource)]
struct StylePanelState {
    fill: ColorState,
    stroke: ColorState,
    fill_enabled: bool,
    stroke_enabled: bool,
    stroke_width: f32,
    /// Whether the selected node's paths are closed.
    closed: bool,
    /// Whether the selection has a style the panel can edit. Empty selections
    /// do not.
    editable: bool,
    /// The selection the panel currently mirrors; used to detect changes.
    synced: Option<TreeAddress>,
    /// Set when the panel has edits that still need writing to the document.
    dirty: bool,
}

impl Default for StylePanelState {
    fn default() -> Self {
        Self {
            fill: ColorState::WHITE,
            stroke: ColorState::WHITE,
            fill_enabled: false,
            stroke_enabled: false,
            stroke_width: DEFAULT_STROKE_WIDTH,
            closed: false,
            editable: false,
            synced: None,
            dirty: false,
        }
    }
}

impl StylePanelState {
    fn value(&self, kind: SliderKind) -> f32 {
        match kind {
            SliderKind::FillR => self.fill.r,
            SliderKind::FillG => self.fill.g,
            SliderKind::FillB => self.fill.b,
            SliderKind::FillA => self.fill.a,
            SliderKind::StrokeR => self.stroke.r,
            SliderKind::StrokeG => self.stroke.g,
            SliderKind::StrokeB => self.stroke.b,
            SliderKind::StrokeA => self.stroke.a,
            SliderKind::StrokeWidth => self.stroke_width,
        }
    }

    fn set_value(&mut self, kind: SliderKind, value: f32) {
        let (min, max) = kind.range();
        let value = value.clamp(min, max);
        match kind {
            SliderKind::FillR => self.fill.r = value,
            SliderKind::FillG => self.fill.g = value,
            SliderKind::FillB => self.fill.b = value,
            SliderKind::FillA => self.fill.a = value,
            SliderKind::StrokeR => self.stroke.r = value,
            SliderKind::StrokeG => self.stroke.g = value,
            SliderKind::StrokeB => self.stroke.b = value,
            SliderKind::StrokeA => self.stroke.a = value,
            SliderKind::StrokeWidth => self.stroke_width = value,
        }
    }

    fn normalized(&self, kind: SliderKind) -> f32 {
        let (min, max) = kind.range();
        ((self.value(kind) - min) / (max - min)).clamp(0.0, 1.0)
    }

    /// Maps a 0..1 slider position onto the value, marking the panel dirty.
    fn set_normalized(&mut self, kind: SliderKind, t: f32) {
        let (min, max) = kind.range();
        let value = min + t.clamp(0.0, 1.0) * (max - min);
        if (self.value(kind) - value).abs() > DIRTY_EPSILON {
            self.set_value(kind, value);
            self.dirty = true;
        }
    }

    fn display(&self, kind: SliderKind) -> String {
        let value = self.value(kind);
        if kind.is_integer() {
            format!("{}", channel_round(value))
        } else {
            format!("{value:.1}")
        }
    }

    fn color(&self, target: ColorTarget) -> ColorState {
        match target {
            ColorTarget::Fill => self.fill,
            ColorTarget::Stroke => self.stroke,
        }
    }

    fn enabled(&self, target: ColorTarget) -> bool {
        match target {
            ColorTarget::Fill => self.fill_enabled,
            ColorTarget::Stroke => self.stroke_enabled,
        }
    }

    /// Applies the panel's paint to a path style, turning off whichever paint
    /// is disabled.
    fn write_to(&self, style: &mut Style) {
        style.fill = self.fill_enabled.then(|| self.fill.to_rgba8());
        style.stroke = self.stroke_enabled.then(|| StrokeStyle {
            color: self.stroke.to_rgba8(),
            width: self.stroke_width,
        });
    }
}

/// The slider currently being dragged, if any.
#[derive(Resource, Default)]
struct ActiveSlider(Option<SliderKind>);

/// A slider's clickable track.
#[derive(Component, Clone, Copy)]
struct SliderTrack {
    kind: SliderKind,
}

/// A fill or stroke on/off button.
#[derive(Component, Clone, Copy)]
struct ToggleButton {
    target: ColorTarget,
}

/// The button that closes or opens the selected path.
#[derive(Component, Clone, Copy)]
struct ClosedCheckbox;

/// Nodes whose appearance follows the panel state.
#[derive(Component, Clone, Copy)]
enum Visual {
    SliderFill(SliderKind),
    SliderThumb(SliderKind),
    Swatch(ColorTarget),
    ToggleMark(ColorTarget),
    ClosedMark,
}

/// Text nodes whose contents follow the panel state.
#[derive(Component, Clone, Copy)]
enum LabelKind {
    Slider(SliderKind),
    Hex(ColorTarget),
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

fn spawn_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(TOOLBAR_WIDTH),
                bottom: Val::Px(0.0),
                right: Val::Px(SIDEBAR_WIDTH),
                height: Val::Px(BOTTOM_BAR_HEIGHT),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(24.0),
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            StylePanelRoot,
        ))
        .with_children(|panel| {
            spawn_color_group(panel, ColorTarget::Fill);
            spawn_color_group(panel, ColorTarget::Stroke);
            spawn_shape_group(panel);
        });
}

/// Hides the shape style panel while the graphic is locked in rig mode.
fn sync_visibility(mode: Res<EditorMode>, mut panels: Query<&mut Node, With<StylePanelRoot>>) {
    let display = if *mode == EditorMode::Draw {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut panels {
        if node.display != display {
            node.display = display;
        }
    }
}

/// A narrow column holding shape-level options.
fn spawn_shape_group(parent: &mut ChildSpawnerCommands) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            width: Val::Px(96.0),
            row_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|group| {
            group.spawn((
                Text::new("Shape"),
                TextFont::from_font_size(13.0),
                TextColor(theme::TEXT),
                Node {
                    height: Val::Px(24.0),
                    ..default()
                },
            ));

            group
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    height: Val::Px(18.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Button,
                        Node {
                            width: Val::Px(18.0),
                            height: Val::Px(18.0),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(TOGGLE_OFF),
                        BorderColor::all(theme::BORDER),
                        ClosedCheckbox,
                        Visual::ClosedMark,
                    ));
                    row.spawn((
                        Text::new("Closed"),
                        TextFont::from_font_size(13.0),
                        TextColor(theme::TEXT),
                    ));
                });
        });
}

fn spawn_color_group(parent: &mut ChildSpawnerCommands, target: ColorTarget) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            row_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|group| {
            spawn_header(group, target);
            for kind in target.channels() {
                spawn_slider(group, kind);
            }
            if target == ColorTarget::Stroke {
                spawn_slider(group, SliderKind::StrokeWidth);
            }
        });
}

fn spawn_header(row: &mut ChildSpawnerCommands, target: ColorTarget) {
    row.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(8.0),
        height: Val::Px(24.0),
        ..default()
    })
    .with_children(|header| {
        header.spawn((
            Button,
            Node {
                width: Val::Px(18.0),
                height: Val::Px(18.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(TOGGLE_OFF),
            BorderColor::all(theme::BORDER),
            ToggleButton { target },
            Visual::ToggleMark(target),
        ));

        header.spawn((
            Text::new(target.label()),
            TextFont::from_font_size(13.0),
            TextColor(theme::TEXT),
        ));

        // Push the swatch and hex code to the right edge of the column.
        header.spawn(Node {
            flex_grow: 1.0,
            ..default()
        });

        header.spawn((
            Node {
                width: Val::Px(20.0),
                height: Val::Px(20.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(theme::WHITE),
            BorderColor::all(theme::BORDER),
            Visual::Swatch(target),
        ));

        header.spawn((
            Text::new("#ffffff"),
            TextFont::from_font_size(12.0),
            TextColor(theme::MUTED),
            Node {
                width: Val::Px(72.0),
                ..default()
            },
            LabelKind::Hex(target),
        ));
    });
}

fn spawn_slider(row: &mut ChildSpawnerCommands, kind: SliderKind) {
    row.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(8.0),
        height: Val::Px(18.0),
        ..default()
    })
    .with_children(|slider| {
        slider.spawn((
            Text::new(kind.label()),
            TextFont::from_font_size(11.0),
            TextColor(theme::MUTED),
            Node {
                width: Val::Px(12.0),
                ..default()
            },
        ));

        slider
            .spawn((
                Button,
                Node {
                    flex_grow: 1.0,
                    height: Val::Px(12.0),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(theme::TRACK_BG),
                RelativeCursorPosition::default(),
                SliderTrack { kind },
            ))
            .with_children(|track| {
                track.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(0.0),
                        height: Val::Px(12.0),
                        width: Val::Percent(50.0),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(theme::ACCENT_STRONG),
                    Visual::SliderFill(kind),
                ));

                track.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(50.0),
                        top: Val::Percent(50.0),
                        width: Val::Px(12.0),
                        height: Val::Px(12.0),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
                    BackgroundColor(theme::WHITE),
                    Visual::SliderThumb(kind),
                ));
            });

        slider.spawn((
            Text::new("0"),
            TextFont::from_font_size(11.0),
            TextColor(theme::TEXT),
            Node {
                width: Val::Px(30.0),
                ..default()
            },
            LabelKind::Slider(kind),
        ));
    });
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

/// Mirrors the selected shape's style into the panel when the selection
/// changes.
fn sync_from_selection(editor: Res<EditorState>, mut panel: ResMut<StylePanelState>) {
    if panel.synced == editor.selected_node {
        return;
    }
    panel.synced = editor.selected_node.clone();
    panel.dirty = false;

    let Some(address) = editor.selected_node.as_ref() else {
        panel.fill_enabled = false;
        panel.stroke_enabled = false;
        panel.closed = false;
        panel.editable = false;
        return;
    };
    panel.closed = path_closed_at(&editor.document, address).unwrap_or(false);
    let Some(style) = style_at(&editor.document, address) else {
        // A node that no longer exists has nothing to paint.
        panel.fill_enabled = false;
        panel.stroke_enabled = false;
        panel.closed = false;
        panel.editable = false;
        return;
    };
    panel.editable = true;

    // Keep the last color when a paint is currently off, so switching it back
    // on restores a sensible value.
    if let Some(fill) = style.fill {
        panel.fill = ColorState::from_rgba8(fill);
    }
    if let Some(stroke) = style.stroke {
        panel.stroke = ColorState::from_rgba8(stroke.color);
        panel.stroke_width = stroke.width.clamp(0.0, MAX_STROKE_WIDTH);
    }
    panel.fill_enabled = style.fill.is_some();
    panel.stroke_enabled = style.stroke.is_some();
}

/// Turns slider clicks and drags into panel values.
fn drag_sliders(
    mouse: Res<ButtonInput<MouseButton>>,
    mut active: ResMut<ActiveSlider>,
    sliders: Query<(&Interaction, &SliderTrack, &RelativeCursorPosition)>,
    mut panel: ResMut<StylePanelState>,
) {
    if !panel.editable {
        active.0 = None;
        return;
    }
    if mouse.just_pressed(MouseButton::Left) {
        let mut started = None;
        for (interaction, track, _) in &sliders {
            if *interaction == Interaction::Pressed {
                started = Some(track.kind);
                break;
            }
        }
        active.0 = started;
    }

    if mouse.just_released(MouseButton::Left) || !mouse.pressed(MouseButton::Left) {
        active.0 = None;
    }

    let Some(kind) = active.0 else {
        return;
    };
    for (_, track, relative) in &sliders {
        if track.kind == kind
            && let Some(normalized) = relative.normalized
        {
            // `normalized` spans -0.5..0.5 across the track.
            panel.set_normalized(kind, normalized.x + 0.5);
        }
    }
}

/// Toggles fill or stroke on/off.
fn handle_toggles(
    toggles: Query<(&Interaction, &ToggleButton), Changed<Interaction>>,
    mut panel: ResMut<StylePanelState>,
) {
    if !panel.editable {
        return;
    }
    for (interaction, toggle) in &toggles {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match toggle.target {
            ColorTarget::Fill => panel.fill_enabled = !panel.fill_enabled,
            ColorTarget::Stroke => panel.stroke_enabled = !panel.stroke_enabled,
        }
        panel.dirty = true;
    }
}

/// Closes or opens the selected shape's paths from the checkbox.
fn handle_closed_checkbox(
    checkboxes: Query<&Interaction, (With<ClosedCheckbox>, Changed<Interaction>)>,
    mut panel: ResMut<StylePanelState>,
) {
    if !panel.editable {
        return;
    }
    for interaction in &checkboxes {
        if *interaction == Interaction::Pressed {
            panel.closed = !panel.closed;
            panel.dirty = true;
        }
    }
}

/// Writes pending panel edits into the selected shape's style and closed
/// state.
fn apply_edits(mut editor: ResMut<EditorState>, mut panel: ResMut<StylePanelState>) {
    if !panel.dirty {
        return;
    }
    panel.dirty = false;

    let Some(address) = panel.synced.clone() else {
        return;
    };
    if editor.selected_node.as_ref() != Some(&address) {
        return;
    }
    let closed = panel.closed;
    if let Some(style) = style_at_mut(&mut editor.document, &address) {
        panel.write_to(style);
    }
    set_path_closed(&mut editor.document, &address, closed);
}

/// A cheap summary of everything the widgets display, used to avoid per-frame
/// UI churn.
#[derive(Clone, Copy, PartialEq)]
struct PanelSignature {
    fill: ColorState,
    stroke: ColorState,
    fill_enabled: bool,
    stroke_enabled: bool,
    stroke_width: f32,
    closed: bool,
}

/// Reflects the panel values into slider fills, thumbs, swatches, toggles, and
/// value labels.
fn update_visuals(
    panel: Res<StylePanelState>,
    mut last: Local<Option<PanelSignature>>,
    mut visuals: Query<(
        &Visual,
        &mut Node,
        &mut BackgroundColor,
        Option<&mut UiTransform>,
    )>,
    mut labels: Query<(&LabelKind, &mut Text)>,
) {
    let signature = PanelSignature {
        fill: panel.fill,
        stroke: panel.stroke,
        fill_enabled: panel.fill_enabled,
        stroke_enabled: panel.stroke_enabled,
        stroke_width: panel.stroke_width,
        closed: panel.closed,
    };
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    for (visual, mut node, mut background, _transform) in &mut visuals {
        match *visual {
            Visual::SliderFill(kind) => {
                node.width = Val::Percent(panel.normalized(kind) * 100.0);
                background.0 = theme::ACCENT_STRONG;
            }
            Visual::SliderThumb(kind) => {
                node.left = Val::Percent(panel.normalized(kind) * 100.0);
                background.0 = theme::WHITE;
            }
            Visual::Swatch(target) => {
                background.0 = panel.color(target).color();
            }
            Visual::ToggleMark(target) => {
                background.0 = if panel.enabled(target) {
                    theme::ACCENT
                } else {
                    TOGGLE_OFF
                };
            }
            Visual::ClosedMark => {
                background.0 = if panel.closed {
                    theme::ACCENT
                } else {
                    TOGGLE_OFF
                };
            }
        }
    }

    for (label, mut text) in &mut labels {
        match *label {
            LabelKind::Slider(kind) => text.0 = panel.display(kind),
            LabelKind::Hex(target) => text.0 = panel.color(target).to_rgba8().to_hex(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, FORMAT_VERSION, Path, Shape};

    #[test]
    fn slider_positions_map_to_values() {
        let mut panel = StylePanelState::default();

        panel.set_normalized(SliderKind::FillR, 0.5);
        assert!((panel.fill.r - 127.5).abs() < 1e-4);

        panel.set_normalized(SliderKind::StrokeWidth, 0.5);
        assert!((panel.stroke_width - MAX_STROKE_WIDTH * 0.5).abs() < 1e-4);

        // Out-of-range positions are clamped to the slider's bounds.
        panel.set_normalized(SliderKind::FillA, 2.0);
        assert_eq!(panel.fill.a, CHANNEL_MAX);
        panel.set_normalized(SliderKind::StrokeWidth, -1.0);
        assert_eq!(panel.stroke_width, 0.0);

        assert!((panel.normalized(SliderKind::FillR) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn writing_the_panel_toggles_fill_and_stroke() {
        let mut panel = StylePanelState::default();
        panel.fill = ColorState::from_rgba8(Rgba8::rgba(10, 20, 30, 40));
        panel.fill_enabled = true;
        panel.stroke = ColorState::from_rgba8(Rgba8::rgb(1, 2, 3));
        panel.stroke_width = 4.0;
        panel.stroke_enabled = false;

        let mut style = Style::default();
        panel.write_to(&mut style);
        assert_eq!(style.fill, Some(Rgba8::rgba(10, 20, 30, 40)));
        assert_eq!(style.stroke, None);

        panel.stroke_enabled = true;
        panel.write_to(&mut style);
        assert_eq!(
            style.stroke,
            Some(StrokeStyle {
                color: Rgba8::rgb(1, 2, 3),
                width: 4.0,
            })
        );

        panel.fill_enabled = false;
        panel.write_to(&mut style);
        assert_eq!(style.fill, None);
    }

    #[test]
    fn style_accessors_reach_a_shape_and_its_paths() {
        let mut document = Document {
            version: FORMAT_VERSION,
            animations: Vec::new(),
            bones: Vec::new(),
            shapes: vec![Shape {
                name: String::new(),
                paths: vec![
                    Path::new([0.0, 0.0]).line_to([10.0, 10.0]),
                    Path::new([0.0, 0.0]).line_to([5.0, 5.0]),
                ],
                ..default()
            }],
        };

        let shape = TreeAddress::Shape { shapes: vec![0] };
        let path = TreeAddress::Path {
            shapes: vec![0],
            path: 1,
        };
        assert_eq!(style_at(&document, &shape), Some(&Style::default()));

        // A path address resolves to its owning shape's style.
        style_at_mut(&mut document, &path).unwrap().fill = Some(Rgba8::rgb(9, 9, 9));
        assert_eq!(
            style_at(&document, &shape).unwrap().fill,
            Some(Rgba8::rgb(9, 9, 9))
        );
    }
}
