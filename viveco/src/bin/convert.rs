//! Converts a Viveco asset between the JSON and binary encodings.
//!
//! The input encoding is detected from its contents. The output encoding is
//! chosen from its extension: `.json` writes JSON and anything else writes the
//! binary format. When no output path is given, one is derived from the input.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use viveco::Asset;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);

    let Some(input) = args.next().map(PathBuf::from) else {
        usage();
        return ExitCode::FAILURE;
    };
    let output = args.next().map(PathBuf::from);
    if args.next().is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    let output = output.unwrap_or_else(|| default_output(&input));

    let asset = match Asset::load_from_file(&input) {
        Ok(asset) => asset,
        Err(err) => {
            eprintln!("failed to load {}: {err}", input.display());
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = asset.save_to_file(&output) {
        eprintln!("failed to write {}: {err}", output.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {}", output.display());
    ExitCode::SUCCESS
}

/// The output path used when the caller does not name one.
///
/// A `.json` input becomes a binary file and any other input becomes JSON.
fn default_output(input: &Path) -> PathBuf {
    let json = input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    input.with_extension(if json { "viveco" } else { "json" })
}

fn usage() {
    eprintln!("usage: convert <input> [output]");
    eprintln!();
    eprintln!("Converts a Viveco asset between the JSON and binary encodings.");
    eprintln!("The input encoding is detected from its contents; the output encoding");
    eprintln!("is chosen by its extension (.json writes JSON, anything else writes");
    eprintln!("binary). When output is omitted it is derived from the input path.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_flips_the_encoding() {
        assert_eq!(
            default_output(Path::new("art.json")),
            PathBuf::from("art.viveco")
        );
        assert_eq!(
            default_output(Path::new("art.JSON")),
            PathBuf::from("art.viveco")
        );
        assert_eq!(
            default_output(Path::new("art.viveco")),
            PathBuf::from("art.json")
        );
        assert_eq!(default_output(Path::new("art")), PathBuf::from("art.json"));
    }
}
