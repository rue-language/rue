struct Args {
    thing: String,
    number: u32,
    shout: bool,
}

fn parse_args() -> Result<Args, &'static str> {
    #![allow(clippy::enum_glob_use)]
    use lexarg_parser::Arg::*;

    let mut thing = None;
    let mut number = 1;
    let mut shout = false;
    let raw = std::env::args_os().collect::<Vec<_>>();
    let mut parser = lexarg_parser::Parser::new(&raw);
    let _bin_name = parser.next_raw();
    while let Some(arg) = parser.next_arg() {
        match arg {
            Short("n") | Long("number") => {
                number = parser
                    .next_flag_value()
                    .ok_or("`--number` requires a value")?
                    .to_str()
                    .ok_or("invalid number")?
                    .parse()
                    .map_err(|_e| "invalid number")?;
            }
            Long("shout") => {
                shout = true;
            }
            Value(val) if thing.is_none() => {
                thing = Some(val.to_str().ok_or("invalid string")?);
            }
            Short("h") | Long("help") => {
                println!("Usage: hello [-n|--number=NUM] [--shout] THING");
                std::process::exit(0);
            }
            _ => {
                return Err("unexpected argument");
            }
        }
    }

    Ok(Args {
        thing: thing.ok_or("missing argument THING")?.to_owned(),
        number,
        shout,
    })
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    let mut message = format!("Hello {}", args.thing);
    if args.shout {
        message = message.to_uppercase();
    }
    for _ in 0..args.number {
        println!("{message}");
    }
    Ok(())
}
