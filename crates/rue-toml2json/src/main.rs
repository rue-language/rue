//! Re-emit a TOML document as JSON on stdout.
//!
//! Repository tooling runs on the Python 3.9 floor, where `tomllib` does not
//! exist. Rather than raise the floor for one file or hand-write a partial
//! TOML parser, the build materializes a JSON twin of a TOML policy file
//! with this converter, and the Python side reads that (RUE-1524).

use std::process::ExitCode;

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = match (args.next(), args.next()) {
        (Some(path), None) => path,
        _ => return Err("usage: rue-toml2json <file.toml>".to_string()),
    };
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("{}: {error}", path.to_string_lossy()))?;
    let value: toml::Value =
        toml::from_str(&source).map_err(|error| format!("{}: {error}", path.to_string_lossy()))?;
    let json = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("{}: {error}", path.to_string_lossy()))?;
    println!("{json}");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}
