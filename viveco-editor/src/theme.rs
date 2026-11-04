//! Shared palette for the editor's UI panels and canvas.
//!
//! Centralizing these colors keeps the toolbar, sidebar, style panel, and
//! canvas backgrounds consistent and gives future UI code a single place to
//! pull from.

use bevy::prelude::Color;
use bevy_vello::prelude::vello::peniko::Color as InkColor;

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

/// Background of a tree row that would receive the dragged node.
///
/// Stronger than [`ACCENT_FAINT`] and [`ACCENT_SELECTED`] so a drop target is
/// unmistakable next to ordinary hover feedback.
pub const DROP_TARGET: Color = Color::srgba_u8(0x4c, 0xc9, 0xf0, 0x80);

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

/// Palette for animation curves and the bones they drive.
const CURVE_COLORS: [InkColor; 10] = [
    InkColor::from_rgba8(0x4c, 0xc9, 0xf0, 0xff),
    InkColor::from_rgba8(0xf0, 0xb4, 0x4c, 0xff),
    InkColor::from_rgba8(0x8a, 0xe0, 0x8a, 0xff),
    InkColor::from_rgba8(0xf0, 0x7a, 0x9c, 0xff),
    InkColor::from_rgba8(0xb4, 0x8a, 0xf0, 0xff),
    InkColor::from_rgba8(0x7a, 0xd8, 0xd0, 0xff),
    InkColor::from_rgba8(0xf0, 0x8a, 0x6a, 0xff),
    InkColor::from_rgba8(0xc8, 0xe0, 0x5a, 0xff),
    InkColor::from_rgba8(0x6b, 0x9a, 0xf0, 0xff),
    InkColor::from_rgba8(0xe0, 0x70, 0xc0, 0xff),
];

/// The color of the curves that animate `bone`.
///
/// The color comes from the bone's name rather than its position among the
/// animation's curves, so it stays the same when curves are added, removed, or
/// reordered, and a bone has the same color in every animation.
pub fn curve_color(bone: &str) -> InkColor {
    CURVE_COLORS[name_hash(bone) % CURVE_COLORS.len()]
}

/// The palette entry for a curve at `index`, used for targets that name no
/// bone.
pub fn curve_color_at(index: usize) -> InkColor {
    CURVE_COLORS[index % CURVE_COLORS.len()]
}

/// A desaturated version of `color`, so skeleton bones echo their curve's
/// color without competing with the artwork.
pub fn muted(color: InkColor) -> InkColor {
    let rgba = color.to_rgba8();
    let grey = (u16::from(rgba.r) + u16::from(rgba.g) + u16::from(rgba.b)) as f32 / 3.0;
    let mix = |value: u8| (f32::from(value) * 0.65 + grey * 0.35).round() as u8;
    InkColor::from_rgba8(mix(rgba.r), mix(rgba.g), mix(rgba.b), rgba.a)
}

/// The fill and stroke colors of `bone` in the skeleton overlay.
pub fn bone_overlay_colors(bone: &str) -> (InkColor, InkColor) {
    let rgba = muted(curve_color(bone)).to_rgba8();
    (
        InkColor::from_rgba8(rgba.r, rgba.g, rgba.b, 0xcc),
        InkColor::from_rgba8(rgba.r, rgba.g, rgba.b, 0xff),
    )
}

/// FNV-1a, so a name always maps to the same palette entry.
fn name_hash(name: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize
}
