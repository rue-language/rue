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
            error!("Test run failed: {}", e);
            std::process::exit(1);
        }
    }
}
