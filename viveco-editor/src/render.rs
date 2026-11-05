//! Converts an [`Asset`](viveco::Asset) into a Vello [`Scene`].
//!
//! The implementation lives in the `viveco` crate so that other integrations
//! can reuse it; this module re-exports it under the editor's existing paths.

pub use viveco::render::*;
