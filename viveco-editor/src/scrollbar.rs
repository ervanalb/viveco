//! Scrollbars for the right-hand panels.
//!
//! The object tree, the skeleton tree, and the animate-mode animation and
//! shape lists all live in the fixed-width column on the right. Their contents
//! can grow taller than the window, so each list is a scroll container with a
//! thin scrollbar in a gutter beside it. Bevy handles the wheel
//! ([`ScrollArea`]) and the thumb's drag ([`Scrollbar`]); this module lays the
//! pair out and keeps the thumb hidden while the list fits.

use bevy::prelude::*;
use bevy::ui::{RelativeCursorPosition, UiSystems, ui_layout_system};
use bevy::ui_widgets::{ControlOrientation, ScrollArea, Scrollbar, ScrollbarThumb};

use crate::theme;

/// Width of the scrollbar gutter, in logical pixels.
pub const SCROLLBAR_WIDTH: f32 = 8.0;

/// Shortest length the scrollbar thumb gets, in logical pixels.
const MIN_THUMB_LENGTH: f32 = 24.0;

/// A scrollbar spawned by [`spawn_scrollable_list`].
#[derive(Component)]
pub struct PanelScrollbar;

/// The moving part of a [`PanelScrollbar`].
#[derive(Component)]
struct PanelScrollbarThumb;

pub struct PanelScrollbarPlugin;

impl Plugin for PanelScrollbarPlugin {
    fn build(&self, app: &mut App) {
        // Run next to bevy's own scrollbar thumb sizing, once the lists have
        // been laid out for this frame.
        app.add_systems(
            PostUpdate,
            hide_inactive_scrollbars
                .in_set(UiSystems::Layout)
                .after(ui_layout_system),
        );
    }
}

/// Spawns a vertically scrolling list with a scrollbar beside it.
///
/// `marker` identifies the list for the caller's rebuild system. The rows are
/// spawned later, so the list starts empty. `row_gap` is the space between
/// those rows.
///
/// The list is a column flex container with a fixed height, so its direct
/// children must set `flex_shrink: 0.0`; otherwise they compress to fit and
/// the list never overflows.
pub fn spawn_scrollable_list<M: Component>(
    parent: &mut ChildSpawnerCommands,
    marker: M,
    row_gap: f32,
) {
    parent
        .spawn(Node {
            flex_grow: 1.0,
            flex_basis: Val::Px(0.0),
            // A column flex item's automatic minimum height is its content
            // height unless it is overridden, which would let the row grow
            // past the panel and stop the list from ever overflowing.
            min_height: Val::Px(0.0),
            flex_direction: FlexDirection::Row,
            ..default()
        })
        .with_children(|row| {
            let list = row
                .spawn((
                    Node {
                        flex_grow: 1.0,
                        flex_basis: Val::Px(0.0),
                        min_width: Val::Px(0.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(row_gap),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollArea,
                    marker,
                ))
                .id();

            row.spawn((
                PanelScrollbar,
                Node {
                    width: Val::Px(SCROLLBAR_WIDTH),
                    flex_shrink: 0.0,
                    ..default()
                },
                RelativeCursorPosition::default(),
                Scrollbar::new(list, ControlOrientation::Vertical, MIN_THUMB_LENGTH),
            ))
            .with_children(|bar| {
                bar.spawn((
                    PanelScrollbarThumb,
                    ScrollbarThumb {
                        border: UiRect::ZERO,
                        border_radius: BorderRadius::all(Val::Px(SCROLLBAR_WIDTH * 0.5)),
                    },
                    BackgroundColor(theme::BORDER),
                ));
            });
        });
}

/// Shows a scrollbar's thumb only while its list is taller than the visible
/// area, so a list that fits reads as a plain list rather than an empty track.
fn hide_inactive_scrollbars(
    scrollbars: Query<(&Scrollbar, &Children), With<PanelScrollbar>>,
    nodes: Query<&ComputedNode>,
    mut thumbs: Query<&mut Visibility, With<PanelScrollbarThumb>>,
) {
    for (scrollbar, children) in &scrollbars {
        let Ok(node) = nodes.get(scrollbar.target) else {
            continue;
        };
        let visible = node.size() * node.inverse_scale_factor;
        let content = node.content_size() * node.inverse_scale_factor;
        let overflows = match scrollbar.orientation {
            ControlOrientation::Vertical => content.y > visible.y,
            ControlOrientation::Horizontal => content.x > visible.x,
        };
        let visibility = if overflows {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        for child in children {
            if let Ok(mut thumb) = thumbs.get_mut(*child)
                && *thumb != visibility
            {
                *thumb = visibility;
            }
        }
    }
}
