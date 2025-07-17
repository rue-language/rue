use bpaf::{Parser, construct};
use rue_compiler::{RueDatabase, SourceFile, compile_file};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
struct Args {
    output: Option<PathBuf>,
    input: PathBuf,
}

fn parser() -> impl Parser<Args> {
    let output = bpaf::short('o')
        .long("output")
        .help("Output binary filename")
        .argument::<PathBuf>("OUTPUT")
        .optional();

    let input = bpaf::positional::<PathBuf>("INPUT").help("Input Rue source file");

    construct!(Args { output, input })
}

fn main() {
    let opts = parser()
        .to_options()
        .descr("The Rue programming language compiler")
        .run();

    let input_path = opts.input;
    let output_path = opts.output.unwrap_or_else(|| input_path.with_extension(""));

    // Read source file
    let source = match fs::read_to_string(&input_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("Error reading file '{}': {}", input_path.display(), e);
            std::process::exit(1);
        }
    };

    // Set up Salsa database
    let db = RueDatabase::default();
    let file = SourceFile::new(&db, input_path.to_string_lossy().to_string(), source);

    // Compile
    match compile_file(&db, file) {
        Ok(executable) => {
            match fs::write(&output_path, &*executable) {
                Ok(()) => {
                    // Make executable on Unix systems
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = fs::metadata(&output_path).unwrap().permissions();
                        perms.set_mode(0o755);
                        fs::set_permissions(&output_path, perms).unwrap();
                    }

                    println!("Successfully compiled to '{}'", output_path.display());
                }
                Err(e) => {
                    eprintln!(
                        "Error writing output file '{}': {}",
                        output_path.display(),
                        e
                    );
                    std::process::exit(1);
                }
            }
        }
        Err(error) => {
            eprintln!("Compilation failed: {}", error.message);
            std::process::exit(1);
        }
    }
}
