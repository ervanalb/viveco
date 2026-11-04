//! Vello rendering and the editor UI for Viveco documents.
//!
//! The document format itself lives in the `viveco` crate; this crate adds the
//! interactive editor around it.

pub mod animate;
pub mod animate_panel;
pub mod animate_sidebar;
pub mod clipboard;
pub mod dialogs;
pub mod editor;
pub mod file;
pub mod history;
pub mod menu_bar;
pub mod mode_tabs;
pub mod render;
pub mod rig;
pub mod rig_panel;
pub mod rig_sidebar;
pub mod sidebar;
pub mod style_panel;
pub mod theme;
pub mod timeline;
pub mod toolbar;
pub mod tree;
pub mod weight_paint;
