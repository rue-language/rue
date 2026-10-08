//! Language server for Rue over stdio.
//!
//! The server is a thin editor adapter over the compiler's canonical
//! artifacts (ADR-0061): live buffers are parsed by a per-buffer
//! `CompilerSession`, and saved files are checked by retained
//! `FilesystemCompilerHost`s — the same host the CLI, watch mode, and the
//! compiler service use. It adds no parser, resolver, or diagnostic renderer
//! of its own. See `docs/process/lsp.md`.

mod check;
mod document;
mod features;
mod server;
mod syntax_index;
mod text;
mod transport;

use std::io::{self, BufReader};

use server::Server;
use transport::{Incoming, read_message, write_message};

fn main() {
    if let Some(argument) = std::env::args().nth(1) {
        match argument.as_str() {
            "--version" => {
                println!("rue-lsp {}", rue_error::VERSION);
                return;
            }
            // Editors commonly pass `--stdio`; stdio is the only transport.
            "--stdio" => {}
            other => {
                eprintln!("rue-lsp: unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }
    std::process::exit(run());
}

fn run() -> i32 {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = io::stdout().lock();
    let mut server = Server::new();
    loop {
        let outgoing = match read_message(&mut input) {
            Ok(Incoming::Message(message)) => server.handle(message),
            Ok(Incoming::Malformed(error)) => vec![Server::parse_error(&error)],
            // A client that disappears without `exit` ends the session as
            // an unclean shutdown.
            Ok(Incoming::Eof) => return 1,
            Err(error) => {
                eprintln!("rue-lsp: {error}");
                return 1;
            }
        };
        for message in &outgoing {
            if let Err(error) = write_message(&mut output, message) {
                eprintln!("rue-lsp: {error}");
                return 1;
            }
        }
        if let Some(code) = server.exit_code() {
            return code;
        }
    }
}
