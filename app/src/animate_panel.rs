//! Bottom panel for animate mode.
//!
//! The top row holds playback controls: a checkbox that shows or hides the
//! skeleton on the canvas, a play/pause button that scrubs the playhead
//! automatically, and a text field for the playback velocity. Below it are the
//! selected animation's duration and cyclic properties. Text fields are edited
//! by clicking the value, typing, then pressing Enter to commit or Escape to
//! cancel. A new duration may delete keyframes that fall outside it.
//!
//! The rest of the panel is the animation timeline, drawn by
//! [`crate::timeline`].

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;

use crate::animate::{
    advance_playhead, commit_animation_rename, selected_animation, set_cyclic, set_duration,
};
use crate::editor::{
    BOTTOM_BAR_HEIGHT, EditorMode, EditorState, SIDEBAR_WIDTH, ShowBones, TOOLBAR_WIDTH,
};
use crate::theme;
use crate::timeline::spawn_timeline;

pub struct AnimatePanelPlugin;

impl Plugin for AnimatePanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DurationEdit>()
            .init_resource::<Playback>()
            .init_resource::<SpeedEdit>()
            .add_systems(Startup, spawn_panel)
            .add_systems(
                Update,
                (
                    sync_visibility,
                    handle_field_press,
                    handle_speed_press,
                    handle_play_pause,
                    handle_bones_toggle,
                    handle_duration_input,
                    handle_speed_input,
                    handle_cyclic_toggle,
                    advance_playback,
                    sync_contents,
                    sync_playback_widgets,
                    paint_buttons,
                )
                    .chain(),
            );
    }
}

/// The root of the bottom animate panel.
#[derive(Component)]
struct AnimatePanelRoot;

/// A part of the panel whose visibility depends on whether an animation is
/// selected.
#[derive(Component, Clone, Copy)]
enum PanelContents {
    /// The duration and cyclic controls, shown when an animation is selected.
    Controls,
    /// The hint shown when no animation is selected.
    EmptyHint,
}

/// The selectable buttons in the panel.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum PanelButton {
    /// The duration text field.
    DurationField,
    /// The cyclic checkbox.
    CyclicToggle,
    /// The playback velocity text field.
    SpeedField,
    /// The play/pause button.
    PlayPause,
    /// The show-bones checkbox.
    BonesToggle,
}

/// The text showing the duration value.
#[derive(Component)]
struct DurationText;

/// The checkbox square inside the cyclic toggle.
#[derive(Component)]
struct CyclicCheck;

/// The text on the play/pause button.
#[derive(Component)]
struct PlayPauseText;

/// The text showing the playback velocity.
#[derive(Component)]
struct SpeedText;

/// The checkbox square inside the bones toggle.
#[derive(Component)]
struct BonesCheck;

/// Playback state for the animate page.
///
/// The playhead itself lives in [`EditorState`]; this resource only stores
/// whether it is moving and how fast.
#[derive(Resource, Debug, PartialEq)]
pub struct Playback {
    /// Whether the playhead is advancing automatically.
    pub playing: bool,
    /// Seconds of animation advanced per second of real time. May be negative.
    pub velocity: f32,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            velocity: 1.0,
        }
    }
}

/// The velocity text edit in progress, if any.
///
/// Other systems consult [`Self::is_editing`] so they do not treat keys typed
/// into the field as shortcuts.
#[derive(Resource, Default)]
pub struct SpeedEdit {
    /// The text typed so far, or `None` when the field does not own the
    /// keyboard.
    buffer: Option<String>,
}

impl SpeedEdit {
    /// Whether the velocity field currently owns the keyboard.
    pub fn is_editing(&self) -> bool {
        self.buffer.is_some()
    }

    /// Abandons the edit in progress, if any.
    pub fn cancel(&mut self) {
        self.buffer = None;
    }
}

/// The text edit in progress, if any.
#[derive(Resource, Default)]
pub struct DurationEdit {
    /// The index of the animation being edited.
    animation: Option<usize>,
    /// The text typed so far.
    buffer: Option<String>,
}

impl DurationEdit {
    /// Whether the field at `index` is being edited.
    fn editing(&self, index: usize) -> bool {
        self.animation == Some(index)
    }

    /// Whether the duration field currently owns the keyboard.
    ///
    /// Other shortcuts consult this so they do not steal keys typed into the
    /// field.
    pub fn is_editing(&self) -> bool {
        self.animation.is_some()
    }

    /// Abandons the edit in progress, if any.
    pub fn cancel(&mut self) {
        self.animation = None;
        self.buffer = None;
    }
}

fn spawn_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(TOOLBAR_WIDTH),
                right: Val::Px(SIDEBAR_WIDTH),
                bottom: Val::Px(0.0),
                height: Val::Px(BOTTOM_BAR_HEIGHT),
                flex_direction: FlexDirection::Column,
                display: Display::None,
                ..default()
            },
            AnimatePanelRoot,
        ))
        .with_children(|panel| {
            // The controls band paints the panel background itself. The
            // timeline node below must stay free of Bevy UI backgrounds: Vello
            // UI scenes are composited beneath Bevy UI, so an opaque
            // background would hide the timeline.
            panel
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect {
                            left: Val::Px(12.0),
                            right: Val::Px(12.0),
                            top: Val::Px(12.0),
                            bottom: Val::Px(10.0),
                        },
                        flex_shrink: 0.0,
                        ..default()
                    },
                    BackgroundColor(theme::PANEL_BG),
                ))
                .with_children(|band| {
                    // Playback controls come first and stay visible even when
                    // no animation is selected, so the bones toggle and speed
                    // can be set before picking one.
                    band.spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(28.0),
                        ..default()
                    })
                    .with_children(spawn_playback_row);

                    band.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(28.0),
                            display: Display::None,
                            ..default()
                        },
                        PanelContents::Controls,
                    ))
                    .with_children(spawn_duration_and_cyclic);

                    band.spawn((
                        Text::new("Select an animation to edit its properties."),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::MUTED),
                        PanelContents::EmptyHint,
                    ));
                });

            spawn_timeline(panel);
        });
}

fn spawn_duration_and_cyclic(controls: &mut ChildSpawnerCommands) {
    // Duration.
    controls
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|group| {
            group.spawn((
                Text::new("Duration"),
                TextFont::from_font_size(12.0),
                TextColor(theme::MUTED),
            ));
            group
                .spawn((
                    Button,
                    PanelButton::DurationField,
                    Node {
                        min_width: Val::Px(76.0),
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(theme::BUTTON_BG),
                    BorderColor::all(theme::BORDER_FAINT),
                ))
                .with_children(|field| {
                    field.spawn((
                        Text::new("0.00"),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::TEXT),
                        DurationText,
                    ));
                });
            group.spawn((
                Text::new("s"),
                TextFont::from_font_size(12.0),
                TextColor(theme::MUTED),
            ));
        });

    // Cyclic.
    spawn_checkbox(controls, PanelButton::CyclicToggle, CyclicCheck, "Cyclic");
}

/// Spawns the playback controls: show-bones, play/pause, and velocity.
fn spawn_playback_row(row: &mut ChildSpawnerCommands) {
    spawn_checkbox(row, PanelButton::BonesToggle, BonesCheck, "Bones");

    row.spawn((
        Button,
        PanelButton::PlayPause,
        Node {
            min_width: Val::Px(64.0),
            padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(theme::BUTTON_BG),
        BorderColor::all(theme::BORDER_FAINT),
    ))
    .with_children(|button| {
        button.spawn((
            Text::new("Play"),
            TextFont::from_font_size(12.0),
            TextColor(theme::TEXT),
            PlayPauseText,
        ));
    });

    // Playback velocity.
    row.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(8.0),
        ..default()
    })
    .with_children(|group| {
        group.spawn((
            Text::new("Speed"),
            TextFont::from_font_size(12.0),
            TextColor(theme::MUTED),
        ));
        group
            .spawn((
                Button,
                PanelButton::SpeedField,
                Node {
                    min_width: Val::Px(76.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(theme::BUTTON_BG),
                BorderColor::all(theme::BORDER_FAINT),
            ))
            .with_children(|field| {
                field.spawn((
                    Text::new("1.00"),
                    TextFont::from_font_size(12.0),
                    TextColor(theme::TEXT),
                    SpeedText,
                ));
            });
        group.spawn((
            Text::new("x"),
            TextFont::from_font_size(12.0),
            TextColor(theme::MUTED),
        ));
    });
}

/// Spawns a labelled checkbox button with the given marker on its square.
fn spawn_checkbox<M: Component>(
    parent: &mut ChildSpawnerCommands,
    button: PanelButton,
    marker: M,
    label: &str,
) {
    parent
        .spawn((
            Button,
            button,
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme::BUTTON_BG),
            BorderColor::all(theme::BORDER_FAINT),
        ))
        .with_children(|toggle| {
            toggle
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                })
                .with_children(|content| {
                    content.spawn((
                        Node {
                            width: Val::Px(14.0),
                            height: Val::Px(14.0),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(3.0)),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        BorderColor::all(theme::BORDER),
                        marker,
                    ));
                    content.spawn((
                        Text::new(label),
                        TextFont::from_font_size(12.0),
                        TextColor(theme::TEXT),
                    ));
                });
        });
}

/// Shows the panel only in animate mode, cancelling any edit in progress and
/// stopping playback when leaving it.
fn sync_visibility(
    mode: Res<EditorMode>,
    mut panels: Query<&mut Node, With<AnimatePanelRoot>>,
    mut edit: ResMut<DurationEdit>,
    mut speed_edit: ResMut<SpeedEdit>,
    mut playback: ResMut<Playback>,
) {
    let display = if *mode == EditorMode::Animate {
        Display::Flex
    } else {
        Display::None
    };
    if *mode != EditorMode::Animate {
        edit.cancel();
        speed_edit.cancel();
        playback.playing = false;
    }
    for mut node in &mut panels {
        if node.display != display {
            node.display = display;
        }
    }
}

/// Starts editing the duration when its field is clicked, or commits an edit
/// already in progress.
fn handle_field_press(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
    mut speed_edit: ResMut<SpeedEdit>,
    mut playback: ResMut<Playback>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::DurationField {
            continue;
        }
        // The field takes the keyboard from any inline rename or the velocity
        // field in progress.
        commit_animation_rename(&mut editor);
        commit_speed(&mut playback, &mut speed_edit);
        let Some(index) = editor.selected_animation else {
            return;
        };
        if edit.editing(index) {
            commit_duration(&mut editor, &mut edit);
        } else {
            let Some(animation) = selected_animation(&editor) else {
                return;
            };
            edit.buffer = Some(format!("{:.2}", animation.duration));
            edit.animation = Some(index);
        }
        return;
    }
}

/// Starts or commits the velocity edit when its field is clicked.
fn handle_speed_press(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut duration_edit: ResMut<DurationEdit>,
    mut speed_edit: ResMut<SpeedEdit>,
    mut playback: ResMut<Playback>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::SpeedField {
            continue;
        }
        if speed_edit.is_editing() {
            commit_speed(&mut playback, &mut speed_edit);
        } else {
            // The velocity field takes the keyboard from the other edits.
            commit_animation_rename(&mut editor);
            commit_duration(&mut editor, &mut duration_edit);
            speed_edit.buffer = Some(format!("{:.2}", playback.velocity));
        }
        return;
    }
}

/// Starts or stops automatic scrubbing from the play/pause button.
fn handle_play_pause(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut speed_edit: ResMut<SpeedEdit>,
    mut playback: ResMut<Playback>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::PlayPause {
            continue;
        }
        commit_speed(&mut playback, &mut speed_edit);
        if playback.playing {
            playback.playing = false;
            return;
        }
        let Some(animation) = selected_animation(&editor) else {
            return;
        };
        let duration = if animation.duration.is_finite() {
            animation.duration.max(0.0)
        } else {
            0.0
        };
        // Playback always begins at one end, depending on its direction.
        editor.playhead = if playback.velocity < 0.0 {
            duration
        } else {
            0.0
        };
        playback.playing = true;
        return;
    }
}

/// Toggles whether the animate canvas draws the skeleton.
fn handle_bones_toggle(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut show_bones: ResMut<ShowBones>,
    mut speed_edit: ResMut<SpeedEdit>,
    mut playback: ResMut<Playback>,
) {
    for (interaction, button) in &buttons {
        if *interaction == Interaction::Pressed && *button == PanelButton::BonesToggle {
            commit_speed(&mut playback, &mut speed_edit);
            show_bones.0 = !show_bones.0;
            return;
        }
    }
}

/// Types into, commits, or cancels the velocity field.
fn handle_speed_input(
    mode: Res<EditorMode>,
    mut events: MessageReader<KeyboardInput>,
    mut playback: ResMut<Playback>,
    mut edit: ResMut<SpeedEdit>,
) {
    if *mode != EditorMode::Animate || edit.buffer.is_none() {
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    if let Some(buffer) = &mut edit.buffer {
        for event in events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::Enter => commit = true,
                KeyCode::Escape => cancel = true,
                KeyCode::Backspace => {
                    buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+') {
                                buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        edit.cancel();
    } else if commit {
        commit_speed(&mut playback, &mut edit);
    }
}

/// Parses and applies the velocity being edited, ignoring invalid input.
fn commit_speed(playback: &mut Playback, edit: &mut SpeedEdit) {
    let Some(text) = edit.buffer.take() else {
        return;
    };
    if let Ok(value) = text.trim().parse::<f32>()
        && value.is_finite()
    {
        playback.velocity = value;
    }
}

/// Moves the playhead every frame while playback is running.
fn advance_playback(
    mode: Res<EditorMode>,
    time: Res<Time>,
    mut playback: ResMut<Playback>,
    mut editor: ResMut<EditorState>,
) {
    if *mode != EditorMode::Animate || !playback.playing {
        return;
    }
    let Some((duration, cyclic)) =
        selected_animation(&editor).map(|animation| (animation.duration, animation.cyclic))
    else {
        playback.playing = false;
        return;
    };
    let (playhead, keep_playing) = advance_playhead(
        editor.playhead,
        playback.velocity,
        time.delta_secs(),
        duration,
        cyclic,
    );
    editor.playhead = playhead;
    if !keep_playing {
        playback.playing = false;
    }
}

/// Types into, commits, or cancels the duration field.
fn handle_duration_input(
    mode: Res<EditorMode>,
    mut events: MessageReader<KeyboardInput>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
) {
    if *mode != EditorMode::Animate || edit.animation.is_none() {
        for _ in events.read() {}
        return;
    }

    let Some(index) = edit.animation else {
        return;
    };
    // A selection change (undo, deletion) abandons the edit.
    if editor.selected_animation != Some(index) {
        edit.cancel();
        for _ in events.read() {}
        return;
    }

    let mut commit = false;
    let mut cancel = false;
    if let Some(buffer) = &mut edit.buffer {
        for event in events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::Enter => commit = true,
                KeyCode::Escape => cancel = true,
                KeyCode::Backspace => {
                    buffer.pop();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+') {
                                buffer.push(ch);
                            }
                        }
                    }
                }
            }
        }
    }

    if cancel {
        edit.cancel();
    } else if commit {
        commit_duration(&mut editor, &mut edit);
    }
}

/// Parses and applies the text being edited, ignoring invalid input.
fn commit_duration(editor: &mut EditorState, edit: &mut DurationEdit) {
    let Some(index) = edit.animation else {
        return;
    };
    let Some(text) = edit.buffer.as_deref() else {
        edit.cancel();
        return;
    };
    if let Ok(value) = text.trim().parse::<f32>()
        && value.is_finite()
    {
        set_duration(editor, index, value);
    }
    edit.cancel();
}

/// Toggles the selected animation's cyclic flag.
fn handle_cyclic_toggle(
    buttons: Query<(&Interaction, &PanelButton), Changed<Interaction>>,
    mut editor: ResMut<EditorState>,
    mut edit: ResMut<DurationEdit>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed || *button != PanelButton::CyclicToggle {
            continue;
        }
        let Some(index) = editor.selected_animation else {
            return;
        };
        // Switching property commits any duration edit first.
        if edit.editing(index) {
            commit_duration(&mut editor, &mut edit);
        }
        let Some(animation) = editor.document.animations.get(index) else {
            return;
        };
        let cyclic = !animation.cyclic;
        set_cyclic(&mut editor, index, cyclic);
        return;
    }
}

/// Shows the controls or the empty hint, and mirrors the selected animation's
/// values into the widgets.
fn sync_contents(
    mode: Res<EditorMode>,
    editor: Res<EditorState>,
    edit: Res<DurationEdit>,
    mut contents: Query<(&mut Node, &PanelContents)>,
    mut texts: Query<&mut Text, With<DurationText>>,
    mut checks: Query<&mut BackgroundColor, With<CyclicCheck>>,
) {
    if *mode != EditorMode::Animate {
        return;
    }
    let selected = selected_animation(&editor).zip(editor.selected_animation);

    for (mut node, contents) in &mut contents {
        let display = match (selected.is_some(), contents) {
            (true, PanelContents::Controls) | (false, PanelContents::EmptyHint) => Display::Flex,
            _ => Display::None,
        };
        if node.display != display {
            node.display = display;
        }
    }

    let Some((animation, index)) = selected else {
        return;
    };

    let label = if edit.editing(index) {
        edit.buffer
            .as_deref()
            .map(|buffer| format!("{buffer}|"))
            .unwrap_or_default()
    } else {
        format!("{:.2}", animation.duration)
    };
    for mut text in &mut texts {
        if text.0 != label {
            text.0 = label.clone();
        }
    }

    let check = if animation.cyclic {
        theme::ACCENT
    } else {
        Color::NONE
    };
    for mut background in &mut checks {
        if background.0 != check {
            background.0 = check;
        }
    }
}

/// Mirrors the playback state into the controls row.
fn sync_playback_widgets(
    mode: Res<EditorMode>,
    playback: Res<Playback>,
    speed_edit: Res<SpeedEdit>,
    show_bones: Res<ShowBones>,
    mut play_texts: Query<&mut Text, (With<PlayPauseText>, Without<SpeedText>)>,
    mut speed_texts: Query<&mut Text, (With<SpeedText>, Without<PlayPauseText>)>,
    mut bones_checks: Query<&mut BackgroundColor, With<BonesCheck>>,
) {
    if *mode != EditorMode::Animate {
        return;
    }

    let play_label = if playback.playing { "Pause" } else { "Play" };
    for mut text in &mut play_texts {
        if text.0 != play_label {
            text.0 = play_label.into();
        }
    }

    let speed_label = match &speed_edit.buffer {
        Some(buffer) => format!("{buffer}|"),
        None => format!("{:.2}", playback.velocity),
    };
    for mut text in &mut speed_texts {
        if text.0 != speed_label {
            text.0 = speed_label.clone();
        }
    }

    let check = if show_bones.0 {
        theme::ACCENT
    } else {
        Color::NONE
    };
    for mut background in &mut bones_checks {
        if background.0 != check {
            background.0 = check;
        }
    }
}

/// Gives the field and toggle hover/active feedback.
fn paint_buttons(
    edit: Res<DurationEdit>,
    speed_edit: Res<SpeedEdit>,
    playback: Res<Playback>,
    editor: Res<EditorState>,
    mut buttons: Query<(&PanelButton, &Interaction, &mut BackgroundColor)>,
) {
    for (button, interaction, mut background) in &mut buttons {
        let active = match button {
            PanelButton::DurationField => editor
                .selected_animation
                .is_some_and(|index| edit.editing(index)),
            PanelButton::SpeedField => speed_edit.is_editing(),
            PanelButton::PlayPause => playback.playing,
            PanelButton::CyclicToggle | PanelButton::BonesToggle => false,
        };
        let color = if active {
            theme::ACCENT_SELECTED
        } else if *interaction != Interaction::None {
            theme::ACCENT_FAINT
        } else {
            theme::BUTTON_BG
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}
