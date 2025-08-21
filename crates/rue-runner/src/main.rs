use anyhow::Result;
use clap::Parser;
use tracing::{error, info};

use rue_runner::{Args, run};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    info!("Starting Rue test runner");

    match run(args) {
        Ok(_) => {
            info!("Test run completed successfully");
            Ok(())
        }
        Err(e) => {
            // Log the full error chain
            error!("Test run failed: {}", e);
            let mut source = e.source();
            while let Some(err) = source {
                error!("  Caused by: {}", err);
                source = err.source();
            }
            std::process::exit(1);
        }
    }
}
