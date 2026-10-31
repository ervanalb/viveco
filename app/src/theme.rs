//! Shared palette for the editor's UI panels and canvas.
//!
//! Centralizing these colors keeps the toolbar, sidebar, style panel, and
//! canvas backgrounds consistent and gives future UI code a single place to
//! pull from.

use bevy::prelude::Color;

/// Translucent background used by every floating panel.
pub const PANEL_BG: Color = Color::srgba_u8(0x12, 0x15, 0x1c, 0xf2);

/// Opaque background drawn behind the document.
pub const CANVAS_BG: Color = Color::srgb_u8(0x1a, 0x1d, 0x24);

/// Primary text color.
pub const TEXT: Color = Color::srgb_u8(0xdd, 0xe1, 0xe8);

/// Secondary text: headings, hints, and slider labels.
pub const MUTED: Color = Color::srgb_u8(0x8b, 0x93, 0xa1);

/// Solid highlight used for active toggles and selections.
pub const ACCENT: Color = Color::srgb_u8(0x4c, 0xc9, 0xf0);

/// Faint accent used for hover feedback.
pub const ACCENT_FAINT: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x26);

/// Accent used for selected row and active button backgrounds.
pub const ACCENT_SELECTED: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x59);

/// Stronger accent used for filled slider tracks.
pub const ACCENT_STRONG: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0xc0);

/// Plain white, used for slider thumbs and swatches.
pub const WHITE: Color = Color::srgb_u8(0xff, 0xff, 0xff);

/// Neutral translucent fill for buttons.
pub const BUTTON_BG: Color = Color::srgba_u8(0xff, 0xff, 0xff, 0x0a);

/// Neutral translucent fill for slider tracks.
pub const TRACK_BG: Color = Color::srgba_u8(0xff, 0xff, 0xff, 0x14);

/// Faint white border around buttons.
pub const BORDER_FAINT: Color = Color::srgba_u8(0xff, 0xff, 0xff, 0x1f);

/// White border around toggle checkboxes and swatches.
pub const BORDER: Color = Color::srgba_u8(0xff, 0xff, 0xff, 0x40);
