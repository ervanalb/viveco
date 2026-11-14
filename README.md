# Viveco

Viveco is an animated 2D vector-art format and a small Rust toolkit for it.
A viveco document consists of shapes made of cubic Bézier paths stroked and filled with solid colors,
optionally rigged to a skeleton with linear blend skinning, and animated with keyframes.
Designed to be rendered with [Vello](https://github.com/linebender/vello)
and integrated into [Bevy](https://bevyengine.org/) games using a provided asset loader and a renderer.
This repository also contains an editor application for authoring viveco files.

## Repository layout

| Path | What it is |
| --- | --- |
| `viveco/` | The library crate: document model, file I/O, SVG import, skinning, animation sampling, Vello rendering, and Bevy integration. |
| `viveco/src/bin/convert.rs` | `viveco-convert`, A command-line file type converter. |
| `viveco/examples/` | Runnable Rust examples (`planet`, `compound`, `parallax`). |
| `examples/` | Example viveco assets |
| `viveco-editor/` | The desktop editor for drawing, rigging, and animating. |

## Running

### Examples
Bevy is not a default feature, so enable it explicitly

```sh
cargo run -p viveco --features bevy --example planet    # A simple example showing a single looping animation
cargo run -p viveco --features bevy --example compound  # A more complex example showing blended animations
cargo run -p viveco --features bevy --example parallax  # A non-animated scene with multi-layer parallax, sourced from a SVG
```

### Editor

```sh
# Installs the `viveco-editor` executable
cargo install --path viveco-editor

# If you don't want to install it, you can use:
# cargo run -p viveco-editor
```

### Converter

Viveco files can be binary or JSON. Binary is recommended for smaller file sizes and quicker loading.

The `viveco-convert` command-line tool can convert files from binary to JSON and vice-versa,
as well as converting SVG files to non-animated viveco.

```sh
# Installs the `viveco-convert` executable
cargo install --path viveco

# If you don't want to install it, you can use:
# cargo run -p viveco --bin viveco-convert --
```

```sh
# SVG -> binary .viveco
viveco-convert art.svg art.viveco

# Convert one named top-level SVG group, e.g. an Inkscape layer
viveco-convert --group Front art.svg front.viveco

# Binary -> JSON
viveco-convert art.viveco art.json
```

## The Format

### Coordinates and geometry

- The origin is at the top-left and Y points down. There are no real-world units or canvas size.
- A shape is made of `paths`. The shape is filled according to a non-zero winding rule,
  allowing for holes.
- Paths may be open or closed, and are cubic Bézier splines.
- Shapes may be filled and/or stroked with a solid color & opacity.
- Stroke width is left unspecified--this allows setting a resolution-dependent stroke width at render time.
  If a constant stroke width is desired, it must be baked into the file as a filled shape (à la "stroke to path" in Inkscape)
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
use viveco::render::{SceneBuilder, SceneInstance};

let asset = Asset::load_from_file("character.viveco")?;

// Draw the asset, with a pose defined by animation[0] at t=1.5 with full weight (1.0).
// Draw at (120, 40) with a stroke width of 2.
let instance = SceneInstance::animated(&asset, &[(0, 1.5, 1.0)])
    .with_transform(Affine::translate((120.0, 40.0)))
    .with_stroke_width(2.0);

let mut scene = Scene::new();
let mut builder = SceneBuilder::new();
builder.add_instance(&instance);
builder.finish(&mut scene);
```

### Bevy features

For Bevy, `VivecoPlugin` registers the `.viveco` asset loader and draws
`VivecoInstance`s onto a `VivecoScene` for rendering.
Each instance specifies its animation mix,
and the renderer abides by the built-in `Transform` and `Visibility` components.

```rust
use bevy::prelude::*;
use bevy_vello::prelude::VelloView;
use bevy_vello::VelloPlugin;
use viveco::{Asset, VivecoDefaultScene, VivecoInstance, VivecoPlugin, VivecoScene};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(VelloPlugin::default())
        .add_plugins(VivecoPlugin)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((Camera2d, VelloView));

    // Spawn a default viveco scene
    commands.spawn((VivecoScene::default(), VivecoDefaultScene));

    // Load the asset
    let asset = asset_server.load::<Asset>("character.viveco");

    // animation[0] at t=1.5 with full weight (1.0), stroke width 2,
    // placed at (120, 40).
    commands.spawn((
        VivecoInstance::new(asset, vec![(0, 1.5, 1.0)]).with_stroke_width(2.0),
        Transform::from_xyz(120.0, 40.0, 0.0),
    ));
}
```

The mixer sets which animation(s) are active and how they are blended.
Animations do not advance automatically; each animation's `t` must be driven forward each frame.

The graphics can be transformed through two separate mechanisms.
The first way is by adding the Bevy `Transform` component.
This transform is generally used to dynamically place the entity within a scene,
e.g. as the result of a physics system.
The second way is by using the `transform` field on `VivecoInstance` (or `.with_transform()`).
This transform is meant to scale the artwork to game units, or position it with respect to the entity's logical center.
It is generally static (although varying it is certainly allowed.)
Note that the type of this transform is a Vello `Affine` and operates in the asset's Y-down coordinate space.

`VivecoInstance::stroke_width` is in the asset's local units and scales with the
instance, its scene, and the camera.
If you want to specify a stroke width in pixels, add `VivecoStrokeWidthInPixels::new(2.0)`.
This component will undo the instance, scene & camera scales to produce a stroke width that stays constant
through scaling & zooming. In this case it would stay at 2 pixels wide.

For blending multiple animations, you may find `AccelEasing` useful for setting weights.
`AccelEasing` eases a weight toward a target with bounded speed and acceleration.
Call `easing.step(target_weight, delta_seconds)` each frame and use `easing.value` as
the weight. See the `compound` example for more details.

Optionally, `ParallaxLayer` and `apply_parallax` add depth-based
scaling. See the `parallax` example for more details.

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
