//! Bottom panel for rig mode.
//!
//! Shows the pose controls and, while the weight-paint tool is active, the
//! brush settings.
//!
//! The brush sliders reuse the style panel's approach: a track (`Button` +
//! [`RelativeCursorPosition`]) with a fill bar and a thumb, where the cursor's
//! position along the track maps onto the value's range.

use bevy::prelude::*;
use bevy::ui::{RelativeCursorPosition, Val2};

use crate::editor::{
    BOTTOM_BAR_HEIGHT, EditorMode, EditorState, SIDEBAR_WIDTH, TOOLBAR_WIDTH, Tool, ToolState,
};
use crate::theme;
use crate::weight_paint::{MAX_BRUSH_SIZE, MIN_BRUSH_SIZE, WeightPaintBrush};

pub struct RigPanelPlugin;

impl Plugin for RigPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActiveBrushSlider>()
            .add_systems(Startup, spawn_rig_panel)
            .add_systems(
                Update,
                (
                    sync_visibility,
                    sync_brush_visibility,
                    drag_brush_sliders,
                    update_brush_visuals,
                    handle_reset_pose_button,
                )
                    .chain(),
            );
    }
}

/// The root of the bottom rig panel.
#[derive(Component)]
struct RigPanelRoot;

/// The brush settings column, shown only while weight painting.
#[derive(Component)]
struct BrushControls;

/// The button that returns the skeleton to its rest pose.
#[derive(Component)]
struct ResetPoseButton;

/// One adjustable brush value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrushSliderKind {
    Size,
    Hardness,
    Strength,
}

impl BrushSliderKind {
    fn range(self) -> (f32, f32) {
        match self {
            Self::Size => (MIN_BRUSH_SIZE, MAX_BRUSH_SIZE),
            Self::Hardness | Self::Strength => (0.0, 1.0),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Hardness => "Hard",
            Self::Strength => "Strength",
        }
    }
}

/// The slider currently being dragged, if any.
#[derive(Resource, Default)]
struct ActiveBrushSlider(Option<BrushSliderKind>);

/// A brush slider's clickable track.
#[derive(Component, Clone, Copy)]
struct BrushSliderTrack {
    kind: BrushSliderKind,
}

/// Nodes whose appearance follows the brush state.
#[derive(Component, Clone, Copy)]
enum BrushVisual {
    Fill(BrushSliderKind),
    Thumb(BrushSliderKind),
}

/// Text nodes showing a brush value.
#[derive(Component, Clone, Copy)]
struct BrushLabel(BrushSliderKind);

fn spawn_rig_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(TOOLBAR_WIDTH),
                right: Val::Px(SIDEBAR_WIDTH),
                bottom: Val::Px(0.0),
                height: Val::Px(BOTTOM_BAR_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexStart,
                column_gap: Val::Px(32.0),
                padding: UiRect::all(Val::Px(12.0)),
                display: Display::None,
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            RigPanelRoot,
        ))
        .with_children(|panel| {
            panel
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|column| {
                    column.spawn((
                        Text::new("Pose"),
                        TextFont::from_font_size(14.0),
                        TextColor(theme::MUTED),
                    ));
                    column
                        .spawn((
                            Button,
                            Node {
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                ..default()
                            },
                            BackgroundColor(theme::BUTTON_BG),
                            BorderColor::all(theme::BORDER_FAINT),
                            ResetPoseButton,
                        ))
                        .with_children(|button| {
                            button.spawn((
                                Text::new("Reset Pose"),
                                TextFont::from_font_size(12.0),
                                TextColor(theme::TEXT),
                            ));
                        });
                });

            panel
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        width: Val::Px(280.0),
                        row_gap: Val::Px(6.0),
                        display: Display::None,
                        ..default()
                    },
                    BrushControls,
                ))
                .with_children(|column| {
                    column.spawn((
                        Text::new("Brush"),
                        TextFont::from_font_size(14.0),
                        TextColor(theme::MUTED),
                    ));
                    for kind in [
                        BrushSliderKind::Size,
                        BrushSliderKind::Hardness,
                        BrushSliderKind::Strength,
                    ] {
                        spawn_brush_slider(column, kind);
                    }
                });
        });
}

fn spawn_brush_slider(column: &mut ChildSpawnerCommands, kind: BrushSliderKind) {
    column
        .spawn(Node {
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
                    width: Val::Px(56.0),
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
                    BrushSliderTrack { kind },
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
                        BrushVisual::Fill(kind),
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
                        BrushVisual::Thumb(kind),
                    ));
                });

            slider.spawn((
                Text::new("0"),
                TextFont::from_font_size(11.0),
                TextColor(theme::TEXT),
                Node {
                    width: Val::Px(36.0),
                    ..default()
                },
                BrushLabel(kind),
            ));
        });
}

/// Shows the panel only in rig mode.
fn sync_visibility(mode: Res<EditorMode>, mut panels: Query<&mut Node, With<RigPanelRoot>>) {
    let display = if *mode == EditorMode::Rig {
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

/// Shows the brush controls only while the weight-paint tool is active.
fn sync_brush_visibility(
    mode: Res<EditorMode>,
    tool: Res<ToolState>,
    mut controls: Query<&mut Node, With<BrushControls>>,
) {
    let display = if *mode == EditorMode::Rig && tool.0 == Tool::WeightPaint {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut controls {
        if node.display != display {
            node.display = display;
        }
    }
}

/// Clears the transient pose, returning the skeleton to rest.
fn handle_reset_pose_button(
    buttons: Query<&Interaction, (Changed<Interaction>, With<ResetPoseButton>)>,
    mut editor: ResMut<EditorState>,
) {
    for interaction in &buttons {
        if *interaction == Interaction::Pressed {
            editor.pose.clear();
        }
    }
}

fn brush_value(brush: &WeightPaintBrush, kind: BrushSliderKind) -> f32 {
    match kind {
        BrushSliderKind::Size => brush.size,
        BrushSliderKind::Hardness => brush.hardness,
        BrushSliderKind::Strength => brush.strength,
    }
}

fn set_brush_value(brush: &mut WeightPaintBrush, kind: BrushSliderKind, value: f32) {
    let (min, max) = kind.range();
    let value = value.clamp(min, max);
    match kind {
        BrushSliderKind::Size => brush.size = value,
        BrushSliderKind::Hardness => brush.hardness = value,
        BrushSliderKind::Strength => brush.strength = value,
    }
}

fn brush_normalized(brush: &WeightPaintBrush, kind: BrushSliderKind) -> f32 {
    let (min, max) = kind.range();
    ((brush_value(brush, kind) - min) / (max - min)).clamp(0.0, 1.0)
}

fn brush_display(brush: &WeightPaintBrush, kind: BrushSliderKind) -> String {
    match kind {
        BrushSliderKind::Size => format!("{:.0}", brush.size),
        BrushSliderKind::Hardness | BrushSliderKind::Strength => {
            format!("{:.2}", brush_value(brush, kind))
        }
    }
}

/// Turns brush slider clicks and drags into brush settings.
fn drag_brush_sliders(
    mouse: Res<ButtonInput<MouseButton>>,
    mut active: ResMut<ActiveBrushSlider>,
    sliders: Query<(&Interaction, &BrushSliderTrack, &RelativeCursorPosition)>,
    mut brush: ResMut<WeightPaintBrush>,
) {
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
            let (min, max) = kind.range();
            let value = min + (normalized.x + 0.5).clamp(0.0, 1.0) * (max - min);
            set_brush_value(&mut brush, kind, value);
        }
    }
}

/// Reflects the brush settings into the slider fills, thumbs, and labels.
fn update_brush_visuals(
    brush: Res<WeightPaintBrush>,
    mut last: Local<Option<(f32, f32, f32)>>,
    mut visuals: Query<(&BrushVisual, &mut Node, &mut BackgroundColor)>,
    mut labels: Query<(&BrushLabel, &mut Text)>,
) {
    let signature = (brush.size, brush.hardness, brush.strength);
    if last.as_ref() == Some(&signature) {
        return;
    }
    *last = Some(signature);

    for (visual, mut node, mut background) in &mut visuals {
        match *visual {
            BrushVisual::Fill(kind) => {
                node.width = Val::Percent(brush_normalized(&brush, kind) * 100.0);
                background.0 = theme::ACCENT_STRONG;
            }
            BrushVisual::Thumb(kind) => {
                node.left = Val::Percent(brush_normalized(&brush, kind) * 100.0);
                background.0 = theme::WHITE;
            }
        }
    }

    for (label, mut text) in &mut labels {
        text.0 = brush_display(&brush, label.0);
    }
}
