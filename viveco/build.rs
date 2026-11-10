//! Generates the assets and constants used by the examples.
//!
//! A package cannot use its own library from its build script (Cargo rejects
//! the self-dependency), so the core model, the SVG importer, the converter,
//! and the `codegen` module are compiled from source here too. Downstream games
//! use the same helpers as `viveco::codegen::emit_asset_constants` and
//! `viveco::convert::convert_file` from their own build scripts.

#[path = "src/animation.rs"]
mod animation;
#[path = "src/asset.rs"]
mod asset;
#[path = "src/bone.rs"]
mod bone;
#[allow(dead_code)]
#[path = "src/convert.rs"]
mod convert;
#[path = "src/pose.rs"]
mod pose;
#[path = "src/shape.rs"]
mod shape;
#[path = "src/svg.rs"]
mod svg;

pub use animation::*;
pub use asset::*;
pub use bone::*;
pub use pose::*;
pub use shape::*;

#[path = "src/codegen.rs"]
mod codegen;

fn main() {
    emit_stick_constants();
    convert_landscape_layers();
}

/// Generates the animation, shape, and bone constants used by
/// `examples/compound.rs`.
fn emit_stick_constants() {
    let asset = "examples/assets/stick.viveco";
    println!("cargo:rerun-if-changed={asset}");

    // Packages built without the example assets still build normally.
    if !std::path::Path::new(asset).exists() {
        return;
    }

    codegen::emit_asset_constants(asset, "stick").expect("failed to generate asset constants");
}

/// Converts the landscape artwork into one asset per layer for
/// `examples/parallax.rs`.
///
/// The layers are written to `$OUT_DIR/assets`, which the example uses as its
/// asset server root.
fn convert_landscape_layers() {
    let source = std::path::Path::new("examples/assets/landscape.svg");
    println!("cargo:rerun-if-changed={}", source.display());

    let assets = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("assets");
    std::fs::create_dir_all(&assets).expect("failed to create the generated asset directory");

    for (group, file) in [
        ("Back", "landscape_back.viveco"),
        ("Mid", "landscape_mid.viveco"),
        ("Front", "landscape_front.viveco"),
    ] {
        let output = assets.join(file);
        convert::convert_file(
            source,
            &output,
            &convert::ConvertOptions::new().with_group(group),
        )
        .unwrap_or_else(|err| panic!("failed to convert {}: {err}", source.display()));
    }
}
