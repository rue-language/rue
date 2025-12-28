use libtest2_mimic::RunError;
use libtest2_mimic::RunResult;
use libtest2_mimic::Trial;

fn main() -> std::io::Result<()> {
    let tests = collect_tests()?;
    libtest2_mimic::Harness::with_env().discover(tests).main()
}

/// Creates one test for each `.rs` file in the current directory or
/// sub-directories of the current directory.
fn collect_tests() -> std::io::Result<Vec<Trial>> {
    fn visit_dir(path: &std::path::Path, tests: &mut Vec<Trial>) -> std::io::Result<()> {
        let current_dir = std::env::current_dir()?;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;

            // Handle files
            let path = entry.path();
            if file_type.is_file() {
                if path.extension() == Some(std::ffi::OsStr::new("rs")) {
                    let name = match path.strip_prefix(&current_dir) {
                        Ok(path) => path,
                        Err(_) => {
                            continue;
                        }
                    }
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned();

                    let test = Trial::test(name, move |_| check_file(&path));
                    tests.push(test);
                }
            } else if file_type.is_dir() {
                // Handle directories
                visit_dir(&path, tests)?;
            }
        }

        Ok(())
    }

    // We recursively look for `.rs` files, starting from the current
    // directory.
    let mut tests = Vec::new();
    let current_dir = std::env::current_dir()?;
    visit_dir(&current_dir, &mut tests)?;

    Ok(tests)
}

/// Performs a couple of tidy tests.
fn check_file(path: &std::path::Path) -> RunResult {
    let content =
        std::fs::read(path).map_err(|e| RunError::fail(format_args!("Cannot read file: {e}")))?;

    // Check that the file is valid UTF-8
    let content = String::from_utf8(content)
        .map_err(|_| RunError::fail("The file's contents are not a valid UTF-8 string!"))?;

    // Check for `\r`: we only want `\n` line breaks!
    if content.contains('\r') {
        return Err(RunError::fail(
            "Contains '\\r' chars. Please use ' \\n' line breaks only!",
        ));
    }

    // Check for tab characters `\t`
    if content.contains('\t') {
        return Err(RunError::fail(
            "Contains tab characters ('\\t'). Indent with four spaces!",
        ));
    }

    // Check for too long lines
    if content.lines().any(|line| line.chars().count() > 100) {
        return Err(RunError::fail("Contains lines longer than 100 codepoints!"));
    }

    Ok(())
}
