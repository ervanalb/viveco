//! Converts Viveco assets and imports SVG documents.
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

use std::path::PathBuf;
use std::process::ExitCode;

use viveco::convert::{self, ConvertOptions};

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

    match convert(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// The command line of the converter.
struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    /// Import only the top-level group with this name.
    group: Option<String>,
}

impl Args {
    /// Parses the command line. Returns `Ok(None)` when help was printed.
    fn parse(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<Option<Self>, String> {
        let mut input = None;
        let mut output = None;
        let mut group = None;

        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("-h" | "--help") => {
                    usage();
                    return Ok(None);
                }
                Some("--group" | "--layer") => {
                    let Some(name) = args.next() else {
                        return Err("--group requires a group name".to_owned());
                    };
                    group = Some(name.to_string_lossy().into_owned());
                }
                Some(text) => {
                    if let Some(name) = text
                        .strip_prefix("--group=")
                        .or_else(|| text.strip_prefix("--layer="))
                    {
                        if name.is_empty() {
                            return Err("--group requires a group name".to_owned());
                        }
                        group = Some(name.to_owned());
                    } else if text.starts_with('-') && text != "-" {
                        return Err(format!("unknown option `{text}`"));
                    } else if input.is_none() {
                        input = Some(PathBuf::from(arg));
                    } else if output.is_none() {
                        output = Some(PathBuf::from(arg));
                    } else {
                        return Err("too many positional arguments".to_owned());
                    }
                }
                // A non-UTF-8 path is still a valid positional argument.
                None => {
                    if input.is_none() {
                        input = Some(PathBuf::from(arg));
                    } else if output.is_none() {
                        output = Some(PathBuf::from(arg));
                    } else {
                        return Err("too many positional arguments".to_owned());
                    }
                }
            }
        }

        let Some(input) = input else {
            return Err("missing input path".to_owned());
        };
        Ok(Some(Self {
            input,
            output,
            group,
        }))
    }
}

fn convert(args: &Args) -> Result<(), String> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| convert::default_output(&args.input));
    let options = ConvertOptions {
        group: args.group.clone(),
    };

    convert::convert_file(&args.input, &output, &options).map_err(|err| err.to_string())?;

    println!("wrote {}", output.display());
    Ok(())
}

fn usage() {
    eprintln!("usage: convert [--group <name>] <input> [output]");
    eprintln!();
    eprintln!("Converts a Viveco asset between the JSON and binary encodings, or");
    eprintln!("imports an SVG document as a Viveco asset.");
    eprintln!();
    eprintln!("The input kind and encoding are detected from the file's contents.");
    eprintln!("The output encoding is chosen by its extension (.json writes JSON,");
    eprintln!("anything else writes binary). When output is omitted it is derived");
    eprintln!("from the input path: SVG and JSON inputs become .viveco files.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --group <name>  Import only the objects inside the top-level <g> named");
    eprintln!("                  <name>. A group's name is its inkscape:label when it");
    eprintln!("                  has one (as Inkscape layers do) and its id otherwise.");
    eprintln!("                  Only applies to SVG input. --layer is an alias.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options_in_any_position() {
        let args = Args::parse(
            ["--group", "Mid", "in.svg", "out.viveco"]
                .into_iter()
                .map(Into::into),
        )
        .unwrap()
        .unwrap();

        assert_eq!(args.input, PathBuf::from("in.svg"));
        assert_eq!(args.output, Some(PathBuf::from("out.viveco")));
        assert_eq!(args.group.as_deref(), Some("Mid"));

        let args = Args::parse(["in.svg", "--group=Mid"].into_iter().map(Into::into))
            .unwrap()
            .unwrap();
        assert_eq!(args.group.as_deref(), Some("Mid"));

        // `--layer` is kept as an alias of `--group`.
        let args = Args::parse(["--layer", "Mid", "in.svg"].into_iter().map(Into::into))
            .unwrap()
            .unwrap();
        assert_eq!(args.group.as_deref(), Some("Mid"));
    }
}
