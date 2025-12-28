fn main() -> lexarg::Result<()> {
    use lexarg::prelude::*;

    let mut test_opts = libtest_lexarg::TestOptsBuilder::new();

    let raw = std::env::args_os().collect::<Vec<_>>();
    let mut parser = lexarg::Parser::new(&raw);
    let bin = parser
        .next_raw()
        .expect("first arg, no pending values")
        .unwrap_or(std::ffi::OsStr::new("test"));
    let mut prev_arg = Value(bin);
    while let Some(arg) = parser.next_arg() {
        match arg {
            Short("h") | Long("help") => {
                let bin = bin.to_string_lossy();
                let options_help = libtest_lexarg::OPTIONS_HELP.trim();
                let after_help = libtest_lexarg::AFTER_HELP.trim();
                println!(
                    "Usage: {bin} [OPTIONS] [FILTER]...

{options_help}

{after_help}"
                );
                std::process::exit(0);
            }
            // All values are the same, whether escaped or not, so its a no-op
            Escape(_) => {
                prev_arg = arg;
                continue;
            }
            Unexpected(_) => {
                return Err(lexarg::LexError::msg("unexpected value")
                    .unexpected(arg)
                    .within(prev_arg)
                    .into());
            }
            _ => {}
        }
        prev_arg = arg;

        let arg = test_opts.parse_next(&mut parser, arg)?;

        if let Some(arg) = arg {
            return Err(lexarg::LexError::msg("unexpected argument")
                .unexpected(arg)
                .into());
        }
    }

    let opts = test_opts.finish()?;
    println!("{opts:#?}");

    Ok(())
}
