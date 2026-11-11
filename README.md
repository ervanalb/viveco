# Viveco

Viveco is a 2D vector-art format and a small Rust toolkit for it. A document is a
tree of shapes made of cubic paths and solid colors, optionally rigged to a bone
skeleton and animated with keyframe curves. Skinning is linear blend skinning,
and rendering goes through [Vello](https://github.com/linebender/vello). The
crate integrates with [Bevy](https://bevyengine.org/) through an asset loader and
a render plugin. This repository also contains an editor application for viveco files.

## Repository layout

| Path | What it is |
| --- | --- |
| `viveco/` | The library crate: document model, file I/O, SVG import, skinning, animation sampling, Vello rendering, and Bevy integration. |
| `viveco/src/bin/convert.rs` | `viveco-convert`, A command-line file type converter. |
| `viveco/examples/` | Runnable Rust examples (`planet`, `compound`, `parallax`). |
| `examples/` | Example viveco assets |
| `viveco-editor/` | The desktop editor for drawing, rigging, and animating. |

## Running things

```sh
# Examples (Bevy is not a default feature, so enable it explicitly)
cargo run -p viveco --features bevy --example planet    # A simple example showing a single looping animation
cargo run -p viveco --features bevy --example compound  # A more complex example showing blended animations
cargo run -p viveco --features bevy --example parallax  # A non-animated scene multi-layer parallax

# Editor
cargo run -p viveco-editor
```

The editor and examples open windows and need a working GPU/display. For faster
incremental builds, enable Bevy's own `dynamic_linking` feature directly:
`cargo run -p viveco-editor --features bevy/dynamic_linking`.

### Converter

Viveco files can be binary or JSON. Binary is recommended for smaller file sizes and quicker loading.

The `viveco-convert` command-line tool can convert files from binary to JSON and vice-versa,
as well as converting SVG to non-animated viveco.

```sh
# SVG -> binary .viveco (the output extension selects the encoding)
cargo run -p viveco --bin viveco-convert -- art.svg art.viveco

# Convert one named top-level SVG group, as Inkscape layers are
cargo run -p viveco --bin viveco-convert -- --group Front art.svg front.viveco

# Binary -> JSON
cargo run -p viveco --bin viveco-convert -- art.viveco art.json
```

## The Format

### Coordinates and geometry

- The origin is at the top-left and Y points down. There are no "units" or canvas size.
- A shape is made of `paths`. The shape is filled according to a non-zero winding rule,
  allowing for holes.
- Paths may be open or closed, and are cubic bezier splines.
- Shapes may be filled and/or stroked with a solid color & opacity.
- Stroke width is left unspecified--this allows setting a resolution-dependent stroke width at render time.
  If a constant stroke width is desired, it must be baked into the file as a filled shape (à la "stroke to path" in inkscape)
- Shapes have a `z` coordinate, which sets render order among siblings.
- Shapes are arranged in a tree. Child shapes are clipped to their parent's interior
  and paint on top of the parent's fill but below its stroke.

### Bones and skinning

Bones are arranged in a tree. Parent bones move child bones.
Every path point carries a weight list, which specifies the bones that affect it, and by how much.
A point moves to the weighted blend of where each influencing bone's transform puts it.

### Animations

Each animation has a `duration` in seconds, a `cyclic` flag, and a list of
keyframes ordered by `timestamp`. The following properties can be animation targets:

- bone translation (x and y)
- bone rotation
- bone scale (uniform)
- shape Z (so paint order can change over the course of an animation.)

Animation curves are 1D cubic Bézier curves, with optional control handles accompanying each keyframe.
A cyclic animation interpolates animation curves across the seam.
Multiple animations can be sampled and additively blended together at runtime.

## The library

Enable the features you need:

| Feature | Adds |
| --- | --- |
| `svg` (default) | `viveco::svg` and `viveco::convert`: SVG shape import and conversion. |
| `vello` (default) | `viveco::render`: converts assets into Vello scenes. |
| `bevy` | The Bevy asset loader, `VivecoPlugin`, scenes and instances, and parallax. |

For faster incremental builds, enable Bevy's `dynamic_linking` feature in your
own application (for example with `--features bevy/dynamic_linking`). Viveco
does not re-export it: Cargo feature unification applies that setting to the
whole dependency graph, so it reaches Viveco's Bevy integration automatically.

Core operations:

```rust
use vello::Scene;
use vello::kurbo::Affine;
use viveco::Asset;
use viveco::render::SceneBuilder;

let asset = Asset::load_from_file("art.viveco")?;

// Sample animation[0] at t = 1.5 s with full weight, then resolve the skin.
let pose = asset.sample_pose(&[(0, 1.5, 1.0)]);
let world = pose.to_world(&asset);

// Paint the skinned asset. `stroke_width` is in asset units.
let mut scene = Scene::new();
let mut builder = SceneBuilder::new();
builder.add_asset_with_world_pose(&asset, &world, 1.0, Affine::IDENTITY);
builder.finish(&mut scene);
```

`Asset::sample_pose` takes `AnimationInstance` values, i.e.
`(animation_index, time_seconds, weight)`. `SceneBuilder` is the lower-level API
for combining several assets and poses into one Vello scene; `Asset::into_scene`
is a shortcut for painting an asset at its rest pose.

For Bevy, `VivecoPlugin` registers the `.viveco` asset loader and draws
`VivecoInstance`s onto a `VivecoScene` for rendering.
Each instance specifies its animation mix.

Optionally, `ParallaxLayer` and `apply_parallax` add depth-based
scaling. See the crate documentation and the examples for complete setups.

### Build-script helpers

The library exposes the conversion and code-generation functions for `build.rs`:

```rust
// build.rs
fn main() {
    viveco::codegen::emit_asset_constants("assets/stick.viveco", "stick")
        .expect("generate constants");
}
```

```rust
// src/main.rs
mod stick {
    include!(concat!(env!("OUT_DIR"), "/stick.rs"));
}

// Named indices of animations, shapes, and bones are now exposed as constants,
// along with animation durations:
// stick::animations::WALK, WALK_DURATION, stick::shapes::HEAD, stick::bones::BODY, ...
```

If you want to keep assets in SVG format,
`viveco::convert::convert_file` can be used in the build script to convert them
as part of the build process.

## The editor

`viveco-editor` opens, edits, and saves viveco assets. It can also import SVG files. It has three modes:

- **Draw** — select, move, and reshape paths; draw with the pen tool.
- **Rig** — build and edit the bone chain, paint vertex weights, and try out different poses.
- **Animate** — create animations by keyframing bone transforms or shape Z.
