//! Inspects and converts Viveco assets and imports SVG documents.
//!
//! The `viveco` command has two modes. The default (or explicit `convert`) mode
//! reads an asset, optionally keeps only some top-level shapes, optionally
//! moves the origin and applies a scale, rotation, and translation, and writes
//! the result. The `info` mode prints a high-level summary instead: the shape
//! and bone hierarchies (with each shape's path bounds), the animations, and
//! the artwork's bounds at rest. Use it to inspect an asset rather than
//! converting the file to JSON.
//!
//! The input kind is detected from the file's contents (with a little help
//! from its extension): an SVG document is imported as art, and a Viveco asset
//! is decoded in whichever encoding it uses. The output encoding is chosen
//! from its extension: `.json` writes JSON and anything else writes the binary
//! format. When no output path is given, one is derived from the input.
//!
//! When converting an SVG, `--group <name>` imports only the objects inside
//! the top-level `<g>` with that name. Groups are named by their
//! `inkscape:label` when they have one, and by their `id` otherwise.
//!
//! The conversion itself lives in [`viveco::convert`], so build scripts can
//! run it too.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use viveco::convert::{self, ConvertOptions, Origin, TransformOptions};
use viveco::{Asset, Bone, Point, Shape};

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args_os().skip(1)) {
        Ok(Some(args)) => args,
        // `--help` was printed.
        Ok(None) => return ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            usage();
            return ExitCode::FAILURE;
        }
    };

    let result = match args.mode {
        Mode::Convert => convert(&args),
        Mode::Info => info(&args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Convert,
    Info,
}

/// The command line.
#[derive(Debug)]
struct Args {
    mode: Mode,
    input: PathBuf,
    output: Option<PathBuf>,
    /// Import only the top-level SVG group with this name.
    group: Option<String>,
    /// Keep only these top-level shape names.
    keep: Vec<String>,
    /// The transform to apply after filtering.
    transform: TransformOptions,
    /// Move the filtered artwork so this bounds anchor is the origin.
    origin: Option<Origin>,
}

impl Args {
    /// Parses the command line. Returns `Ok(None)` when help was printed.
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Option<Self>, String> {
        let mut args = args.peekable();
        let mut mode = Mode::Convert;
        // A leading `info` or `convert` selects the mode. With neither, the
        // first positional is the input, which keeps the original
        // `convert <input> [output]` form working.
        if let Some(first) = args.peek().and_then(|arg| arg.to_str()) {
            match first {
                "info" => {
                    args.next();
                    mode = Mode::Info;
                }
                "convert" => {
                    args.next();
                }
                _ => {}
            }
        }

        let mut input = None;
        let mut output = None;
        let mut group = None;
        let mut keep = Vec::new();
        let mut transform = TransformOptions::new();
        let mut origin = None;
        // Tracked separately from the transform's value so an explicit
        // `--scale 1` still counts as asking for a transform.
        let mut transform_used = false;

        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("-h" | "--help") => {
                    usage();
                    return Ok(None);
                }
                Some("--group" | "--layer") => {
                    let value = take_value(&mut args, "--group")?;
                    group = Some(value.to_string_lossy().into_owned());
                }
                Some("--keep") => {
                    let value = take_value(&mut args, "--keep")?;
                    extend_names(&mut keep, option_text(&value, "--keep")?)?;
                }
                Some("--translate") => {
                    let value = take_value(&mut args, "--translate")?;
                    transform.translate = parse_point(option_text(&value, "--translate")?)?;
                    transform_used = true;
                }
                Some("--scale") => {
                    let value = take_value(&mut args, "--scale")?;
                    transform.scale = parse_scale(option_text(&value, "--scale")?)?;
                    transform_used = true;
                }
                Some("--rotate") => {
                    let value = take_value(&mut args, "--rotate")?;
                    transform.rotate = parse_number(option_text(&value, "--rotate")?)?.to_radians();
                    transform_used = true;
                }
                Some("--origin") => {
                    let value = take_value(&mut args, "--origin")?;
                    origin = Some(option_text(&value, "--origin")?.parse::<Origin>()?);
                }
                Some(text) if text.starts_with("--") && text.contains('=') => {
                    let (option, value) = text.split_once('=').expect("the text contains `=`");
                    apply_assignment(
                        option,
                        value,
                        &mut group,
                        &mut keep,
                        &mut transform,
                        &mut transform_used,
                        &mut origin,
                    )?;
                }
                Some(text) if text.starts_with('-') && text != "-" => {
                    return Err(format!("unknown option `{text}`"));
                }
                _ => push_positional(arg, &mut input, &mut output)?,
            }
        }

        let Some(input) = input else {
            return Err("missing input path".to_owned());
        };

        if mode == Mode::Info {
            if output.is_some() {
                return Err("info takes no output path".to_owned());
            }
            if !keep.is_empty() {
                return Err("--keep only applies to conversions".to_owned());
            }
            if transform_used || origin.is_some() {
                return Err("transform and origin options only apply to conversions".to_owned());
            }
        }

        Ok(Some(Self {
            mode,
            input,
            output,
            group,
            keep,
            transform,
            origin,
        }))
    }
}

/// Consumes the value of an option that takes one.
fn take_value(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

/// Borrows an option value as text, rejecting non-UTF-8 values.
fn option_text<'a>(value: &'a OsString, option: &str) -> Result<&'a str, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{option} value is not valid UTF-8"))
}

/// Applies the `--option=value` form.
#[allow(clippy::too_many_arguments)]
fn apply_assignment(
    option: &str,
    value: &str,
    group: &mut Option<String>,
    keep: &mut Vec<String>,
    transform: &mut TransformOptions,
    transform_used: &mut bool,
    origin: &mut Option<Origin>,
) -> Result<(), String> {
    match option {
        "--group" | "--layer" => {
            if value.is_empty() {
                return Err("--group requires a group name".to_owned());
            }
            *group = Some(value.to_owned());
        }
        "--keep" => extend_names(keep, value)?,
        "--translate" => {
            transform.translate = parse_point(value)?;
            *transform_used = true;
        }
        "--scale" => {
            transform.scale = parse_scale(value)?;
            *transform_used = true;
        }
        "--rotate" => {
            transform.rotate = parse_number(value)?.to_radians();
            *transform_used = true;
        }
        "--origin" => *origin = Some(value.parse::<Origin>()?),
        _ => return Err(format!("unknown option `{option}`")),
    }
    Ok(())
}

/// Adds a comma-separated list of shape names.
fn extend_names(names: &mut Vec<String>, value: &str) -> Result<(), String> {
    for name in value.split(',') {
        let name = name.trim();
        if name.is_empty() {
            return Err("--keep requires a non-empty shape name".to_owned());
        }
        names.push(name.to_owned());
    }
    Ok(())
}

/// Parses an `x,y` point.
fn parse_point(text: &str) -> Result<Point, String> {
    let (x, y) = text
        .split_once(',')
        .ok_or_else(|| format!("expected a point `x,y`, found `{text}`"))?;
    Ok([parse_number(x)?, parse_number(y)?])
}

/// Parses a finite number.
fn parse_number(text: &str) -> Result<f32, String> {
    let value = text
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("expected a number, found `{text}`"))?;
    if !value.is_finite() {
        return Err(format!("expected a finite number, found `{text}`"));
    }
    Ok(value)
}

/// Parses a positive, finite scale factor.
fn parse_scale(text: &str) -> Result<f32, String> {
    let value = parse_number(text)?;
    if value <= 0.0 {
        return Err(format!("expected a positive scale, found `{text}`"));
    }
    Ok(value)
}

/// Stores a positional argument as the input or the output.
fn push_positional(
    arg: OsString,
    input: &mut Option<PathBuf>,
    output: &mut Option<PathBuf>,
) -> Result<(), String> {
    if input.is_none() {
        *input = Some(PathBuf::from(arg));
    } else if output.is_none() {
        *output = Some(PathBuf::from(arg));
    } else {
        return Err("too many positional arguments".to_owned());
    }
    Ok(())
}

fn convert(args: &Args) -> Result<(), String> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| convert::default_output(&args.input));
    let options = ConvertOptions {
        group: args.group.clone(),
        keep: args.keep.clone(),
        transform: args.transform,
        origin: args.origin,
    };

    convert::convert_file(&args.input, &output, &options).map_err(|err| err.to_string())?;

    println!("wrote {}", output.display());
    Ok(())
}

fn info(args: &Args) -> Result<(), String> {
    let options = ConvertOptions {
        group: args.group.clone(),
        ..Default::default()
    };
    let asset = convert::load_asset(&args.input, &options).map_err(|err| err.to_string())?;
    print!("{}", info_text(&args.input, &asset));
    Ok(())
}

/// Renders the high-level summary printed by `info`.
fn info_text(path: &Path, asset: &Asset) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", path.display());
    let _ = writeln!(
        out,
        "  shapes: {} top-level, {} total",
        asset.shapes.len(),
        asset.shape_count()
    );
    let _ = writeln!(out, "  bones: {}", asset.bone_count());
    let _ = writeln!(out, "  animations: {}", asset.animations.len());
    out.push('\n');

    let _ = writeln!(out, "Shapes:");
    if asset.shapes.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        write_shapes(&mut out, &asset.shapes, 1);
    }
    out.push('\n');

    let _ = writeln!(out, "Bones:");
    if asset.bones.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        write_bones(&mut out, &asset.bones, 1);
    }
    out.push('\n');

    let _ = writeln!(out, "Animations:");
    if asset.animations.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        for (index, animation) in asset.animations.iter().enumerate() {
            let name = display_name(&animation.name);
            let cyclic = if animation.cyclic {
                "cyclic"
            } else {
                "not cyclic"
            };
            let _ = writeln!(
                out,
                "  [{index}] \"{name}\": {}s, {cyclic}",
                number(animation.duration)
            );
        }
    }
    out.push('\n');

    let _ = writeln!(out, "Bounds (at rest):");
    match asset.bounds() {
        Some(bounds) => {
            let _ = writeln!(
                out,
                "  min {}, max {}",
                display_point(bounds.min),
                display_point(bounds.max)
            );
            let _ = writeln!(
                out,
                "  size {} x {}, center {}",
                number(bounds.width()),
                number(bounds.height()),
                display_point(bounds.center())
            );
        }
        None => {
            let _ = writeln!(out, "  none (no artwork)");
        }
    }

    out
}

/// Writes a shape hierarchy, one shape per line.
///
/// Each line lists the shape's paths' bounds, which are the geometry the shape
/// fills, strokes, and clips its children against.
fn write_shapes(out: &mut String, shapes: &[Shape], depth: usize) {
    let indent = "  ".repeat(depth);
    for shape in shapes {
        let bounds = match shape.path_bounds() {
            Some(bounds) => format!(
                ", bounds {} to {}",
                display_point(bounds.min),
                display_point(bounds.max)
            ),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "{indent}\"{}\" ({}, {}{bounds})",
            display_name(&shape.name),
            paths_label(shape.paths.len()),
            children_label(shape.children.len())
        );
        write_shapes(out, &shape.children, depth + 1);
    }
}

/// Writes a bone hierarchy, one bone per line.
fn write_bones(out: &mut String, bones: &[Bone], depth: usize) {
    let indent = "  ".repeat(depth);
    for bone in bones {
        let _ = writeln!(out, "{indent}\"{}\"", display_name(&bone.name));
        write_bones(out, &bone.children, depth + 1);
    }
}

fn display_name(name: &str) -> &str {
    if name.is_empty() { "(unnamed)" } else { name }
}

fn paths_label(count: usize) -> String {
    format!("{count} path{}", if count == 1 { "" } else { "s" })
}

fn children_label(count: usize) -> String {
    format!("{count} {}", if count == 1 { "child" } else { "children" })
}

fn display_point(point: Point) -> String {
    format!("({}, {})", number(point[0]), number(point[1]))
}

/// Formats a number compactly, dropping trailing zeros.
fn number(value: f32) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let mut text = format!("{value:.3}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" { "0".to_owned() } else { text }
}

fn usage() {
    eprintln!("usage:");
    eprintln!("  viveco [convert] [options] <input> [output]");
    eprintln!("  viveco info [options] <input>");
    eprintln!();
    eprintln!("Without a mode, or with `convert`, reads a Viveco asset or SVG document");
    eprintln!("and writes it as a Viveco asset. `info` prints a summary of a file instead");
    eprintln!("without writing anything: the shape and bone hierarchies, each shape's");
    eprintln!("path bounds, the animations, and the artwork's rest bounds. Use it to");
    eprintln!("inspect an asset; converting to JSON only changes its encoding.");
    eprintln!();
    eprintln!("The conversion pipeline is: read (importing an SVG), keep only the named");
    eprintln!("top-level shapes, apply the origin and transform, then write. The input kind");
    eprintln!("and encoding are detected from the file's contents. The output encoding is");
    eprintln!("chosen by its extension (.json writes JSON, anything else writes binary).");
    eprintln!("When output is omitted it is derived from the input path: SVG and JSON");
    eprintln!("inputs become .viveco files.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --group <name>        Import only the objects inside the top-level <g>");
    eprintln!("                        named <name>. A group's name is its inkscape:label");
    eprintln!("                        when it has one (as Inkscape layers do) and its id");
    eprintln!("                        otherwise. Only applies to SVG input. --layer is an");
    eprintln!("                        alias.");
    eprintln!("  --keep <name,...>     Keep only these top-level shapes and their");
    eprintln!("                        subtrees. Repeatable; commas separate names.");
    eprintln!("  --translate <x,y>     Translate the artwork by this offset.");
    eprintln!("  --rotate <degrees>    Rotate clockwise about the origin.");
    eprintln!("  --scale <factor>      Uniformly scale about the origin. Must be positive.");
    eprintln!("  --origin <anchor>     Move the filtered artwork so this anchor of its");
    eprintln!("                        rest bounds becomes the origin (0,0), which is also");
    eprintln!("                        the point --rotate and --scale act about. One of:");
    eprintln!("                        top-left, top, top-right, left, center, right,");
    eprintln!("                        bottom-left, bottom, bottom-right. Applied before");
    eprintln!("                        the transform options.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        Args::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_options_in_any_position() {
        let args = parse(&["--group", "Mid", "in.svg", "out.viveco"])
            .unwrap()
            .unwrap();

        assert_eq!(args.mode, Mode::Convert);
        assert_eq!(args.input, PathBuf::from("in.svg"));
        assert_eq!(args.output, Some(PathBuf::from("out.viveco")));
        assert_eq!(args.group.as_deref(), Some("Mid"));

        let args = parse(&["in.svg", "--group=Mid"]).unwrap().unwrap();
        assert_eq!(args.group.as_deref(), Some("Mid"));

        // `--layer` is kept as an alias of `--group`.
        let args = parse(&["--layer", "Mid", "in.svg"]).unwrap().unwrap();
        assert_eq!(args.group.as_deref(), Some("Mid"));
    }

    #[test]
    fn info_is_a_mode_and_takes_no_output() {
        let args = parse(&["info", "art.viveco"]).unwrap().unwrap();
        assert_eq!(args.mode, Mode::Info);
        assert_eq!(args.input, PathBuf::from("art.viveco"));

        let err = parse(&["info", "art.viveco", "out.viveco"]).unwrap_err();
        assert!(err.contains("no output"), "{err}");

        let err = parse(&["info", "--keep", "Body", "art.viveco"]).unwrap_err();
        assert!(err.contains("--keep"), "{err}");

        let err = parse(&["info", "--scale", "2", "art.viveco"]).unwrap_err();
        assert!(err.contains("transform"), "{err}");
    }

    #[test]
    fn explicit_convert_keeps_the_same_syntax() {
        let args = parse(&["convert", "in.viveco", "out.json"])
            .unwrap()
            .unwrap();
        assert_eq!(args.mode, Mode::Convert);
        assert_eq!(args.input, PathBuf::from("in.viveco"));
        assert_eq!(args.output, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_filter_and_transform_options() {
        let args = parse(&[
            "--keep",
            "Body,Head",
            "--keep=Arms",
            "--translate",
            "10,-5",
            "--rotate",
            "90",
            "--scale",
            "2",
            "in.viveco",
        ])
        .unwrap()
        .unwrap();

        assert_eq!(args.keep, ["Body", "Head", "Arms"]);
        assert_eq!(args.transform.translate, [10.0, -5.0]);
        assert!((args.transform.rotate - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(args.transform.scale, 2.0);

        let args = parse(&["in.viveco", "--translate=3,4"]).unwrap().unwrap();
        assert_eq!(args.transform.translate, [3.0, 4.0]);
    }

    #[test]
    fn parses_origin_and_allows_transforms() {
        let args = parse(&["--origin", "top-left", "in.viveco"])
            .unwrap()
            .unwrap();
        assert_eq!(args.origin, Some(Origin::TopLeft));

        let args = parse(&["--origin=bottom-right", "in.viveco"])
            .unwrap()
            .unwrap();
        assert_eq!(args.origin, Some(Origin::BottomRight));

        for bad in ["nope", "center,top", ""] {
            assert!(
                parse(&["--origin", bad, "in.viveco"]).is_err(),
                "{bad:?} must be rejected"
            );
        }

        let args = parse(&[
            "--origin",
            "top",
            "--rotate",
            "90",
            "--scale",
            "2",
            "in.viveco",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(args.origin, Some(Origin::Top));
        assert!(!args.transform.is_identity());

        let err = parse(&["info", "--origin", "top", "in.viveco"]).unwrap_err();
        assert!(err.contains("conversions"), "{err}");
    }

    #[test]
    fn rejects_invalid_transform_values() {
        for bad in ["0", "-1"] {
            assert!(parse(&["--scale", bad, "in.viveco"]).is_err(), "{bad}");
        }
        for bad in ["nope", "inf", "NaN"] {
            assert!(parse(&["--rotate", bad, "in.viveco"]).is_err(), "{bad}");
        }
        assert!(parse(&["--translate", "1", "in.viveco"]).is_err());
        assert!(parse(&["--keep", "a,,b", "in.viveco"]).is_err());
    }

    fn sample_asset() -> Asset {
        Asset {
            shapes: vec![Shape {
                name: "Body".into(),
                paths: vec![viveco::Path::new([0.0, 0.0]).line_to([10.0, 20.0])],
                children: vec![Shape {
                    name: "Head".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            bones: vec![Bone {
                name: "Root".into(),
                head: Some([0.0, 0.0]),
                tail: [0.0, 10.0],
                children: vec![Bone {
                    name: "Arm".into(),
                    tail: [10.0, 10.0],
                    ..Default::default()
                }],
            }],
            animations: vec![viveco::Animation {
                name: "Wave".into(),
                duration: 2.0,
                cyclic: true,
                ..Default::default()
            }],
        }
    }

    #[test]
    fn info_text_reports_hierarchies_animations_and_bounds() {
        let text = info_text(Path::new("art.viveco"), &sample_asset());

        assert!(text.contains("shapes: 1 top-level, 2 total"), "{text}");
        assert!(text.contains("bones: 2"), "{text}");
        assert!(text.contains("animations: 1"), "{text}");
        assert!(
            text.contains("\"Body\" (1 path, 1 child, bounds (0, 0) to (10, 20))"),
            "{text}"
        );
        assert!(text.contains("  \"Head\" (0 paths, 0 children)"), "{text}");
        assert!(text.contains("  \"Root\""), "{text}");
        assert!(text.contains("    \"Arm\""), "{text}");
        assert!(text.contains("\"Wave\": 2s, cyclic"), "{text}");
        assert!(text.contains("min (0, 0), max (10, 20)"), "{text}");
        assert!(text.contains("size 10 x 20, center (5, 10)"), "{text}");
    }

    #[test]
    fn info_text_reports_empty_assets() {
        let text = info_text(Path::new("empty.viveco"), &Asset::new());
        assert!(text.contains("Shapes:\n  (none)"), "{text}");
        assert!(text.contains("Bones:\n  (none)"), "{text}");
        assert!(text.contains("Animations:\n  (none)"), "{text}");
        assert!(text.contains("none (no artwork)"), "{text}");
    }
}
