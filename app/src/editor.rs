//! Selection and control-point editing.
//!
//! The rendered document lives on the GPU as a Vello scene, so interaction is
//! done against the CPU-side [`Document`]: clicks are mapped from screen pixels
//! into document space and tested against the path geometry in painting order.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_vello::prelude::{VelloScene2d, vello};
use vello::Scene;
use vello::kurbo::{
    Affine, Circle, CubicBez, Line as KurboLine, ParamCurveNearest, PathSeg, Point as KurboPoint,
    Shape as KurboShape, Stroke as KurboStroke,
};
use vello::peniko::{Color, Fill};

use crate::menu_bar::MenuState;
use crate::model::{
    Bounds, DEFAULT_STROKE_WIDTH, Document, Path, PathEnds, Rgba8, Segment, Shape, StrokeStyle,
    Style,
};
use crate::render::{
    build_scene, path_to_bez_path, shape_fill_path, shape_to_bez_path, to_kurbo_point,
};
use crate::rig::{BoneAddress, BoneRename, RigState};

/// Width of the layer sidebar on the right, in screen pixels.
///
/// The document is fitted into the remaining area to the left of it.
pub const SIDEBAR_WIDTH: f32 = 260.0;

/// Height of the style panel along the bottom, in screen pixels.
///
/// The document is fitted into the remaining area above it.
pub const BOTTOM_BAR_HEIGHT: f32 = 200.0;

/// Width of the tool palette along the left, in screen pixels.
///
/// The document is fitted into the remaining area to the right of it.
pub const TOOLBAR_WIDTH: f32 = 56.0;

/// Height of the menu bar along the top, in screen pixels.
///
/// The document is fitted into the remaining area below it.
pub const MENU_BAR_HEIGHT: f32 = 28.0;

/// Height of the mode tab bar directly below the menu bar, in screen pixels.
pub const TAB_BAR_HEIGHT: f32 = 30.0;

/// Y coordinate where the content below the menu and mode tabs begins.
pub const CONTENT_TOP: f32 = MENU_BAR_HEIGHT + TAB_BAR_HEIGHT;

/// Which editor mode is active.
///
/// Draw mode edits the graphic's rest pose; Rig mode locks the graphic and
/// edits the skeleton over it.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditorMode {
    #[default]
    Draw,
    Rig,
}

/// The active editing tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Select existing paths and drag them or their control points.
    #[default]
    Select,
    /// Draw new paths by clicking, optionally dragging to round the corners.
    Pen,
}

/// Which tool is currently active.
#[derive(Resource, Default)]
pub struct ToolState(pub Tool);

/// Bounds of the loaded document, used to fit it to the window.
///
/// Kept fixed for the session so that dragging a point does not rescale the
/// whole image under the cursor.
#[derive(Resource, Clone, Copy)]
pub struct DocumentBounds(pub Bounds);

/// Marks the entity holding the rendered document.
#[derive(Component)]
pub struct DocumentScene;

/// Pan and zoom applied on top of the automatic fit.
///
/// `pan` is a screen-pixel offset: increasing it moves the document right and
/// down. `zoom` multiplies the fit scale. Both are reset only by restarting
/// the app; the document stays fitted when the window is resized.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub zoom: f32,
    pub pan: Vec2,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

/// Zoom limits for the mouse wheel.
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 32.0;

/// Longest gap between two clicks that still counts as a double-click, in
/// seconds.
const DOUBLE_CLICK_SECONDS: f64 = 0.35;

/// Largest cursor travel between the two clicks of a double-click, in pixels.
const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

/// Fraction of the available canvas area the fitted document occupies.
const FIT_FRACTION: f32 = 0.9;

/// Radius, in screen pixels, within which a click grabs a control point.
const HANDLE_HIT_RADIUS: f32 = 8.0;

/// Extra distance, in screen pixels, within which a click counts as hitting a
/// path outline.
const PATH_HIT_SLACK: f32 = 3.0;

/// Distance, in screen pixels, a shift-drag must travel before it decides
/// which segment to bend.
const BEND_THRESHOLD: f32 = 3.0;

/// Accuracy passed to kurbo's nearest-point search.
const NEAREST_ACCURACY: f64 = 0.01;

/// Multiplier applied per wheel notch when zooming.
const ZOOM_STEP: f32 = 1.1;

/// Control overlay dimensions, in screen pixels.
const OVERLAY_OUTLINE_WIDTH: f32 = 2.0;
const OVERLAY_ARM_WIDTH: f32 = 1.0;
const OVERLAY_HANDLE_RADIUS: f32 = 4.0;
const OVERLAY_SELECTED_HANDLE_RADIUS: f32 = 5.5;
const OVERLAY_HANDLE_OUTLINE_WIDTH: f32 = 1.5;
const RUBBER_BAND_WIDTH: f32 = 1.5;

/// Default paint for a newly drawn pen path.
const PEN_STROKE_COLOR: Rgba8 = Rgba8::rgb(0xdd, 0xe1, 0xe8);

/// Control overlay colors, matching the UI accent.
const OVERLAY_ACCENT: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0xff);
const OVERLAY_BAND: Color = Color::from_rgba8(0x4c, 0xc9, 0xf0, 0x80);
const OVERLAY_HANDLE_FILL: Color = Color::from_rgba8(0xff, 0xff, 0xff, 0xff);
const OVERLAY_HANDLE_OUTLINE: Color = Color::from_rgba8(0x0b, 0x2a, 0x40, 0xff);

/// A node selected for editing, addressed by tree position.
///
/// `shapes` is a chain of shape indices: `[i]` addresses
/// `document.shapes[i]`, `[i, j]` addresses its `children[j]`, and so on. A
/// [`TreeAddress::Shape`] selects a whole shape together with all of its
/// paths and nested children; a [`TreeAddress::Path`] selects one contour
/// among that shape's direct paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeAddress {
    /// A whole shape, including its paths and children.
    Shape { shapes: Vec<usize> },
    /// One of the paths directly owned by the shape named by `shapes`.
    Path { shapes: Vec<usize>, path: usize },
}

impl TreeAddress {
    /// The chain of shape indices leading to the addressed node.
    fn shape_chain(&self) -> &[usize] {
        match self {
            Self::Shape { shapes } | Self::Path { shapes, .. } => shapes,
        }
    }
}

/// The control point of the selected node that is currently highlighted.
///
/// When the selection is a whole shape, this also names which of its direct
/// paths the point belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedNode {
    /// Index into the selected shape's [`crate::model::Shape::paths`].
    pub path: usize,
    pub handle: Handle,
}

/// An in-progress inline rename of a tree node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameState {
    /// The node whose name is being edited.
    pub address: TreeAddress,
    /// The text typed so far.
    pub buffer: String,
}

/// A draggable point of a [`Path`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The start anchor of an open path.
    Start,
    /// The end anchor of segment `index`.
    Anchor(usize),
    /// The Bézier control point near the start of segment `index`.
    Ctrl1(usize),
    /// The Bézier control point near the end of segment `index`.
    Ctrl2(usize),
}

#[derive(Clone, Debug)]
enum Drag {
    /// Dragging one control point of a path.
    Handle {
        address: TreeAddress,
        handle: Handle,
        /// Shift was held: keep the neighbouring handle symmetric around the
        /// shared anchor.
        symmetric: bool,
    },
    /// Shift-dragging an anchor instantiate a missing control point there.
    /// `target` is chosen from the drag direction on the first movement.
    Bend {
        address: TreeAddress,
        handle: Handle,
        target: Option<BendTarget>,
    },
    /// Dragging a whole shape by its body. The last document-space point is
    /// remembered so each frame only applies the incremental movement.
    Object {
        address: TreeAddress,
        last: [f32; 2],
    },
}

/// A line segment that a bend drag will turn into a cubic.
///
/// The index is the segment in `Path::segments`. `Incoming` bends the segment
/// that ends at the dragged anchor; `Outgoing` bends the one that starts there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BendTarget {
    Incoming(usize),
    Outgoing(usize),
}

/// Which control point of a cubic segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtrlSlot {
    First,
    Second,
}

/// A path being drawn with the pen tool.
///
/// Finishing the draft wraps the path in a brand-new top-level shape.
#[derive(Clone, Debug)]
struct PenDraft {
    path: Path,
    /// Outgoing control of the most recent anchor, carried into the next
    /// segment so smooth drags continue smoothly.
    pending_out: Option<[f32; 2]>,
    /// True while the mouse is held after placing an anchor (shaping handles).
    dragging: bool,
}

impl PenDraft {
    /// Shapes smooth handles around, and mirrors them across, the most recently
    /// placed anchor. `point` is the current cursor position in document space.
    fn shape_last_anchor(&mut self, point: [f32; 2]) {
        let Some(index) = self.path.segments.len().checked_sub(1) else {
            return;
        };
        let anchor = self.path.segments[index].to;
        let delta = offset(anchor, point);
        if delta == [0.0, 0.0] {
            return;
        }

        // The incoming control mirrors the outgoing one, which is carried into
        // the next segment. The start-side control is left as it is (omitted
        // controls already lie on the previous anchor).
        self.path.segments[index].ctrl2 = Some(translate(anchor, [-delta[0], -delta[1]]));
        self.pending_out = Some(translate(anchor, delta));
    }
}

/// Editor state: the mutable document plus the current selection.
#[derive(Resource)]
pub struct EditorState {
    pub document: Document,
    /// The selected node, either a whole shape or one of its paths.
    pub selected: Option<TreeAddress>,
    /// The selected control point of [`Self::selected`], if any.
    pub selected_handle: Option<SelectedNode>,
    /// Incremented whenever the object tree changes shape (an item is removed
    /// or moved), so the sidebar knows to rebuild its rows.
    pub structure_revision: u64,
    /// The tree node whose name is being edited inline, if any.
    pub renaming: Option<RenameState>,
    /// The selected bone in rig mode, if any.
    pub selected_bone: Option<BoneAddress>,
    /// The bone whose name is being edited inline, if any.
    pub bone_renaming: Option<BoneRename>,
    drag: Option<Drag>,
    /// The in-progress pen path, if any.
    pen: Option<PenDraft>,
    /// When and where the last canvas click happened, for double-click
    /// detection.
    last_click: Option<(f64, Vec2)>,
}

impl EditorState {
    pub fn new(document: Document) -> Self {
        Self {
            document,
            selected: None,
            selected_handle: None,
            structure_revision: 0,
            renaming: None,
            selected_bone: None,
            bone_renaming: None,
            drag: None,
            pen: None,
            last_click: None,
        }
    }

    /// Selects `address` (or clears the selection when it is `None`).
    ///
    /// Any in-progress drag, control-point selection, and rename are dropped,
    /// so the selection always starts from a clean state.
    pub fn select(&mut self, address: Option<TreeAddress>) {
        self.selected = address;
        self.selected_handle = None;
        self.renaming = None;
        self.drag = None;
    }

    /// Selects a bone (or clears the bone selection).
    pub fn select_bone(&mut self, address: Option<BoneAddress>) {
        self.selected_bone = address;
        self.bone_renaming = None;
    }

    /// Whether any inline rename (shape or bone) is in progress.
    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some() || self.bone_renaming.is_some()
    }

    /// Starts renaming the node at `address`, seeding the text field with its
    /// current name.
    pub fn begin_rename(&mut self, address: TreeAddress) {
        let buffer = name_at(&self.document, &address)
            .unwrap_or_default()
            .to_owned();
        self.selected = Some(address.clone());
        self.selected_handle = None;
        self.drag = None;
        self.renaming = Some(RenameState { address, buffer });
    }

    /// Swaps in a whole new document, dropping all transient editing state.
    ///
    /// Used for new, load, and undo/redo. The structure revision is bumped so
    /// views that mirror the object tree rebuild even when the selection does
    /// not change.
    pub fn replace_document(&mut self, document: Document) {
        self.document = document;
        self.selected = None;
        self.selected_handle = None;
        self.renaming = None;
        self.selected_bone = None;
        self.bone_renaming = None;
        self.drag = None;
        self.pen = None;
        self.last_click = None;
        self.structure_revision += 1;
    }
}

/// System set for canvas pointer handling.
///
/// UI systems that own the pointer (such as an open menu) run after this set
/// so that canvas input still sees the UI state from the start of the frame.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanvasInput;

/// Systems for selection, dragging, fitting, and redrawing.
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ToolState>()
            .init_resource::<Viewport>()
            .init_resource::<EditorMode>()
            .add_systems(
                Update,
                (
                    handle_viewport_input,
                    handle_input.in_set(CanvasInput),
                    refresh_scene,
                    fit_document,
                )
                    .chain(),
            );
    }
}

/// Centers the document in the area right of the toolbar, left of the sidebar,
/// below the menu bar and mode tabs, and above the style panel, scaling it to
/// fill 90% of that area.
pub fn fit_transform(bounds: Bounds, viewport_width: f32, viewport_height: f32) -> Transform {
    // The menu bar, mode tabs, toolbar, sidebar, and style panel cover the
    // edges, so fit into what remains.
    let available_width = (viewport_width - SIDEBAR_WIDTH - TOOLBAR_WIDTH).max(1.0);
    let available_height = (viewport_height - CONTENT_TOP - BOTTOM_BAR_HEIGHT).max(1.0);
    let doc_width = bounds.width().max(f32::EPSILON);
    let doc_height = bounds.height().max(f32::EPSILON);
    let scale = FIT_FRACTION * (available_width / doc_width).min(available_height / doc_height);

    // Document Y points down, so shifting the document's centre to the content
    // area's centre means moving the entity *up* by `scale * centre.y`. That
    // centre sits between the toolbar and the sidebar, half of the top bars
    // below the top edge, and half a style panel above the bottom edge.
    let center = bounds.center();
    Transform::from_xyz(
        -scale * center[0] + (TOOLBAR_WIDTH - SIDEBAR_WIDTH) * 0.5,
        scale * center[1] + (BOTTOM_BAR_HEIGHT - CONTENT_TOP) * 0.5,
        0.0,
    )
    .with_scale(Vec3::splat(scale))
}

/// The scene transform for the current pan and zoom on top of the automatic
/// fit.
fn view_transform(bounds: Bounds, width: f32, height: f32, viewport: &Viewport) -> Transform {
    let base = fit_transform(bounds, width, height);
    Transform::from_xyz(
        base.translation.x + viewport.pan.x,
        base.translation.y - viewport.pan.y,
        0.0,
    )
    .with_scale(Vec3::splat(base.scale.x * viewport.zoom))
}

fn fit_document(
    window: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    viewport: Res<Viewport>,
    mut scenes: Query<&mut Transform, With<DocumentScene>>,
) {
    let Ok(window) = window.single() else {
        return;
    };
    let transform = view_transform(bounds.0, window.width(), window.height(), &viewport);
    for mut scene_transform in &mut scenes {
        *scene_transform = transform;
    }
}

/// Applies mouse-wheel panning and zooming to the [`Viewport`].
///
/// Plain scrolling pans vertically, shift-scrolling horizontally, and
/// control-scrolling zooms around the cursor.
fn handle_viewport_input(
    mut wheel: MessageReader<MouseWheel>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    mut viewport: ResMut<Viewport>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);

    let width = window.width();
    let height = window.height();

    for event in wheel.read() {
        let conversion = MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR;
        let (dx, dy) = match event.unit {
            MouseScrollUnit::Line => (event.x * conversion, event.y * conversion),
            MouseScrollUnit::Pixel => (event.x, event.y),
        };

        if ctrl {
            let anchor = window
                .cursor_position()
                .unwrap_or(Vec2::new(width * 0.5, height * 0.5));
            let factor = ZOOM_STEP.powf(dy / conversion);
            zoom_around(&mut viewport, bounds.0, width, height, anchor, factor);
        } else if shift {
            // A vertical wheel becomes horizontal panning.
            let horizontal = if dx != 0.0 { dx } else { dy };
            viewport.pan.x += horizontal;
        } else {
            viewport.pan += Vec2::new(dx, dy);
        }
    }
}

/// Multiplies the zoom by `factor`, keeping the document point under `anchor`
/// pinned to that screen position.
fn zoom_around(
    view: &mut Viewport,
    bounds: Bounds,
    width: f32,
    height: f32,
    anchor: Vec2,
    factor: f32,
) {
    let zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    if (zoom - view.zoom).abs() <= f32::EPSILON {
        return;
    }

    let current = ViewMapping::new(bounds, width, height, view);
    let document = current.screen_to_document(anchor);

    // Solve for the pan that keeps `document` under `anchor` at the new zoom.
    let base = fit_transform(bounds, width, height);
    let scale = base.scale.x * zoom;
    view.zoom = zoom;
    view.pan = Vec2::new(
        anchor.x - width * 0.5 - base.translation.x - scale * document[0],
        anchor.y - height * 0.5 + base.translation.y - scale * document[1],
    );
}

/// Maps between window pixels and document coordinates for the current fit.
#[derive(Clone, Copy)]
pub(crate) struct ViewMapping {
    scale: f32,
    tx: f32,
    ty: f32,
    half_width: f32,
    half_height: f32,
}

impl ViewMapping {
    pub(crate) fn new(bounds: Bounds, width: f32, height: f32, viewport: &Viewport) -> Self {
        let transform = view_transform(bounds, width, height, viewport);
        Self {
            scale: transform.scale.x,
            tx: transform.translation.x,
            ty: transform.translation.y,
            half_width: width * 0.5,
            half_height: height * 0.5,
        }
    }

    pub(crate) fn screen_to_document(self, screen: Vec2) -> [f32; 2] {
        [
            (screen.x - self.half_width - self.tx) / self.scale,
            (screen.y - self.half_height + self.ty) / self.scale,
        ]
    }

    /// Length of one screen pixel in document units.
    pub(crate) fn document_per_pixel(self) -> f32 {
        1.0 / self.scale
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    bounds: Res<DocumentBounds>,
    viewport: Res<Viewport>,
    mode: Res<EditorMode>,
    mut tool: ResMut<ToolState>,
    menu: Option<Res<MenuState>>,
    mut editor: ResMut<EditorState>,
) {
    // Rig mode locks the graphic. Bone editing has its own input system.
    if *mode != EditorMode::Draw {
        return;
    }

    // An open dropdown owns the pointer: clicks on its rows run menu actions
    // and clicks elsewhere close it, but none of them may edit the canvas.
    if menu.is_some_and(|menu| menu.is_open()) {
        if mouse.just_released(MouseButton::Left) {
            editor.drag = None;
        }
        return;
    }

    // While a tree row is being renamed, the keyboard belongs to the text
    // field. A click elsewhere commits the edit and then behaves normally.
    if editor.is_renaming() {
        if !mouse.just_pressed(MouseButton::Left) {
            return;
        }
        commit_rename(&mut editor);
    }

    // Escape leaves the pen tool, abandoning a half-drawn path rather than
    // finishing it.
    if tool.0 == Tool::Pen && keys.just_pressed(KeyCode::Escape) {
        editor.pen = None;
        editor.drag = None;
        tool.0 = Tool::Select;
        return;
    }

    match tool.0 {
        Tool::Select => handle_select_input(
            &mouse,
            &keys,
            &time,
            &windows,
            &bounds,
            &viewport,
            &mut editor,
        ),
        Tool::Pen => handle_pen_input(&mouse, &keys, &windows, &bounds, &viewport, &mut editor),
    }
}

/// True while either shift key is held.
fn shift_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

/// True when the cursor is over one of the editor's panels rather than the
/// canvas.
pub(crate) fn over_ui(cursor: Vec2, window: &Window) -> bool {
    cursor.x < TOOLBAR_WIDTH
        || cursor.x >= window.width() - SIDEBAR_WIDTH
        || cursor.y < CONTENT_TOP
        || cursor.y >= window.height() - BOTTOM_BAR_HEIGHT
}

fn handle_select_input(
    mouse: &ButtonInput<MouseButton>,
    keys: &ButtonInput<KeyCode>,
    time: &Time,
    windows: &Query<&Window, With<PrimaryWindow>>,
    bounds: &DocumentBounds,
    viewport: &Viewport,
    editor: &mut EditorState,
) {
    if keys.just_pressed(KeyCode::Escape) {
        editor.selected = None;
        editor.selected_handle = None;
        editor.drag = None;
        return;
    }

    if keys.just_pressed(KeyCode::Delete) {
        delete_selection(editor);
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    let view = ViewMapping::new(bounds.0, window.width(), window.height(), viewport);
    let point = view.screen_to_document(cursor);

    if over_ui(cursor, window) {
        // The UI handles clicks here. Still finish a drag that was released
        // over a panel.
        if mouse.just_released(MouseButton::Left) {
            editor.drag = None;
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        // A double-click on the outline of a path inserts a new anchor there,
        // subdivided so the path keeps its shape.
        let now = time.elapsed_secs_f64();
        let double_click = editor.last_click.is_some_and(|(last_time, last_cursor)| {
            now - last_time <= DOUBLE_CLICK_SECONDS
                && cursor.distance(last_cursor) <= DOUBLE_CLICK_DISTANCE
        });
        if double_click
            && insert_point(editor, point, view.document_per_pixel() * HANDLE_HIT_RADIUS)
        {
            editor.last_click = None;
            return;
        }
        editor.last_click = Some((now, cursor));

        // Control points of the current selection take priority over picking a
        // different path, otherwise clicking a handle next to another shape
        // would re-select that shape.
        let shift = shift_held(keys);
        let mut started_drag = false;
        if let Some(address) = editor.selected.clone()
            && let Some((node, node_address, path)) = node_under(
                &editor.document,
                &address,
                point,
                view.document_per_pixel() * HANDLE_HIT_RADIUS,
            )
        {
            let handle = node.handle;
            let endpoint = matches!(handle, Handle::Start | Handle::Anchor(_));
            let bending = shift && endpoint && !bend_candidates(path, handle).is_empty();
            editor.drag = Some(if bending {
                Drag::Bend {
                    address: node_address,
                    handle,
                    target: None,
                }
            } else {
                Drag::Handle {
                    address: node_address,
                    handle,
                    symmetric: shift,
                }
            });
            editor.selected_handle = Some(node);
            started_drag = true;
        }

        if !started_drag {
            // Pressing a shape selects it and starts moving the whole thing;
            // pressing empty space clears the selection.
            let picked = pick_shape(
                &editor.document,
                editor.selected.as_ref(),
                point,
                view.document_per_pixel() * PATH_HIT_SLACK,
            );
            editor.selected_handle = None;
            editor.drag = picked.clone().map(|address| Drag::Object {
                address,
                last: point,
            });
            editor.selected = picked;
        }
    }

    if let Some(drag) = editor.drag.clone() {
        if mouse.pressed(MouseButton::Left) {
            match drag {
                Drag::Handle {
                    address,
                    handle,
                    symmetric,
                } => {
                    if let Some(path) = path_at_mut(&mut editor.document, &address) {
                        set_handle(path, handle, point);
                        if symmetric {
                            symmetrize(path, handle, point);
                        }
                    }
                }
                Drag::Bend {
                    address,
                    handle,
                    target,
                } => {
                    let threshold = view.document_per_pixel() * BEND_THRESHOLD;
                    let mut decided = target;
                    if let Some(path) = path_at_mut(&mut editor.document, &address) {
                        if let Some(target) = target {
                            // Already bending: keep the handle under the cursor.
                            apply_bend(path, target, point);
                            decided = Some(target);
                        } else {
                            // Choose which of the lines meeting here to bend
                            // once the drag has a clear direction.
                            let anchor = endpoint_anchor(path, handle).unwrap_or(point);
                            if distance(anchor, point) >= threshold
                                && let Some(target) =
                                    choose_bend(path, handle, offset(anchor, point))
                                && apply_bend(path, target, point)
                            {
                                decided = Some(target);
                            }
                        }
                    }
                    if decided != target {
                        editor.drag = Some(Drag::Bend {
                            address,
                            handle,
                            target: decided,
                        });
                    }
                }
                Drag::Object { address, last } => {
                    let delta = offset(last, point);
                    if delta != [0.0, 0.0]
                        && let Some(shape) = shape_at_mut(&mut editor.document, &address)
                    {
                        translate_shape(shape, delta);
                    }
                    editor.drag = Some(Drag::Object {
                        address,
                        last: point,
                    });
                }
            }
        }
        if mouse.just_released(MouseButton::Left) {
            editor.drag = None;
        }
    }
}

fn handle_pen_input(
    mouse: &ButtonInput<MouseButton>,
    keys: &ButtonInput<KeyCode>,
    windows: &Query<&Window, With<PrimaryWindow>>,
    bounds: &DocumentBounds,
    viewport: &Viewport,
    editor: &mut EditorState,
) {
    // Escape is handled by the caller, which also switches to the select tool.

    if keys.just_pressed(KeyCode::Delete) {
        // Drawing does not disable editing: Delete still removes the selected
        // point or object (for example, the shape the pen just finished).
        delete_selection(editor);
        return;
    }

    if keys.just_pressed(KeyCode::Enter) {
        finish_pen(editor, true);
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    let view = ViewMapping::new(bounds.0, window.width(), window.height(), viewport);
    let point = view.screen_to_document(cursor);
    let slack = view.document_per_pixel() * HANDLE_HIT_RADIUS;

    if over_ui(cursor, window) {
        if mouse.just_released(MouseButton::Left)
            && let Some(draft) = &mut editor.pen
        {
            draft.dragging = false;
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        if editor.pen.is_none() {
            editor.pen = Some(PenDraft {
                path: Path::new(point),
                pending_out: None,
                dragging: false,
            });
            editor.selected = None;
            editor.selected_handle = None;
        } else if pen_should_close(editor, point, slack) {
            if let Some(draft) = &mut editor.pen {
                draft.path.close();
            }
            finish_pen(editor, true);
            return;
        } else if let Some(draft) = &mut editor.pen {
            // The next segment starts from the last anchor and uses the
            // outgoing control left by a previous drag, if any.
            draft.path.segments.push(Segment {
                ctrl1: draft.pending_out.take(),
                ctrl2: None,
                to: point,
            });
            draft.dragging = true;
        }
    }

    // While the mouse is held after placing an anchor, dragging shapes smooth
    // handles around it.
    if mouse.pressed(MouseButton::Left)
        && let Some(draft) = &mut editor.pen
        && draft.dragging
    {
        draft.shape_last_anchor(point);
    }

    if mouse.just_released(MouseButton::Left)
        && let Some(draft) = &mut editor.pen
    {
        draft.dragging = false;
    }
}

fn refresh_scene(
    editor: Res<EditorState>,
    bounds: Res<DocumentBounds>,
    viewport: Res<Viewport>,
    mode: Res<EditorMode>,
    rig: Res<RigState>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut scenes: Query<&mut VelloScene2d, With<DocumentScene>>,
) {
    let window = windows.single().ok();
    let view =
        window.map(|window| ViewMapping::new(bounds.0, window.width(), window.height(), &viewport));
    let scale = view.map(|view| view.scale).unwrap_or(1.0);

    let mut scene = build_scene(&editor.document);

    if *mode == EditorMode::Draw {
        if let Some(address) = &editor.selected {
            // A whole-shape selection shows the points of every path it owns.
            // Only the path holding the selected point highlights it.
            for (index, path) in selection_paths(&editor.document, address) {
                let selected_handle = editor
                    .selected_handle
                    .filter(|node| node.path == index)
                    .map(|node| node.handle);
                draw_control_overlay(&mut scene, path, scale, selected_handle);
            }
        }

        if let Some(draft) = &editor.pen {
            draw_control_overlay(&mut scene, &draft.path, scale, None);

            // Rubber band from the last anchor to the cursor while the next
            // point is being chosen.
            if !draft.dragging
                && let (Some(view), Some(window)) = (view, window)
                && let Some(cursor) = window.cursor_position()
                && !over_ui(cursor, window)
            {
                stroke_segment(
                    &mut scene,
                    path_end(&draft.path),
                    view.screen_to_document(cursor),
                    f64::from(RUBBER_BAND_WIDTH / scale.max(f32::EPSILON)),
                    &OVERLAY_BAND,
                );
            }
        }
    } else {
        crate::rig::draw_bone_overlay(
            &mut scene,
            &editor.document,
            editor.selected_bone.as_ref(),
            rig.preview,
            scale,
        );
    }

    if let Ok(mut scene_component) = scenes.single_mut() {
        *scene_component = VelloScene2d::from(scene);
    }
}

// ---------------------------------------------------------------------------
// Tree addressing
// ---------------------------------------------------------------------------

/// Resolves a chain of child indices to the nested list of shapes it names.
fn shapes_at<'a>(mut shapes: &'a [Shape], indices: &[usize]) -> Option<&'a [Shape]> {
    for &index in indices {
        shapes = &shapes.get(index)?.children;
    }
    Some(shapes)
}

fn shapes_at_mut<'a>(mut shapes: &'a mut [Shape], indices: &[usize]) -> Option<&'a mut [Shape]> {
    for &index in indices {
        shapes = &mut shapes.get_mut(index)?.children;
    }
    Some(shapes)
}

/// Resolves a chain of child indices to the mutable list that contains the last
/// index, so the addressed child can be removed or inserted.
fn shapes_vec_at_mut<'a>(
    document: &'a mut Document,
    indices: &[usize],
) -> Option<&'a mut Vec<Shape>> {
    let mut shapes = &mut document.shapes;
    for &index in indices {
        shapes = &mut shapes.get_mut(index)?.children;
    }
    Some(shapes)
}

/// Resolves a chain of shape indices to the shape it names.
fn shape_at_chain<'a>(document: &'a Document, chain: &[usize]) -> Option<&'a Shape> {
    let (last, parents) = chain.split_last()?;
    shapes_at(&document.shapes, parents)?.get(*last)
}

fn shape_at_chain_mut<'a>(document: &'a mut Document, chain: &[usize]) -> Option<&'a mut Shape> {
    let (last, parents) = chain.split_last()?;
    shapes_at_mut(&mut document.shapes, parents)?.get_mut(*last)
}

/// Resolves an address to the shape that owns it.
///
/// Paths are owned by their enclosing shape, so both variants resolve to a
/// [`Shape`].
pub fn shape_at<'a>(document: &'a Document, address: &TreeAddress) -> Option<&'a Shape> {
    shape_at_chain(document, address.shape_chain())
}

pub fn shape_at_mut<'a>(
    document: &'a mut Document,
    address: &TreeAddress,
) -> Option<&'a mut Shape> {
    shape_at_chain_mut(document, address.shape_chain())
}

/// Resolves a path address to the path it names.
fn path_at<'a>(document: &'a Document, address: &TreeAddress) -> Option<&'a Path> {
    let TreeAddress::Path { shapes, path } = address else {
        return None;
    };
    shape_at_chain(document, shapes)?.paths.get(*path)
}

fn path_at_mut<'a>(document: &'a mut Document, address: &TreeAddress) -> Option<&'a mut Path> {
    let TreeAddress::Path { shapes, path } = address else {
        return None;
    };
    shape_at_chain_mut(document, shapes)?.paths.get_mut(*path)
}

/// The paths the addressed node exposes for editing.
///
/// A whole shape exposes all of its direct paths; a path address exposes just
/// that one.
fn selection_paths<'a>(document: &'a Document, address: &TreeAddress) -> Vec<(usize, &'a Path)> {
    match address {
        TreeAddress::Shape { shapes } => shape_at_chain(document, shapes)
            .map(|shape| shape.paths.iter().enumerate().collect())
            .unwrap_or_default(),
        TreeAddress::Path {
            shapes,
            path: index,
        } => shape_at_chain(document, shapes)
            .and_then(|shape| shape.paths.get(*index))
            .map(|owned| vec![(*index, owned)])
            .unwrap_or_default(),
    }
}

/// Resolves an address to the style applied to the shape it names.
///
/// Paths paint with their owner's style, so a path address resolves to the
/// style of its enclosing shape.
pub fn style_at<'a>(document: &'a Document, address: &TreeAddress) -> Option<&'a Style> {
    Some(&shape_at(document, address)?.style)
}

/// Resolves an address to the mutable style applied to the shape it names.
pub fn style_at_mut<'a>(
    document: &'a mut Document,
    address: &TreeAddress,
) -> Option<&'a mut Style> {
    Some(&mut shape_at_mut(document, address)?.style)
}

/// Whether the addressed path is closed.
///
/// A whole shape reports whether all of its paths are closed, and `None` when
/// it has no paths.
pub fn path_closed_at(document: &Document, address: &TreeAddress) -> Option<bool> {
    match address {
        TreeAddress::Path { .. } => path_at(document, address).map(Path::is_closed),
        TreeAddress::Shape { shapes } => {
            let shape = shape_at_chain(document, shapes)?;
            (!shape.paths.is_empty()).then(|| shape.paths.iter().all(Path::is_closed))
        }
    }
}

/// Closes or opens the path named by `address`.
///
/// A path address changes only that path; a shape address changes every one of
/// its direct paths. Closing adds a straight segment back to the start and
/// opening removes it. Returns whether anything was changed.
pub fn set_path_closed(document: &mut Document, address: &TreeAddress, closed: bool) -> bool {
    match address {
        TreeAddress::Path { .. } => match path_at_mut(document, address) {
            Some(path) => {
                if closed {
                    path.close()
                } else {
                    path.open()
                }
            }
            None => false,
        },
        TreeAddress::Shape { shapes } => {
            let Some(shape) = shape_at_chain_mut(document, shapes) else {
                return false;
            };
            let mut changed = false;
            for path in &mut shape.paths {
                changed |= if closed { path.close() } else { path.open() };
            }
            changed
        }
    }
}

// ---------------------------------------------------------------------------
// Tree editing
// ---------------------------------------------------------------------------

/// A shape's position in the document tree: the chain of shape indices leading
/// from the root down to the shape itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShapeSlot {
    chain: Vec<usize>,
}

impl ShapeSlot {
    /// Indices of the shapes that contain this one.
    fn parents(&self) -> &[usize] {
        &self.chain[..self.chain.len() - 1]
    }

    /// The shape's index inside its container.
    fn index(&self) -> usize {
        *self.chain.last().expect("a shape slot is never empty")
    }
}

/// Resolves a shape address to its position, validating every index.
fn shape_slot(document: &Document, address: &TreeAddress) -> Option<ShapeSlot> {
    let chain = match address {
        TreeAddress::Shape { shapes } => shapes,
        TreeAddress::Path { .. } => return None,
    };
    let (last, parents) = chain.split_last()?;
    let siblings = shapes_at(&document.shapes, parents)?;
    if *last >= siblings.len() {
        return None;
    }
    Some(ShapeSlot {
        chain: chain.clone(),
    })
}

/// Adjusts `slot` so it still names the same shape after the shape at `removed`
/// has been taken out of the tree.
fn shift_slot_after_removal(mut slot: ShapeSlot, removed: &ShapeSlot) -> ShapeSlot {
    let depth = removed.parents().len();
    if slot.chain.len() > depth
        && slot.chain[..depth] == removed.parents()[..]
        && slot.chain[depth] > removed.index()
    {
        slot.chain[depth] -= 1;
    }
    slot
}

/// Whether `outer` names a shape that is `inner` itself or one of its
/// ancestors. A path is never anybody's ancestor.
pub(crate) fn address_is_inside(outer: &TreeAddress, inner: &TreeAddress) -> bool {
    let TreeAddress::Shape {
        shapes: outer_shapes,
    } = outer
    else {
        return false;
    };
    let inner_shapes = inner.shape_chain();
    inner_shapes.len() >= outer_shapes.len()
        && inner_shapes[..outer_shapes.len()] == outer_shapes[..]
}

/// Removes the shape named by `address` from its container.
fn take_shape(document: &mut Document, address: &TreeAddress) -> Option<Shape> {
    let slot = shape_slot(document, address)?;
    let siblings = shapes_vec_at_mut(document, slot.parents())?;
    if slot.index() >= siblings.len() {
        return None;
    }
    Some(siblings.remove(slot.index()))
}

/// Removes the path named by `address` from its owning shape.
fn take_path(document: &mut Document, address: &TreeAddress) -> Option<Path> {
    let TreeAddress::Path { shapes, path } = address else {
        return None;
    };
    let shape = shape_at_chain_mut(document, shapes)?;
    if *path >= shape.paths.len() {
        return None;
    }
    Some(shape.paths.remove(*path))
}

/// Rewrites a sibling list's Z values to match its vector order.
///
/// Painting sorts stably by Z, so a list whose Z values equal their indices
/// paints in vector order. Reordering physically moves shapes in the vector
/// and then calls this, keeping both notions of order in agreement.
fn renormalize_z(siblings: &mut [Shape]) {
    for (index, shape) in siblings.iter_mut().enumerate() {
        shape.z = index as f32;
    }
}

/// Moves `source` next to `target` in the target's sibling list.
///
/// `higher` inserts the source after the target (higher Z, painted on top and
/// listed below it); `false` inserts it before (lower Z, painted underneath and
/// listed above it). Returns the shape's new address, or `None` when either
/// address is invalid or the move would put a shape inside itself.
pub fn move_shape_near(
    document: &mut Document,
    source: &TreeAddress,
    target: &TreeAddress,
    higher: bool,
) -> Option<TreeAddress> {
    if address_is_inside(source, target) {
        return None;
    }
    let source_slot = shape_slot(document, source)?;
    let target_slot = shape_slot(document, target)?;

    let shape = take_shape(document, source)?;
    let target_slot = shift_slot_after_removal(target_slot, &source_slot);
    let index = target_slot.index() + usize::from(higher);
    let siblings = shapes_vec_at_mut(document, target_slot.parents())?;
    let index = index.min(siblings.len());
    siblings.insert(index, shape);
    renormalize_z(siblings);

    let mut chain = target_slot.parents().to_vec();
    chain.push(index);
    Some(TreeAddress::Shape { shapes: chain })
}

/// Parents `source` to `target` as its last child.
pub fn move_shape_into(
    document: &mut Document,
    source: &TreeAddress,
    target: &TreeAddress,
) -> Option<TreeAddress> {
    if address_is_inside(source, target) {
        return None;
    }
    let source_slot = shape_slot(document, source)?;
    let target_slot = shape_slot(document, target)?;

    let shape = take_shape(document, source)?;
    let target_slot = shift_slot_after_removal(target_slot, &source_slot);
    let children = shapes_vec_at_mut(document, &target_slot.chain)?;
    let index = children.len();
    children.push(shape);
    renormalize_z(children);

    let mut chain = target_slot.chain;
    chain.push(index);
    Some(TreeAddress::Shape { shapes: chain })
}

/// Moves `source` next to the path named by `target`, either within the same
/// shape or into another shape.
///
/// Paths are listed in vector order, so `after` inserts the source below the
/// target in the list and `false` inserts it above. Returns the path's new
/// address, or `None` when either address is invalid or names the same path.
pub fn move_path_near(
    document: &mut Document,
    source: &TreeAddress,
    target: &TreeAddress,
    after: bool,
) -> Option<TreeAddress> {
    let TreeAddress::Path {
        shapes: source_shapes,
        path: source_index,
    } = source
    else {
        return None;
    };
    let TreeAddress::Path {
        shapes: target_shapes,
        path: target_index,
    } = target
    else {
        return None;
    };
    if source == target {
        return None;
    }
    if *source_index >= shape_at_chain(document, source_shapes)?.paths.len()
        || *target_index >= shape_at_chain(document, target_shapes)?.paths.len()
    {
        return None;
    }

    let path = take_path(document, source)?;
    let target_shape = shape_at_chain_mut(document, target_shapes)?;
    let mut index = *target_index;
    if source_shapes == target_shapes && *source_index < *target_index {
        // Taking the source out shifted the target towards the front.
        index -= 1;
    }
    let index = (index + usize::from(after)).min(target_shape.paths.len());
    target_shape.paths.insert(index, path);
    Some(TreeAddress::Path {
        shapes: target_shapes.clone(),
        path: index,
    })
}

/// Moves a path from one shape into another, appending it to the target's
/// paths. Returns the path's new address.
pub fn move_path_into_shape(
    document: &mut Document,
    source: &TreeAddress,
    target: &TreeAddress,
) -> Option<TreeAddress> {
    let TreeAddress::Shape {
        shapes: target_shapes,
    } = target
    else {
        return None;
    };
    let TreeAddress::Path { .. } = source else {
        return None;
    };

    let path = take_path(document, source)?;
    let target_shape = shape_at_chain_mut(document, target_shapes)?;
    let index = target_shape.paths.len();
    target_shape.paths.push(path);
    Some(TreeAddress::Path {
        shapes: target_shapes.clone(),
        path: index,
    })
}

/// The editable name of the node at `address`.
pub fn name_at<'a>(document: &'a Document, address: &TreeAddress) -> Option<&'a str> {
    match address {
        TreeAddress::Shape { .. } => Some(shape_at(document, address)?.name.as_str()),
        TreeAddress::Path { .. } => Some(path_at(document, address)?.name.as_str()),
    }
}

/// Renames the node at `address`. Returns `false` when it does not exist.
pub fn rename_at(document: &mut Document, address: &TreeAddress, name: String) -> bool {
    match address {
        TreeAddress::Shape { .. } => match shape_at_mut(document, address) {
            Some(shape) => {
                shape.name = name;
                true
            }
            None => false,
        },
        TreeAddress::Path { .. } => match path_at_mut(document, address) {
            Some(path) => {
                path.name = name;
                true
            }
            None => false,
        },
    }
}

/// Commits the in-progress rename, if any.
pub fn commit_rename(editor: &mut EditorState) {
    let Some(renaming) = editor.renaming.take() else {
        return;
    };
    let name = renaming.buffer.trim().to_owned();
    if rename_at(&mut editor.document, &renaming.address, name) {
        editor.structure_revision += 1;
    }
}

/// Abandons the in-progress rename, if any.
pub fn cancel_rename(editor: &mut EditorState) {
    editor.renaming = None;
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

/// Returns the shape under `point` that the user is most likely aiming at.
///
/// A selected shape is tried first with clipping ignored, so a shape that its
/// parent clips away stays reachable while it is selected (for example after
/// dragging it outside its parent). Trying it before the paint order also
/// raises the selection above everything else. Every other shape is only
/// pickable where it is actually visible.
fn pick_shape(
    document: &Document,
    selected: Option<&TreeAddress>,
    point: [f32; 2],
    slack: f32,
) -> Option<TreeAddress> {
    if let Some(TreeAddress::Shape { shapes }) = selected
        && let Some(shape) = shape_at_chain(document, shapes)
        && let Some(address) = pick_shape_node(shape, point, slack, shapes, false)
    {
        return Some(address);
    }

    // Later indices are painted later, so walk the paint order backwards.
    for index in document.shape_indices_in_paint_order().into_iter().rev() {
        if let Some(address) =
            pick_shape_node(&document.shapes[index], point, slack, &[index], true)
        {
            return Some(address);
        }
    }
    None
}

/// Tests one shape and its children, mirroring painting order.
///
/// The stroke is painted over the children and the fill underneath them.
/// Children are only visible inside the parent's fill when the parent has
/// paths; a group without paths does not clip at all. `respect_clip` controls
/// whether that visibility rule applies to the descent.
fn pick_shape_node(
    shape: &Shape,
    point: [f32; 2],
    slack: f32,
    chain: &[usize],
    respect_clip: bool,
) -> Option<TreeAddress> {
    let address = || TreeAddress::Shape {
        shapes: chain.to_vec(),
    };

    // Every shape is pickable near its outline, whether or not it has paint,
    // so an unfilled or unstroked shape can still be selected and dragged. A
    // stroke widens the tolerance by half its width.
    let tolerance = shape.style.stroke.map_or(0.0, |stroke| stroke.width * 0.5) + slack;
    if shape_distance(shape, point) <= tolerance {
        return Some(address());
    }

    if !shape.children.is_empty() {
        let visible = !respect_clip || shape.paths.is_empty() || shape_contains(shape, point);
        if visible {
            for index in shape.child_indices_in_paint_order().into_iter().rev() {
                let mut child_chain = chain.to_vec();
                child_chain.push(index);
                if let Some(found) = pick_shape_node(
                    &shape.children[index],
                    point,
                    slack,
                    &child_chain,
                    respect_clip,
                ) {
                    return Some(found);
                }
            }
        }
    }

    if shape.style.fill.is_some() && shape_contains(shape, point) {
        return Some(address());
    }

    None
}

/// Whether `point` lies inside the shape's combined fill.
fn shape_contains(shape: &Shape, point: [f32; 2]) -> bool {
    shape_fill_path(shape).contains(to_kurbo_point(point))
}

/// Distance from `point` to the nearest contour of `shape`.
fn shape_distance(shape: &Shape, point: [f32; 2]) -> f32 {
    let point = to_kurbo_point(point);
    shape_to_bez_path(shape)
        .segments()
        .map(|segment| segment.nearest(point, NEAREST_ACCURACY).distance_sq)
        .fold(f64::INFINITY, f64::min)
        .sqrt() as f32
}

// ---------------------------------------------------------------------------
// Inserting points
// ---------------------------------------------------------------------------

/// The curve of one model segment, starting at the previous anchor.
fn segment_curve(path: &Path, index: usize) -> Option<PathSeg> {
    let segment = *path.segments.get(index)?;
    let start = segment_start(path, index);
    let curve = if segment.is_line() {
        PathSeg::Line(KurboLine::new(
            to_kurbo_point(start),
            to_kurbo_point(segment.to),
        ))
    } else {
        PathSeg::Cubic(CubicBez::new(
            to_kurbo_point(start),
            to_kurbo_point(segment.ctrl1.unwrap_or(start)),
            to_kurbo_point(segment.ctrl2.unwrap_or(segment.to)),
            to_kurbo_point(segment.to),
        ))
    };
    Some(curve)
}

/// The segment nearest to `point`, as `(index, parameter, distance)`.
fn nearest_point_on_path(path: &Path, point: [f32; 2]) -> Option<(usize, f64, f32)> {
    let point = to_kurbo_point(point);
    let mut best: Option<(usize, f64, f64)> = None;
    for index in 0..path.segments.len() {
        let Some(curve) = segment_curve(path, index) else {
            continue;
        };
        let nearest = curve.nearest(point, NEAREST_ACCURACY);
        if best.is_none_or(|(_, _, distance_sq)| nearest.distance_sq < distance_sq) {
            best = Some((index, nearest.t, nearest.distance_sq));
        }
    }
    best.map(|(index, t, distance_sq)| (index, t, distance_sq.sqrt() as f32))
}

fn lerp_point(from: KurboPoint, to: KurboPoint, t: f64) -> KurboPoint {
    KurboPoint::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
}

fn from_kurbo_point(point: KurboPoint) -> [f32; 2] {
    [point.x as f32, point.y as f32]
}

/// Splits segment `index` at parameter `t` with de Casteljau's algorithm,
/// keeping the drawn shape unchanged, and returns the index of the new anchor.
///
/// A straight segment is split into two straight segments so no handles
/// appear. For a curve, the split controls are stored and controls that were
/// omitted stay omitted, so only genuinely curved sides gain handles.
fn split_segment(path: &mut Path, index: usize, t: f64) -> Option<usize> {
    let segment = *path.segments.get(index)?;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    let start = segment_start(path, index);
    let p0 = to_kurbo_point(start);
    let p3 = to_kurbo_point(segment.to);

    if segment.is_line() {
        let split = from_kurbo_point(lerp_point(p0, p3, t));
        path.segments[index].to = split;
        path.segments.insert(index + 1, Segment::line(segment.to));
        return Some(index);
    }

    // Resolve omitted controls to their endpoints, then subdivide the cubic.
    let p1 = to_kurbo_point(segment.ctrl1.unwrap_or(start));
    let p2 = to_kurbo_point(segment.ctrl2.unwrap_or(segment.to));
    let q0 = lerp_point(p0, p1, t);
    let q1 = lerp_point(p1, p2, t);
    let q2 = lerp_point(p2, p3, t);
    let r0 = lerp_point(q0, q1, t);
    let r1 = lerp_point(q1, q2, t);
    let split = lerp_point(r0, r1, t);

    path.segments[index] = Segment {
        ctrl1: segment.ctrl1.map(|_| from_kurbo_point(q0)),
        ctrl2: Some(from_kurbo_point(r0)),
        to: from_kurbo_point(split),
    };
    path.segments.insert(
        index + 1,
        Segment {
            ctrl1: Some(from_kurbo_point(r1)),
            ctrl2: segment.ctrl2.map(|_| from_kurbo_point(q2)),
            to: segment.to,
        },
    );

    // The split point is the modified segment's endpoint, i.e. `Anchor(index)`.
    Some(index)
}

/// Inserts a new anchor at the point of the path outline nearest to `point`.
///
/// Only the outline is eligible: clicking the interior of a filled shape far
/// from its edge does nothing. Returns `true` when a point was inserted.
fn insert_point(editor: &mut EditorState, point: [f32; 2], slack: f32) -> bool {
    let Some(address) = pick_shape(&editor.document, editor.selected.as_ref(), point, slack) else {
        return false;
    };
    let Some(shape) = shape_at(&editor.document, &address) else {
        return false;
    };
    let Some((path_index, segment_index, t, distance)) = nearest_on_shape(shape, point) else {
        return false;
    };

    // Clicking on a wide stroke still counts as clicking its outline.
    let stroke_half_width = shape.style.stroke.map_or(0.0, |stroke| stroke.width * 0.5);
    if distance > slack + stroke_half_width {
        return false;
    }

    // Do not create a duplicate anchor on top of an existing one.
    const EDGE_T: f64 = 1e-3;
    if !(EDGE_T..=1.0 - EDGE_T).contains(&t) {
        return false;
    }

    let Some(shape) = shape_at_mut(&mut editor.document, &address) else {
        return false;
    };
    let Some(path) = shape.paths.get_mut(path_index) else {
        return false;
    };
    let Some(new_index) = split_segment(path, segment_index, t) else {
        return false;
    };

    editor.selected = Some(address);
    editor.selected_handle = Some(SelectedNode {
        path: path_index,
        handle: Handle::Anchor(new_index),
    });
    editor.drag = None;
    true
}

/// The segment nearest to `point` among all of a shape's paths, as
/// `(path index, segment index, parameter, distance)`.
fn nearest_on_shape(shape: &Shape, point: [f32; 2]) -> Option<(usize, usize, f64, f32)> {
    let mut best: Option<(usize, usize, f64, f32)> = None;
    for (path_index, path) in shape.paths.iter().enumerate() {
        let Some((segment_index, t, distance)) = nearest_point_on_path(path, point) else {
            continue;
        };
        if best.is_none_or(|(_, _, _, best_distance)| distance < best_distance) {
            best = Some((path_index, segment_index, t, distance));
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Control points
// ---------------------------------------------------------------------------

/// All draggable points of a path, including the control points that exist.
fn handles(path: &Path) -> Vec<(Handle, [f32; 2])> {
    let mut handles = Vec::new();
    // A closed loop has no separate start anchor: it is the last segment's
    // endpoint, listed below like any other anchor.
    if let (false, Some(start)) = (path.is_closed(), path.start()) {
        handles.push((Handle::Start, start));
    }
    for (index, segment) in path.segments.iter().enumerate() {
        handles.push((Handle::Anchor(index), segment.to));
        if let Some(ctrl1) = segment.ctrl1 {
            handles.push((Handle::Ctrl1(index), ctrl1));
        }
        if let Some(ctrl2) = segment.ctrl2 {
            handles.push((Handle::Ctrl2(index), ctrl2));
        }
    }
    handles
}

/// Finds the selected node's control point nearest to `point`, if within
/// `radius`.
///
/// A whole-shape selection can grab points from any of its direct paths, so
/// the search spans all of them and returns the path-qualified address of the
/// best match. A single-path selection only searches that path.
fn node_under<'a>(
    document: &'a Document,
    address: &TreeAddress,
    point: [f32; 2],
    radius: f32,
) -> Option<(SelectedNode, TreeAddress, &'a Path)> {
    let mut best: Option<(f32, usize, Handle, &'a Path)> = None;
    for (path_index, path) in selection_paths(document, address) {
        for (handle, position) in handles(path) {
            let distance = distance(position, point);
            if distance <= radius
                && best
                    .as_ref()
                    .is_none_or(|(best_distance, ..)| distance < *best_distance)
            {
                best = Some((distance, path_index, handle, path));
            }
        }
    }

    let (_, path_index, handle, path) = best?;
    let drag_address = TreeAddress::Path {
        shapes: address.shape_chain().to_vec(),
        path: path_index,
    };
    Some((
        SelectedNode {
            path: path_index,
            handle,
        },
        drag_address,
        path,
    ))
}

fn set_handle(path: &mut Path, handle: Handle, point: [f32; 2]) {
    match handle {
        Handle::Start => {
            let PathEnds::Open(start) = &mut path.ends else {
                return;
            };
            let delta = offset(*start, point);
            *start = point;

            // The outgoing control attached to the anchor follows it.
            if let Some(segment) = path.segments.first_mut()
                && let Some(ctrl1) = segment.ctrl1.as_mut()
            {
                *ctrl1 = translate(*ctrl1, delta);
            }
        }
        Handle::Anchor(index) => {
            let Some(segment) = path.segments.get(index) else {
                return;
            };
            let delta = offset(segment.to, point);
            let len = path.segments.len();

            path.segments[index].to = point;
            // The incoming control of this segment and the outgoing control of
            // the following one travel with the anchor.
            if let Some(ctrl2) = path.segments[index].ctrl2.as_mut() {
                *ctrl2 = translate(*ctrl2, delta);
            }
            let next = if index + 1 < len {
                Some(index + 1)
            } else if path.is_closed() {
                Some(0)
            } else {
                None
            };
            if let Some(next) = next
                && let Some(ctrl1) = path.segments[next].ctrl1.as_mut()
            {
                *ctrl1 = translate(*ctrl1, delta);
            }
        }
        Handle::Ctrl1(index) => {
            if let Some(ctrl1) = path
                .segments
                .get_mut(index)
                .and_then(|segment| segment.ctrl1.as_mut())
            {
                *ctrl1 = point;
            }
        }
        Handle::Ctrl2(index) => {
            if let Some(ctrl2) = path
                .segments
                .get_mut(index)
                .and_then(|segment| segment.ctrl2.as_mut())
            {
                *ctrl2 = point;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Instantiating control points and symmetric handles
// ---------------------------------------------------------------------------

/// The anchor a segment starts at.
fn segment_start(path: &Path, index: usize) -> [f32; 2] {
    if index == 0 {
        path.start().unwrap_or([0.0, 0.0])
    } else {
        path.segments[index - 1].to
    }
}

/// The position of an anchor handle.
fn endpoint_anchor(path: &Path, handle: Handle) -> Option<[f32; 2]> {
    match handle {
        Handle::Start => path.start(),
        Handle::Anchor(index) => path.segments.get(index).map(|segment| segment.to),
        Handle::Ctrl1(_) | Handle::Ctrl2(_) => None,
    }
}

/// Segments that a shift-drag starting at `handle` can give a control point.
///
/// Only sides without a control yet are candidates. Each entry pairs the
/// segment with the direction from the anchor to the segment's far end, so
/// when two segments meet at the anchor the drag direction picks one.
fn bend_candidates(path: &Path, handle: Handle) -> Vec<(BendTarget, [f32; 2])> {
    let Some(anchor) = endpoint_anchor(path, handle) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();

    // The segment ending at the dragged anchor.
    let incoming = match handle {
        Handle::Anchor(index) => Some(index),
        Handle::Start | Handle::Ctrl1(_) | Handle::Ctrl2(_) => None,
    };
    if let Some(index) = incoming
        && let Some(segment) = path.segments.get(index)
        && segment.ctrl2.is_none()
    {
        let far = segment_start(path, index);
        candidates.push((BendTarget::Incoming(index), offset(anchor, far)));
    }

    // The segment starting at the dragged anchor.
    let len = path.segments.len();
    let outgoing = match handle {
        Handle::Start => Some(0),
        Handle::Anchor(index) if index + 1 < len => Some(index + 1),
        Handle::Anchor(_) if path.is_closed() => Some(0),
        _ => None,
    };
    if let Some(index) = outgoing
        && let Some(segment) = path.segments.get(index)
        && segment.ctrl1.is_none()
    {
        candidates.push((BendTarget::Outgoing(index), offset(anchor, segment.to)));
    }

    candidates
}

/// Picks the candidate whose direction best matches the drag.
fn choose_bend(path: &Path, handle: Handle, drag: [f32; 2]) -> Option<BendTarget> {
    let drag_length = (drag[0] * drag[0] + drag[1] * drag[1]).sqrt();
    if drag_length <= f32::EPSILON {
        return None;
    }

    bend_candidates(path, handle)
        .into_iter()
        .map(|(target, direction)| {
            let length = (direction[0] * direction[0] + direction[1] * direction[1])
                .sqrt()
                .max(f32::EPSILON);
            let alignment = (direction[0] * drag[0] + direction[1] * drag[1]) / length;
            (alignment, target)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, target)| target)
}

/// Gives the targeted side of a segment a control point at `point`, creating
/// it when missing. Calling it again keeps moving that control. Returns
/// `false` when the segment does not exist.
fn apply_bend(path: &mut Path, target: BendTarget, point: [f32; 2]) -> bool {
    let (index, slot) = match target {
        BendTarget::Outgoing(index) => (index, CtrlSlot::First),
        BendTarget::Incoming(index) => (index, CtrlSlot::Second),
    };
    let Some(segment) = path.segments.get_mut(index) else {
        return false;
    };
    match slot {
        CtrlSlot::First => segment.ctrl1 = Some(point),
        CtrlSlot::Second => segment.ctrl2 = Some(point),
    }
    true
}

/// Mirrors the neighbouring handle of a shift-dragged control point.
///
/// Symmetry only applies across a shared node: when the adjacent segment has a
/// control at the anchor this handle meets, that control is mirrored across the
/// anchor so the joint stays smooth. A missing neighbour control is left
/// missing, so a lone cubic's other control never moves.
fn symmetrize(path: &mut Path, handle: Handle, point: [f32; 2]) {
    match handle {
        Handle::Ctrl1(index) => {
            let anchor = segment_start(path, index);
            if let Some(location) = incoming_control(path, index) {
                set_control(path, location, mirror(anchor, point));
            }
        }
        Handle::Ctrl2(index) => {
            let Some(segment) = path.segments.get(index) else {
                return;
            };
            let anchor = segment.to;
            if let Some(location) = outgoing_control(path, index) {
                set_control(path, location, mirror(anchor, point));
            }
        }
        Handle::Start | Handle::Anchor(_) => {}
    }
}

/// The control that meets the start of segment `index` at its anchor.
fn incoming_control(path: &Path, index: usize) -> Option<(usize, CtrlSlot)> {
    let previous = if index > 0 {
        index - 1
    } else if path.is_closed() {
        path.segments.len().checked_sub(1)?
    } else {
        return None;
    };
    path.segments
        .get(previous)?
        .ctrl2
        .is_some()
        .then_some((previous, CtrlSlot::Second))
}

/// The control that meets the end of segment `index` at its anchor.
fn outgoing_control(path: &Path, index: usize) -> Option<(usize, CtrlSlot)> {
    let len = path.segments.len();
    let next = if index + 1 < len {
        index + 1
    } else if path.is_closed() {
        0
    } else {
        return None;
    };
    path.segments
        .get(next)?
        .ctrl1
        .is_some()
        .then_some((next, CtrlSlot::First))
}

fn set_control(path: &mut Path, location: (usize, CtrlSlot), point: [f32; 2]) {
    if let Some(segment) = path.segments.get_mut(location.0) {
        match location.1 {
            CtrlSlot::First => segment.ctrl1 = Some(point),
            CtrlSlot::Second => segment.ctrl2 = Some(point),
        }
    }
}

fn mirror(anchor: [f32; 2], point: [f32; 2]) -> [f32; 2] {
    [2.0 * anchor[0] - point[0], 2.0 * anchor[1] - point[1]]
}

fn segment_to(segment: &Segment) -> [f32; 2] {
    segment.to
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

// ---------------------------------------------------------------------------
// Object dragging
// ---------------------------------------------------------------------------

/// Moves every point of `path` by `delta`.
fn translate_path(path: &mut Path, delta: [f32; 2]) {
    if let PathEnds::Open(start) = &mut path.ends {
        *start = translate(*start, delta);
    }
    for segment in &mut path.segments {
        if let Some(ctrl1) = segment.ctrl1.as_mut() {
            *ctrl1 = translate(*ctrl1, delta);
        }
        if let Some(ctrl2) = segment.ctrl2.as_mut() {
            *ctrl2 = translate(*ctrl2, delta);
        }
        segment.to = translate(segment.to, delta);
    }
}

/// Moves every point of `shape` by `delta`, including nested children.
pub(crate) fn translate_shape(shape: &mut Shape, delta: [f32; 2]) {
    for path in &mut shape.paths {
        translate_path(path, delta);
    }
    for child in &mut shape.children {
        translate_shape(child, delta);
    }
}

// ---------------------------------------------------------------------------
// Pen tool
// ---------------------------------------------------------------------------

/// True when the next click should close the current pen path rather than add
/// another anchor.
fn pen_should_close(editor: &EditorState, point: [f32; 2], slack: f32) -> bool {
    editor.pen.as_ref().is_some_and(|draft| {
        !draft.path.segments.is_empty()
            && draft
                .path
                .start()
                .is_some_and(|start| distance(point, start) <= slack)
    })
}

/// Ends the pen draft. When `finalize` is set and the draft has at least one
/// segment, the path is wrapped in a new top-level shape and selected.
pub fn finish_pen(editor: &mut EditorState, finalize: bool) {
    let Some(draft) = editor.pen.take() else {
        return;
    };
    if !finalize || draft.path.segments.is_empty() {
        return;
    }

    // New shapes go on top of everything, using a Z greater than any sibling.
    let z = match editor
        .document
        .shapes
        .iter()
        .map(|shape| shape.z)
        .reduce(f32::max)
    {
        Some(max) => max + 1.0,
        None => 0.0,
    };
    let index = editor.document.shapes.len();
    editor.document.shapes.push(Shape {
        name: String::new(),
        z,
        style: pen_style(),
        paths: vec![draft.path],
        children: Vec::new(),
    });

    editor.selected = Some(TreeAddress::Shape {
        shapes: vec![index],
    });
    editor.selected_handle = None;
    editor.drag = None;
    editor.structure_revision += 1;
}

/// Default paint for a newly drawn path: a light stroke with no fill.
fn pen_style() -> Style {
    Style {
        fill: None,
        stroke: Some(StrokeStyle {
            color: PEN_STROKE_COLOR,
            width: DEFAULT_STROKE_WIDTH,
        }),
    }
}

/// The last anchor of `path`.
fn path_end(path: &Path) -> [f32; 2] {
    path.segments
        .last()
        .map(segment_to)
        .or_else(|| path.start())
        .unwrap_or([0.0, 0.0])
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

/// Deletes the selected control point, or the selected object when no control
/// point is selected.
fn delete_selection(editor: &mut EditorState) {
    if let Some(node) = editor.selected_handle.take() {
        // A control point is selected: only it is deleted, even when the
        // handle happens to sit on top of another object.
        let node_address = editor
            .selected
            .as_ref()
            .map(|address| node_path_address(address, node.path));
        if let Some(node_address) = node_address
            && let Some(path) = path_at_mut(&mut editor.document, &node_address)
            && delete_handle(path, node.handle)
        {
            editor.drag = None;
            editor.structure_revision += 1;
        }
        return;
    }

    let Some(address) = editor.selected.take() else {
        return;
    };
    if remove_object(&mut editor.document, &address) {
        editor.structure_revision += 1;
    }
    editor.drag = None;
}

/// The address of path `index` within the node selected at `address`.
fn node_path_address(address: &TreeAddress, index: usize) -> TreeAddress {
    TreeAddress::Path {
        shapes: address.shape_chain().to_vec(),
        path: index,
    }
}

/// Removes one point from `path`.
///
/// Removing a control point leaves the segment's other control intact, so a
/// segment only becomes a line once both controls are gone.
///
/// Returns `false` when the handle cannot be deleted on its own:
///
/// * an open path's start has no successor to become the new start,
/// * a closed loop cannot lose its only segment,
/// * the control point is already missing.
fn delete_handle(path: &mut Path, handle: Handle) -> bool {
    match handle {
        Handle::Start => {
            if path.is_closed() || path.segments.is_empty() {
                return false;
            }
            // The first segment's endpoint becomes the new start.
            let first = path.segments.remove(0);
            path.ends = PathEnds::Open(first.to);
            true
        }
        Handle::Anchor(index) => {
            if index >= path.segments.len() {
                return false;
            }
            if path.is_closed() && path.segments.len() <= 1 {
                return false;
            }
            path.segments.remove(index);
            true
        }
        Handle::Ctrl1(index) => {
            let Some(segment) = path.segments.get_mut(index) else {
                return false;
            };
            segment.ctrl1.take().is_some()
        }
        Handle::Ctrl2(index) => {
            let Some(segment) = path.segments.get_mut(index) else {
                return false;
            };
            segment.ctrl2.take().is_some()
        }
    }
}

/// Removes the node named by `address` from the document.
///
/// Removing a shape takes its paths and nested children with it.
fn remove_object(document: &mut Document, address: &TreeAddress) -> bool {
    match address {
        TreeAddress::Shape { .. } => take_shape(document, address).is_some(),
        TreeAddress::Path { .. } => take_path(document, address).is_some(),
    }
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

/// Draws the selected path's outline, bezier arms, and draggable points.
///
/// Sizes are given in screen pixels and converted to document units so the
/// overlay stays legible at any zoom.
fn draw_control_overlay(
    scene: &mut Scene,
    path: &Path,
    scale: f32,
    selected_handle: Option<Handle>,
) {
    let scale = scale.max(f32::EPSILON);
    let pixels = |amount: f32| f64::from(amount / scale);

    scene.stroke(
        &KurboStroke::new(pixels(OVERLAY_OUTLINE_WIDTH)),
        Affine::IDENTITY,
        &OVERLAY_ACCENT,
        None,
        &path_to_bez_path(path),
    );

    // Arms from cubic control points to the anchors they influence. Only the
    // control points that actually exist have arms; omitted ones lie on their
    // anchors and would draw a zero-length arm anyway.
    let mut previous = path.start().unwrap_or([0.0, 0.0]);
    for segment in &path.segments {
        if let Some(ctrl1) = segment.ctrl1 {
            stroke_segment(
                scene,
                previous,
                ctrl1,
                pixels(OVERLAY_ARM_WIDTH),
                &OVERLAY_ACCENT,
            );
        }
        if let Some(ctrl2) = segment.ctrl2 {
            stroke_segment(
                scene,
                ctrl2,
                segment.to,
                pixels(OVERLAY_ARM_WIDTH),
                &OVERLAY_ACCENT,
            );
        }
        previous = segment.to;
    }

    for (handle, position) in handles(path) {
        // The selected control point is the one the Delete key will remove, so
        // make it stand out.
        let selected = selected_handle == Some(handle);
        let radius = pixels(if selected {
            OVERLAY_SELECTED_HANDLE_RADIUS
        } else {
            OVERLAY_HANDLE_RADIUS
        });
        let fill = if selected {
            &OVERLAY_ACCENT
        } else {
            &OVERLAY_HANDLE_FILL
        };
        let circle = Circle::new(to_kurbo_point(position), radius);
        scene.fill(Fill::NonZero, Affine::IDENTITY, fill, None, &circle);
        scene.stroke(
            &KurboStroke::new(pixels(OVERLAY_HANDLE_OUTLINE_WIDTH)),
            Affine::IDENTITY,
            &OVERLAY_HANDLE_OUTLINE,
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
    use crate::model::{FORMAT_VERSION, Rgba8, StrokeStyle, Style};

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

    fn stroked(width: f32) -> Style {
        Style {
            fill: None,
            stroke: Some(StrokeStyle {
                color: Rgba8::rgb(0, 0, 0),
                width,
            }),
        }
    }

    fn shaped(path: Path, style: Style) -> Shape {
        Shape {
            paths: vec![path],
            style,
            ..default()
        }
    }

    fn document_of(shapes: Vec<Shape>) -> Document {
        Document {
            version: FORMAT_VERSION,
            bones: Vec::new(),
            shapes,
        }
    }

    fn shape_address(chain: &[usize]) -> TreeAddress {
        TreeAddress::Shape {
            shapes: chain.to_vec(),
        }
    }

    fn path_address(chain: &[usize], path: usize) -> TreeAddress {
        TreeAddress::Path {
            shapes: chain.to_vec(),
            path,
        }
    }

    #[test]
    fn picks_topmost_sibling_shape() {
        let document = document_of(vec![
            Shape {
                z: 0.0,
                ..shaped(rect(0.0, 0.0, 100.0, 100.0), filled(Rgba8::rgb(255, 0, 0)))
            },
            Shape {
                z: 10.0,
                ..shaped(
                    rect(50.0, 50.0, 150.0, 150.0),
                    filled(Rgba8::rgb(0, 0, 255)),
                )
            },
        ]);

        assert_eq!(
            pick_shape(&document, None, [75.0, 75.0], 0.0),
            Some(shape_address(&[1]))
        );
        assert_eq!(
            pick_shape(&document, None, [10.0, 10.0], 0.0),
            Some(shape_address(&[0]))
        );
        assert_eq!(pick_shape(&document, None, [200.0, 200.0], 0.0), None);
    }

    #[test]
    fn clipped_away_shapes_are_only_pickable_while_selected() {
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            children: vec![shaped(
                rect(0.0, 0.0, 200.0, 200.0),
                filled(Rgba8::rgb(0, 255, 0)),
            )],
            ..default()
        }]);

        // Inside the parent's clip the child is visible and pickable.
        assert_eq!(
            pick_shape(&document, None, [50.0, 50.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
        // Outside it the child is clipped away, so a click finds nothing...
        assert_eq!(pick_shape(&document, None, [150.0, 150.0], 0.0), None);

        // ...unless the child is selected, so it can be dragged back into view.
        let child = shape_address(&[0, 0]);
        assert_eq!(
            pick_shape(&document, Some(&child), [150.0, 150.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
    }

    #[test]
    fn visible_shapes_win_over_clipped_away_children() {
        let document = document_of(vec![
            Shape {
                z: 0.0,
                paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
                children: vec![shaped(
                    rect(0.0, 0.0, 200.0, 200.0),
                    filled(Rgba8::rgb(0, 255, 0)),
                )],
                ..default()
            },
            Shape {
                z: 10.0,
                ..shaped(rect(0.0, 0.0, 200.0, 200.0), filled(Rgba8::rgb(0, 0, 255)))
            },
        ]);

        // The top shape is visible here, so it wins over the clipped child
        // that happens to cover the same point.
        assert_eq!(
            pick_shape(&document, None, [150.0, 150.0], 0.0),
            Some(shape_address(&[1]))
        );

        // A selected shape is raised to the top, even when clipped away.
        let child = shape_address(&[0, 0]);
        assert_eq!(
            pick_shape(&document, Some(&child), [150.0, 150.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
    }

    #[test]
    fn a_shape_without_paths_does_not_clip_its_children() {
        let document = document_of(vec![Shape {
            children: vec![shaped(
                rect(0.0, 0.0, 100.0, 100.0),
                filled(Rgba8::rgb(0, 255, 0)),
            )],
            ..default()
        }]);

        assert_eq!(
            pick_shape(&document, None, [50.0, 50.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
    }

    #[test]
    fn parent_stroke_is_picked_over_children() {
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            style: stroked(4.0),
            children: vec![shaped(
                rect(0.0, 0.0, 100.0, 100.0),
                filled(Rgba8::rgb(255, 0, 0)),
            )],
            ..default()
        }]);

        // Away from the outline the child is picked.
        assert_eq!(
            pick_shape(&document, None, [50.0, 50.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
        // The parent's stroke is painted over the child, so it wins there.
        assert_eq!(
            pick_shape(&document, None, [1.0, 50.0], 0.0),
            Some(shape_address(&[0]))
        );
    }

    #[test]
    fn parent_fill_is_picked_under_children() {
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            style: filled(Rgba8::rgb(0, 0, 255)),
            children: vec![shaped(
                rect(20.0, 20.0, 80.0, 80.0),
                filled(Rgba8::rgb(255, 0, 0)),
            )],
            ..default()
        }]);

        // The child is painted over the parent's fill.
        assert_eq!(
            pick_shape(&document, None, [50.0, 50.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
        // Where the child does not cover, the parent's fill is picked.
        assert_eq!(
            pick_shape(&document, None, [10.0, 50.0], 0.0),
            Some(shape_address(&[0]))
        );
    }

    #[test]
    fn unpainted_shapes_are_picked_near_their_outline() {
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            style: Style::default(),
            ..default()
        }]);

        // With no paint, the interior is not a pick target...
        assert_eq!(pick_shape(&document, None, [50.0, 50.0], 0.0), None);
        // ...but the outline is, within the slack distance.
        assert_eq!(
            pick_shape(&document, None, [0.0, 50.0], 0.0),
            Some(shape_address(&[0]))
        );
        assert_eq!(
            pick_shape(&document, None, [3.0, 50.0], 3.0),
            Some(shape_address(&[0]))
        );
        assert_eq!(pick_shape(&document, None, [4.0, 50.0], 3.0), None);
    }

    #[test]
    fn a_compound_shape_can_have_a_hole() {
        let hole = Path::new([20.0, 20.0])
            .line_to([20.0, 80.0])
            .line_to([80.0, 80.0])
            .line_to([80.0, 20.0])
            .closed();
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0), hole],
            style: filled(Rgba8::rgb(0, 0, 0)),
            ..default()
        }]);

        // The inner contour is wound the other way, so it is a hole.
        assert_eq!(pick_shape(&document, None, [50.0, 50.0], 0.0), None);
        assert_eq!(
            pick_shape(&document, None, [10.0, 10.0], 0.0),
            Some(shape_address(&[0]))
        );
    }

    #[test]
    fn nested_children_can_be_addressed_and_edited() {
        let mut document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            style: filled(Rgba8::rgb(0, 0, 255)),
            children: vec![Shape {
                paths: vec![rect(10.0, 10.0, 90.0, 90.0)],
                style: filled(Rgba8::rgb(0, 255, 0)),
                ..default()
            }],
            ..default()
        }]);

        let address = path_address(&[0, 0], 0);
        assert!(path_at(&document, &address).is_some());
        assert!(shape_at(&document, &address).is_some());

        *path_at_mut(&mut document, &address).unwrap() = rect(20.0, 20.0, 80.0, 80.0);

        let inner = &document.shapes[0].children[0];
        assert_eq!(inner.paths[0], rect(20.0, 20.0, 80.0, 80.0));
    }

    #[test]
    fn a_ring_with_children_is_picked_by_its_band() {
        // A filled ring: an outer contour plus an opposite-wound hole, with a
        // child painted over part of the band.
        let hole = Path::new([30.0, 30.0])
            .line_to([30.0, 70.0])
            .line_to([70.0, 70.0])
            .line_to([70.0, 30.0])
            .closed();
        let document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0), hole],
            style: filled(Rgba8::rgb(0xe7, 0x6f, 0x51)),
            children: vec![shaped(
                rect(5.0, 5.0, 15.0, 15.0),
                filled(Rgba8::rgb(255, 255, 255)),
            )],
            ..default()
        }]);

        // The child painted over the band wins.
        assert_eq!(
            pick_shape(&document, None, [10.0, 10.0], 0.0),
            Some(shape_address(&[0, 0]))
        );
        // The band away from the child and the hole picks the ring itself.
        assert_eq!(
            pick_shape(&document, None, [85.0, 10.0], 0.0),
            Some(shape_address(&[0]))
        );
        // The hole is not filled, so nothing is there.
        assert_eq!(pick_shape(&document, None, [50.0, 50.0], 0.0), None);
    }

    #[test]
    fn strokes_are_hit_by_distance() {
        let document = document_of(vec![shaped(
            Path::new([0.0, 50.0]).line_to([100.0, 50.0]),
            stroked(4.0),
        )]);

        assert!(pick_shape(&document, None, [50.0, 52.0], 0.0).is_some());
        assert!(pick_shape(&document, None, [50.0, 60.0], 0.0).is_none());
    }

    #[test]
    fn address_is_inside_tracks_ancestry() {
        assert!(address_is_inside(
            &shape_address(&[0]),
            &shape_address(&[0, 1])
        ));
        assert!(address_is_inside(
            &shape_address(&[0]),
            &path_address(&[0, 1], 3)
        ));
        assert!(address_is_inside(
            &shape_address(&[0]),
            &shape_address(&[0])
        ));
        assert!(!address_is_inside(
            &shape_address(&[0, 1]),
            &shape_address(&[0])
        ));
        assert!(!address_is_inside(
            &path_address(&[0], 0),
            &path_address(&[0], 0)
        ));
    }

    #[test]
    fn screen_mapping_matches_fit() {
        let bounds = Bounds {
            min: [0.0, 0.0],
            max: [400.0, 200.0],
        };
        let view = ViewMapping::new(bounds, 900.0, 900.0, &Viewport::default());

        // The content is centered in the window area between the toolbar and
        // the sidebar, below the menu bar, and above the style panel.
        let content_center_x = TOOLBAR_WIDTH + (900.0 - SIDEBAR_WIDTH - TOOLBAR_WIDTH) * 0.5;
        let content_center_y = CONTENT_TOP + (900.0 - CONTENT_TOP - BOTTOM_BAR_HEIGHT) * 0.5;
        let center = view.screen_to_document(Vec2::new(content_center_x, content_center_y));
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
        set_handle(&mut path, Handle::Ctrl2(0), [12.0, 34.0]);
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: Some([10.0, 0.0]),
                ctrl2: Some([12.0, 34.0]),
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

        let join = path.start().unwrap();
        let at_join = handles(&path)
            .into_iter()
            .filter(|(_, position)| distance(*position, join) <= 1e-3)
            .count();
        assert_eq!(at_join, 1);
    }

    #[test]
    fn node_under_searches_every_path_of_a_shape() {
        let document = document_of(vec![Shape {
            paths: vec![
                Path::new([0.0, 0.0]).line_to([10.0, 0.0]),
                Path::new([50.0, 50.0]).line_to([60.0, 50.0]),
            ],
            ..default()
        }]);
        let address = shape_address(&[0]);

        let (node, node_address, _) = node_under(&document, &address, [50.0, 50.0], 1.0).unwrap();
        assert_eq!(
            node,
            SelectedNode {
                path: 1,
                handle: Handle::Start
            }
        );
        assert_eq!(node_address, path_address(&[0], 1));
    }

    #[test]
    fn dragging_an_anchor_moves_its_controls() {
        let mut path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 10.0], [30.0, 0.0])
            .cubic_to([40.0, 10.0], [50.0, 0.0], [60.0, 0.0]);

        set_handle(&mut path, Handle::Anchor(0), [40.0, 0.0]);

        // The anchor's incoming and outgoing controls both moved by the
        // anchor's delta, while the far ends of the segments stayed put.
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: Some([10.0, 0.0]),
                ctrl2: Some([30.0, 10.0]),
                to: [40.0, 0.0]
            }
        );
        assert_eq!(
            path.segments[1],
            Segment {
                ctrl1: Some([50.0, 10.0]),
                ctrl2: Some([50.0, 0.0]),
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

        // The join is the last segment's endpoint, not a separate start handle.
        set_handle(&mut path, Handle::Anchor(1), [5.0, 5.0]);

        assert_eq!(path.start(), Some([5.0, 5.0]));
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: Some([15.0, 5.0]),
                ctrl2: Some([20.0, 0.0]),
                to: [30.0, 0.0]
            }
        );
        assert_eq!(
            path.segments[1],
            Segment {
                ctrl1: Some([20.0, 30.0]),
                ctrl2: Some([15.0, 35.0]),
                to: [5.0, 5.0]
            }
        );
    }

    #[test]
    fn deleting_an_anchor_removes_its_segment() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([20.0, 0.0]);

        assert!(delete_handle(&mut path, Handle::Anchor(0)));
        assert_eq!(path.start(), Some([0.0, 0.0]));
        assert_eq!(path.segments, vec![Segment::line([20.0, 0.0])]);
    }

    #[test]
    fn deleting_a_control_point_removes_only_that_control() {
        let mut path = Path::new([0.0, 0.0]).cubic_to([10.0, 0.0], [20.0, 0.0], [30.0, 0.0]);

        assert!(delete_handle(&mut path, Handle::Ctrl1(0)));
        assert_eq!(
            path.segments,
            vec![Segment {
                ctrl1: None,
                ctrl2: Some([20.0, 0.0]),
                to: [30.0, 0.0]
            }]
        );

        // With both controls gone the segment is a line.
        assert!(delete_handle(&mut path, Handle::Ctrl2(0)));
        assert_eq!(path.segments, vec![Segment::line([30.0, 0.0])]);
    }

    #[test]
    fn deleting_a_start_point_promotes_the_next_anchor() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([20.0, 0.0]);

        assert!(delete_handle(&mut path, Handle::Start));
        assert_eq!(path.start(), Some([10.0, 0.0]));
        assert_eq!(path.segments, vec![Segment::line([20.0, 0.0])]);

        // A closed path's start is its join, so it cannot be removed on its
        // own.
        let mut closed = Path::new([0.0, 0.0]).line_to([10.0, 0.0]).closed();
        assert!(!delete_handle(&mut closed, Handle::Start));
    }

    #[test]
    fn deleting_shapes_and_paths_removes_them_from_the_tree() {
        let mut document = document_of(vec![
            shaped(rect(0.0, 0.0, 10.0, 10.0), Style::default()),
            Shape {
                paths: vec![rect(0.0, 0.0, 10.0, 10.0)],
                children: vec![shaped(rect(0.0, 0.0, 5.0, 5.0), Style::default())],
                ..default()
            },
        ]);

        // A nested shape is removed from its parent.
        assert!(remove_object(&mut document, &shape_address(&[1, 0])));
        assert!(document.shapes[1].children.is_empty());

        // A path is removed from its owning shape.
        assert!(remove_object(&mut document, &path_address(&[0], 0)));
        assert!(document.shapes[0].paths.is_empty());

        // A whole shape takes its subtree with it.
        assert!(remove_object(&mut document, &shape_address(&[1])));
        assert_eq!(document.shapes.len(), 1);
    }

    #[test]
    fn dragging_a_shape_translates_all_of_its_points() {
        let mut path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 10.0], [30.0, 10.0])
            .line_to([40.0, 20.0]);
        path.open();
        let mut shape = Shape {
            paths: vec![path],
            children: vec![shaped(
                Path::new([100.0, 100.0]).line_to([110.0, 100.0]),
                Style::default(),
            )],
            ..default()
        };

        translate_shape(&mut shape, [5.0, -3.0]);

        let path = &shape.paths[0];
        assert_eq!(path.start(), Some([5.0, -3.0]));
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: Some([15.0, -3.0]),
                ctrl2: Some([25.0, 7.0]),
                to: [35.0, 7.0],
            }
        );
        assert_eq!(path.segments[1], Segment::line([45.0, 17.0]));
        assert_eq!(shape.children[0].paths[0].start(), Some([105.0, 97.0]));
    }

    #[test]
    fn finishing_the_pen_creates_a_new_top_level_shape() {
        let mut editor = EditorState::new(document_of(Vec::new()));
        editor.pen = Some(PenDraft {
            path: Path::new([0.0, 0.0]).line_to([10.0, 10.0]),
            pending_out: None,
            dragging: false,
        });

        finish_pen(&mut editor, true);

        assert!(editor.pen.is_none());
        assert_eq!(editor.document.shapes.len(), 1);
        assert_eq!(editor.selected, Some(shape_address(&[0])));
        assert_eq!(editor.document.shapes[0].paths.len(), 1);
        assert_eq!(editor.document.shapes[0].z, 0.0);
        assert_eq!(editor.structure_revision, 1);
    }

    #[test]
    fn pen_stacks_new_shapes_above_existing_ones() {
        let mut editor = EditorState::new(document_of(vec![Shape {
            z: 4.0,
            ..default()
        }]));
        editor.pen = Some(PenDraft {
            path: Path::new([0.0, 0.0]).line_to([10.0, 10.0]),
            pending_out: None,
            dragging: false,
        });

        finish_pen(&mut editor, true);

        assert_eq!(editor.document.shapes[1].z, 5.0);
        assert_eq!(editor.selected, Some(shape_address(&[1])));
    }

    #[test]
    fn pen_drag_shapes_smooth_handles() {
        // Starting from a straight segment, dragging the anchor bends it into a
        // cubic whose incoming control mirrors the outgoing one across the
        // anchor.
        let mut draft = PenDraft {
            path: Path::new([0.0, 0.0]).line_to([10.0, 0.0]),
            pending_out: None,
            dragging: true,
        };

        draft.shape_last_anchor([10.0, 5.0]);

        assert_eq!(
            draft.path.segments[0],
            Segment {
                ctrl1: None,
                ctrl2: Some([10.0, -5.0]),
                to: [10.0, 0.0],
            }
        );
        // The outgoing control is carried into the next segment.
        assert_eq!(draft.pending_out, Some([10.0, 5.0]));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let bounds = Bounds {
            min: [0.0, 0.0],
            max: [400.0, 200.0],
        };
        let mut viewport = Viewport::default();
        let anchor = Vec2::new(300.0, 420.0);

        let before = ViewMapping::new(bounds, 900.0, 900.0, &viewport).screen_to_document(anchor);
        zoom_around(&mut viewport, bounds, 900.0, 900.0, anchor, 2.0);
        let after = ViewMapping::new(bounds, 900.0, 900.0, &viewport).screen_to_document(anchor);

        assert!((viewport.zoom - 2.0).abs() < 1e-5);
        assert!((after[0] - before[0]).abs() < 1e-3);
        assert!((after[1] - before[1]).abs() < 1e-3);
    }

    #[test]
    fn viewport_pan_moves_the_document_with_the_scroll() {
        let bounds = Bounds {
            min: [0.0, 0.0],
            max: [400.0, 200.0],
        };
        let anchor = Vec2::new(450.0, 450.0);
        let mut viewport = Viewport::default();
        let before = ViewMapping::new(bounds, 900.0, 900.0, &viewport).screen_to_document(anchor);

        viewport.pan = Vec2::new(20.0, 10.0);
        let after = ViewMapping::new(bounds, 900.0, 900.0, &viewport).screen_to_document(anchor);
        let scale = fit_transform(bounds, 900.0, 900.0).scale.x;

        assert!((after[0] - (before[0] - 20.0 / scale)).abs() < 1e-3);
        assert!((after[1] - (before[1] - 10.0 / scale)).abs() < 1e-3);
    }

    #[test]
    fn shift_dragging_an_anchor_instantiates_a_control_point() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([10.0, 10.0]);

        // Instantiating the outgoing control of the second segment.
        assert!(apply_bend(&mut path, BendTarget::Outgoing(1), [14.0, 2.0]));
        assert_eq!(
            path.segments[1],
            Segment {
                ctrl1: Some([14.0, 2.0]),
                ctrl2: None,
                to: [10.0, 10.0]
            }
        );

        // Further dragging moves that control only.
        assert!(apply_bend(&mut path, BendTarget::Outgoing(1), [16.0, 4.0]));
        assert_eq!(path.segments[1].ctrl1, Some([16.0, 4.0]));

        // The incoming side puts the control in `ctrl2`.
        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 0.0]);
        assert!(apply_bend(&mut path, BendTarget::Incoming(0), [8.0, -4.0]));
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: None,
                ctrl2: Some([8.0, -4.0]),
                to: [10.0, 0.0]
            }
        );
    }

    #[test]
    fn bend_direction_picks_between_two_segments() {
        let path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([10.0, 10.0]);

        // Dragging toward the second segment picks it.
        assert_eq!(
            choose_bend(&path, Handle::Anchor(0), [0.0, 20.0]),
            Some(BendTarget::Outgoing(1))
        );
        // Dragging back toward the first picks that one instead.
        assert_eq!(
            choose_bend(&path, Handle::Anchor(0), [-20.0, 0.0]),
            Some(BendTarget::Incoming(0))
        );
    }

    #[test]
    fn the_closing_edge_of_a_closed_path_can_be_curved() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([10.0, 10.0])
            .closed();

        // Closing adds a real segment back to the start.
        assert_eq!(path.segments.len(), 3);
        assert_eq!(path.start(), Some([0.0, 0.0]));

        // Dragging the join toward the closing edge picks that edge.
        assert_eq!(
            choose_bend(&path, Handle::Anchor(2), [5.0, 5.0]),
            Some(BendTarget::Incoming(2))
        );
        assert!(apply_bend(&mut path, BendTarget::Incoming(2), [6.0, 4.0]));
        assert_eq!(path.segments[2].ctrl2, Some([6.0, 4.0]));
    }

    #[test]
    fn splitting_a_cubic_uses_de_casteljau() {
        let mut path = Path::new([0.0, 0.0]).cubic_to([0.0, 100.0], [100.0, 100.0], [100.0, 0.0]);

        assert_eq!(split_segment(&mut path, 0, 0.5), Some(0));
        assert_eq!(
            path.segments[0],
            Segment {
                ctrl1: Some([0.0, 50.0]),
                ctrl2: Some([25.0, 75.0]),
                to: [50.0, 75.0],
            }
        );
        assert_eq!(
            path.segments[1],
            Segment {
                ctrl1: Some([75.0, 75.0]),
                ctrl2: Some([100.0, 50.0]),
                to: [100.0, 0.0],
            }
        );
    }

    #[test]
    fn splitting_preserves_the_curve_pointwise() {
        use vello::kurbo::ParamCurve;

        let original = CubicBez::new(
            KurboPoint::new(0.0, 0.0),
            KurboPoint::new(0.0, 100.0),
            KurboPoint::new(100.0, 100.0),
            KurboPoint::new(100.0, 0.0),
        );
        let mut path = Path::new([0.0, 0.0]).cubic_to([0.0, 100.0], [100.0, 100.0], [100.0, 0.0]);

        let t = 0.37;
        assert_eq!(split_segment(&mut path, 0, t), Some(0));

        let first = segment_curve(&path, 0).unwrap();
        let second = segment_curve(&path, 1).unwrap();

        for step in 0..=100 {
            let u = f64::from(step) / 100.0;
            let expected = original.eval(u);
            let actual = if u <= t {
                first.eval(u / t)
            } else {
                second.eval((u - t) / (1.0 - t))
            };
            let dx = expected.x - actual.x;
            let dy = expected.y - actual.y;
            assert!((dx * dx + dy * dy).sqrt() < 1e-3, "curve drifted at u={u}");
        }
    }

    #[test]
    fn splitting_a_line_adds_a_plain_anchor() {
        let mut path = Path::new([0.0, 0.0]).line_to([10.0, 0.0]);

        assert_eq!(split_segment(&mut path, 0, 0.5), Some(0));
        assert_eq!(
            path.segments,
            vec![Segment::line([5.0, 0.0]), Segment::line([10.0, 0.0])]
        );
    }

    #[test]
    fn split_preserves_an_omitted_control() {
        // Only the outgoing control exists on this segment.
        let mut path = Path::new([0.0, 0.0]).cubic_to([0.0, 100.0], [100.0, 100.0], [100.0, 0.0]);
        path.segments[0].ctrl2 = None;

        assert_eq!(split_segment(&mut path, 0, 0.5), Some(0));

        // The first half keeps its missing control near the start; the second
        // half keeps its missing control near the end.
        assert_eq!(path.segments[0].ctrl1, Some([0.0, 50.0]));
        assert_eq!(path.segments[1].ctrl2, None);
    }

    #[test]
    fn the_closing_edge_can_be_split() {
        let mut path = Path::new([0.0, 0.0])
            .line_to([10.0, 0.0])
            .line_to([10.0, 10.0])
            .closed();

        // The closing edge runs from [10, 10] back to the start.
        assert_eq!(split_segment(&mut path, 2, 0.5), Some(2));
        assert_eq!(path.segments.len(), 4);
        assert_eq!(path.segments[2], Segment::line([5.0, 5.0]));
        assert_eq!(path.segments[3], Segment::line([0.0, 0.0]));
        assert_eq!(path.start(), Some([0.0, 0.0]));
    }

    #[test]
    fn nearest_point_finds_the_closest_segment() {
        let path = Path::new([0.0, 0.0])
            .line_to([0.0, 100.0])
            .line_to([100.0, 100.0]);

        let (index, t, distance) = nearest_point_on_path(&path, [0.0, 25.0]).unwrap();
        assert_eq!(index, 0);
        assert!((t - 0.25).abs() < 1e-6);
        assert!(distance < 1e-3);

        let (index, _, _) = nearest_point_on_path(&path, [90.0, 100.0]).unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn inserting_a_point_selects_the_new_anchor() {
        let document = document_of(vec![shaped(
            Path::new([0.0, 0.0]).line_to([100.0, 0.0]),
            stroked(2.0),
        )]);
        let mut editor = EditorState::new(document);

        assert!(insert_point(&mut editor, [50.0, 0.0], 1.0));
        assert_eq!(editor.selected, Some(shape_address(&[0])));
        assert_eq!(
            editor.selected_handle,
            Some(SelectedNode {
                path: 0,
                handle: Handle::Anchor(0)
            })
        );

        let shape = shape_at(&editor.document, &shape_address(&[0])).unwrap();
        let path = &shape.paths[0];
        assert_eq!(path.segments.len(), 2);
        assert_eq!(path.segments[0], Segment::line([50.0, 0.0]));
        assert_eq!(path.segments[1], Segment::line([100.0, 0.0]));
    }

    #[test]
    fn insertion_ignores_the_interior_of_a_filled_shape() {
        let document = document_of(vec![shaped(
            rect(0.0, 0.0, 100.0, 100.0),
            filled(Rgba8::rgb(0, 0, 0)),
        )]);
        let mut editor = EditorState::new(document);

        // Far from any edge: the fill is hit but no point is inserted.
        assert!(!insert_point(&mut editor, [50.0, 50.0], 1.0));
        assert!(editor.selected.is_none());
    }

    #[test]
    fn shift_dragging_a_handle_mirrors_the_neighbouring_handle() {
        let mut path = Path::new([0.0, 0.0])
            .cubic_to([10.0, 0.0], [20.0, 0.0], [30.0, 0.0])
            .cubic_to([40.0, 10.0], [50.0, 20.0], [60.0, 30.0]);

        set_handle(&mut path, Handle::Ctrl2(0), [35.0, 5.0]);
        symmetrize(&mut path, Handle::Ctrl2(0), [35.0, 5.0]);

        // The anchor is [30, 0]; the next segment's control mirrors around it.
        assert_eq!(path.segments[1].ctrl1, Some([25.0, -5.0]));

        // The same happens when dragging from the other side of the anchor.
        set_handle(&mut path, Handle::Ctrl1(1), [36.0, 6.0]);
        symmetrize(&mut path, Handle::Ctrl1(1), [36.0, 6.0]);
        assert_eq!(path.segments[0].ctrl2, Some([24.0, -6.0]));
    }

    #[test]
    fn shift_dragging_a_lone_cubic_leaves_its_other_control_alone() {
        let mut path = Path::new([0.0, 0.0]).cubic_to([10.0, 5.0], [20.0, 5.0], [30.0, 0.0]);

        set_handle(&mut path, Handle::Ctrl1(0), [12.0, 40.0]);
        symmetrize(&mut path, Handle::Ctrl1(0), [12.0, 40.0]);

        // Symmetry is only across a shared node; with no neighbouring segment
        // the same cubic's other control must not move.
        assert_eq!(path.segments[0].ctrl2, Some([20.0, 5.0]));
    }

    #[test]
    fn closed_state_can_be_read_and_written() {
        let mut document = document_of(vec![
            Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([10.0, 0.0])],
                ..default()
            },
            Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([5.0, 5.0])],
                ..default()
            },
        ]);

        let first = shape_address(&[0]);
        assert_eq!(path_closed_at(&document, &first), Some(false));
        assert!(set_path_closed(&mut document, &first, true));
        assert_eq!(path_closed_at(&document, &first), Some(true));

        // Closing adds the edge that joins the last point to the start.
        let path = &document.shapes[0].paths[0];
        assert_eq!(path.start(), Some([0.0, 0.0]));
        assert_eq!(path.segments.len(), 2);
        assert_eq!(path.segments[1], Segment::line([0.0, 0.0]));

        // Opening removes it again.
        assert!(set_path_closed(&mut document, &first, false));
        assert_eq!(document.shapes[0].paths[0].segments.len(), 1);

        // A path address only changes that path.
        let path = path_address(&[1], 0);
        assert!(set_path_closed(&mut document, &path, true));
        assert_eq!(path_closed_at(&document, &path), Some(true));

        // A shape with no paths has no closed state.
        let empty = Shape {
            paths: Vec::new(),
            ..default()
        };
        let mut document = document_of(vec![empty]);
        assert_eq!(path_closed_at(&document, &shape_address(&[0])), None);

        let missing = shape_address(&[9]);
        assert!(!set_path_closed(&mut document, &missing, true));
    }

    #[test]
    fn closing_a_shape_closes_all_of_its_paths() {
        let mut document = document_of(vec![Shape {
            paths: vec![
                Path::new([0.0, 0.0]).line_to([10.0, 0.0]),
                Path::new([0.0, 0.0]).line_to([5.0, 5.0]),
            ],
            ..default()
        }]);

        let shape = shape_address(&[0]);
        assert_eq!(path_closed_at(&document, &shape), Some(false));
        assert!(set_path_closed(&mut document, &shape, true));
        assert_eq!(path_closed_at(&document, &shape), Some(true));
        assert!(document.shapes[0].paths.iter().all(Path::is_closed));
    }

    fn named_shape(point: [f32; 2], name: &str) -> Shape {
        Shape {
            name: name.into(),
            paths: vec![Path::new(point).line_to([point[0] + 10.0, point[1]])],
            style: filled(Rgba8::rgb(0, 0, 0)),
            ..default()
        }
    }

    fn named_path(index: usize, name: &str) -> Path {
        let x = index as f32 * 20.0;
        Path::new([x, 0.0]).line_to([x + 10.0, 0.0]).named(name)
    }

    fn paths_shape(names: &[&str]) -> Shape {
        Shape {
            paths: names
                .iter()
                .enumerate()
                .map(|(index, name)| named_path(index, name))
                .collect(),
            ..default()
        }
    }

    fn path_names(shape: &Shape) -> Vec<String> {
        shape.paths.iter().map(|path| path.name.clone()).collect()
    }

    #[test]
    fn paths_reorder_within_their_shape() {
        let mut document = document_of(vec![paths_shape(&["A", "B", "C"])]);

        // Inserting A after B moves it later in the vector, below B.
        let moved = move_path_near(
            &mut document,
            &path_address(&[0], 0),
            &path_address(&[0], 1),
            true,
        )
        .unwrap();
        assert_eq!(moved, path_address(&[0], 1));
        assert_eq!(path_names(&document.shapes[0]), ["B", "A", "C"]);

        // Inserting C before A moves it back up the vector.
        let moved = move_path_near(
            &mut document,
            &path_address(&[0], 2),
            &path_address(&[0], 1),
            false,
        )
        .unwrap();
        assert_eq!(moved, path_address(&[0], 1));
        assert_eq!(path_names(&document.shapes[0]), ["B", "C", "A"]);
    }

    #[test]
    fn paths_move_next_to_paths_in_another_shape() {
        let mut document = document_of(vec![paths_shape(&["A", "B"]), paths_shape(&["C"])]);

        let moved = move_path_near(
            &mut document,
            &path_address(&[0], 0),
            &path_address(&[1], 0),
            true,
        )
        .unwrap();

        assert_eq!(moved, path_address(&[1], 1));
        assert_eq!(path_names(&document.shapes[0]), ["B"]);
        assert_eq!(path_names(&document.shapes[1]), ["C", "A"]);
    }

    #[test]
    fn paths_only_move_next_to_other_paths() {
        let mut document = document_of(vec![paths_shape(&["A"]), paths_shape(&["B"])]);

        // A shape is not an insertion point for a path.
        assert_eq!(
            move_path_near(
                &mut document,
                &path_address(&[0], 0),
                &shape_address(&[1]),
                true,
            ),
            None
        );
        // Dropping a path on itself is a no-op.
        assert_eq!(
            move_path_near(
                &mut document,
                &path_address(&[0], 0),
                &path_address(&[0], 0),
                true,
            ),
            None
        );
        assert_eq!(path_names(&document.shapes[0]), ["A"]);
        assert_eq!(path_names(&document.shapes[1]), ["B"]);
    }

    #[test]
    fn shapes_reorder_among_siblings() {
        let mut document = document_of(vec![
            named_shape([10.0, 0.0], "A"),
            named_shape([30.0, 0.0], "B"),
        ]);
        let first = shape_address(&[0]);
        let second = shape_address(&[1]);

        let moved = move_shape_near(&mut document, &second, &first, false).unwrap();
        assert_eq!(moved, shape_address(&[0]));
        assert_eq!(document.shapes[0].name, "B");
        assert_eq!(document.shapes[0].z, 0.0);
        assert_eq!(document.shapes[1].z, 1.0);

        // Dropping the front shape after the back one swaps them again.
        let moved = move_shape_near(&mut document, &first, &second, true).unwrap();
        assert_eq!(moved, shape_address(&[1]));
        assert_eq!(document.shapes[0].name, "A");
        assert_eq!(document.shapes[1].name, "B");
    }

    #[test]
    fn shapes_nest_and_move_between_parents() {
        let mut document = document_of(vec![
            named_shape([0.0, 0.0], "Child"),
            Shape {
                name: "Parent".into(),
                paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
                ..default()
            },
        ]);
        let source = shape_address(&[0]);
        let target = shape_address(&[1]);

        let moved = move_shape_into(&mut document, &source, &target).unwrap();
        // Taking the child out of the top level leaves the parent at index 0.
        let parent = shape_address(&[0]);
        assert_eq!(moved, shape_address(&[0, 0]));
        assert_eq!(document.shapes.len(), 1);
        assert_eq!(document.shapes[0].children[0].name, "Child");
        assert_eq!(document.shapes[0].children[0].z, 0.0);

        // Moving it back out puts it beside the parent.
        let moved_out = move_shape_near(&mut document, &moved, &parent, false).unwrap();
        assert_eq!(moved_out, shape_address(&[0]));
        assert_eq!(document.shapes.len(), 2);
        assert_eq!(document.shapes[0].name, "Child");
        assert_eq!(document.shapes[1].name, "Parent");
    }

    #[test]
    fn a_shape_cannot_be_moved_into_its_own_contents() {
        let mut document = document_of(vec![Shape {
            paths: vec![rect(0.0, 0.0, 100.0, 100.0)],
            children: vec![named_shape([5.0, 5.0], "Child")],
            ..default()
        }]);
        let parent = shape_address(&[0]);
        let child = shape_address(&[0, 0]);

        assert_eq!(move_shape_into(&mut document, &parent, &child), None);
        assert_eq!(
            move_shape_near(&mut document, &parent, &parent, false),
            None
        );
        assert_eq!(move_shape_near(&mut document, &parent, &child, true), None);

        assert_eq!(document.shapes[0].children.len(), 1);
    }

    #[test]
    fn paths_move_between_shapes() {
        let mut document = document_of(vec![
            Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0])],
                ..default()
            },
            Shape {
                paths: vec![Path::new([5.0, 5.0]).line_to([6.0, 6.0])],
                ..default()
            },
        ]);
        let source = path_address(&[0], 0);
        let target = shape_address(&[1]);

        let moved = move_path_into_shape(&mut document, &source, &target).unwrap();
        assert_eq!(moved, path_address(&[1], 1));
        assert!(document.shapes[0].paths.is_empty());
        assert_eq!(document.shapes[1].paths.len(), 2);

        // A shape cannot be moved into a path.
        assert_eq!(move_path_into_shape(&mut document, &target, &target), None);
    }

    #[test]
    fn tree_names_can_be_read_and_changed() {
        let mut document = document_of(vec![
            Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([1.0, 1.0]).named("Original")],
                ..default()
            },
            Shape {
                paths: vec![Path::new([0.0, 0.0]).line_to([2.0, 2.0])],
                ..default()
            },
        ]);
        let path = path_address(&[0], 0);
        let shape = shape_address(&[1]);

        assert_eq!(name_at(&document, &path), Some("Original"));
        assert!(rename_at(&mut document, &path, "Curve".into()));
        assert_eq!(name_at(&document, &path), Some("Curve"));

        assert_eq!(name_at(&document, &shape), Some(""));
        assert!(rename_at(&mut document, &shape, "Group".into()));
        assert_eq!(name_at(&document, &shape), Some("Group"));

        let missing = shape_address(&[9]);
        assert!(!rename_at(&mut document, &missing, "Nope".into()));
        assert_eq!(name_at(&document, &missing), None);

        // Committing from the editor trims the buffer and bumps the revision.
        let mut editor = EditorState::new(document);
        editor.begin_rename(path.clone());
        editor.renaming.as_mut().unwrap().buffer = "  Renamed  ".into();
        commit_rename(&mut editor);
        assert!(editor.renaming.is_none());
        assert_eq!(name_at(&editor.document, &path), Some("Renamed"));
        assert_eq!(editor.structure_revision, 1);
    }
}
