//! Generates the animation constants used by `examples/animate.rs`.
//!
//! A package cannot use its own library from its build script (Cargo rejects
//! the self-dependency), so the core model and the `codegen` module are
//! compiled from source here too. Downstream games use the same helper as
//! `viveco::codegen::emit_asset_constants` from their own build scripts.

#[path = "src/animation.rs"]
mod animation;
#[path = "src/asset.rs"]
mod asset;
#[path = "src/bone.rs"]
mod bone;
#[path = "src/pose.rs"]
mod pose;
#[path = "src/shape.rs"]
mod shape;

pub use animation::*;
pub use asset::*;
pub use bone::*;
pub use pose::*;
pub use shape::*;

#[path = "src/codegen.rs"]
mod codegen;

fn main() {
    let asset = "examples/assets/stick.viveco";
    println!("cargo:rerun-if-changed={asset}");

    // Packages built without the example assets still build normally.
    if !std::path::Path::new(asset).exists() {
        return;
    }

    codegen::emit_asset_constants(asset, "stick").expect("failed to generate asset constants");
}
