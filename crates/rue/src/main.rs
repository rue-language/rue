use bpaf::{Parser, construct};
use rue_compiler::{
    CompileOptions, RueDatabase, SourceFile, compile_file_to_assembly_with_options,
    compile_file_with_options,
};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
struct Args {
    output: Option<PathBuf>,
    input: PathBuf,
    emit_asm: bool,
    optimize: bool,
}

fn parser() -> impl Parser<Args> {
    let output = bpaf::short('o')
        .long("output")
        .help("Output binary filename")
        .argument::<PathBuf>("OUTPUT")
        .optional();

    let emit_asm = bpaf::long("emit-asm")
        .help("Emit assembly instead of executable")
        .switch();

    let optimize = bpaf::short('O').help("Enable MIR optimizations").switch();

    let input = bpaf::positional::<PathBuf>("INPUT").help("Input Rue source file");

    construct!(Args {
        output,
        emit_asm,
        optimize,
        input,
    })
}

fn main() {
    let opts = parser()
        .to_options()
        .descr("The Rue programming language compiler")
        .run();

    let input_path = opts.input;
    let output_path = if opts.emit_asm {
        opts.output
            .unwrap_or_else(|| input_path.with_extension("s"))
    } else {
        opts.output.unwrap_or_else(|| input_path.with_extension(""))
    };

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
    let options = CompileOptions::new(&db, opts.optimize);

    // Compile
    if opts.emit_asm {
        // Generate assembly
        let result = compile_file_to_assembly_with_options(&db, file, options);
        match result {
            Ok(assembly) => match fs::write(&output_path, &*assembly) {
                Ok(()) => {
                    println!(
                        "Successfully generated assembly to '{}'",
                        output_path.display()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Error writing output file '{}': {}",
                        output_path.display(),
                        e
                    );
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("Compilation failed: {}", error.message);
                std::process::exit(1);
            }
        }
    } else {
        // Generate executable
        let result = compile_file_with_options(&db, file, options);
        match result {
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
}
