//! Corpus loading and management.

use std::path::Path;

/// Load all files from a corpus directory.
pub fn load_corpus(dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut corpus = Vec::new();

    if !dir.exists() {
        anyhow::bail!("corpus directory does not exist: {}", dir.display());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            match std::fs::read(&path) {
                Ok(data) => corpus.push(data),
                Err(e) => {
                    eprintln!("Warning: failed to read {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(corpus)
}

/// Create a seed corpus from existing test files.
pub fn create_seed_corpus(source_dir: &Path, output_dir: &Path) -> anyhow::Result<usize> {
    std::fs::create_dir_all(output_dir)?;

    let mut count = 0;

    fn visit_dir(dir: &Path, output_dir: &Path, count: &mut usize) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                visit_dir(&path, output_dir, count)?;
            } else if path.extension().map_or(false, |ext| ext == "toml") {
                // Extract source from TOML test files
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    extract_sources_from_toml(&contents, output_dir, count)?;
                }
            }
        }
        Ok(())
    }

    visit_dir(source_dir, output_dir, &mut count)?;

    Ok(count)
}

fn extract_sources_from_toml(
    contents: &str,
    output_dir: &Path,
    count: &mut usize,
) -> anyhow::Result<()> {
    // Simple extraction: find `source = """..."""` patterns
    // This is a simplified parser that handles the common case
    let mut in_source = false;
    let mut source_lines = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("source = \"\"\"") {
            in_source = true;
            // Check if it's a single-line source or multi-line
            let prefix = "source = \"\"\"";
            if trimmed.len() > prefix.len() {
                let rest = &trimmed[prefix.len()..];
                if rest.ends_with("\"\"\"") && rest.len() >= 3 {
                    // Single-line source: source = """content"""
                    let source = &rest[..rest.len() - 3];
                    save_source(source.as_bytes(), output_dir, count)?;
                    in_source = false;
                } else {
                    // Multi-line source with content on first line
                    source_lines.clear();
                    source_lines.push(rest.to_string());
                }
            } else {
                // Multi-line source: source = """
                source_lines.clear();
            }
        } else if in_source {
            if trimmed == "\"\"\"" || trimmed.ends_with("\"\"\"") {
                // End of source block
                let source = source_lines.join("\n");
                save_source(source.as_bytes(), output_dir, count)?;
                in_source = false;
                source_lines.clear();
            } else {
                source_lines.push(line.to_string());
            }
        }
    }

    Ok(())
}

fn save_source(source: &[u8], output_dir: &Path, count: &mut usize) -> anyhow::Result<()> {
    use std::io::Write;

    let filename = format!("seed-{:06}.rue", *count);
    let path = output_dir.join(filename);

    let mut file = std::fs::File::create(&path)?;
    file.write_all(source)?;

    *count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_sources_simple() {
        let toml = r#"
[[case]]
name = "test1"
source = """fn main() -> i32 { 42 }"""
exit_code = 42

[[case]]
name = "test2"
source = """
fn main() -> i32 {
    let x = 1;
    x + 2
}
"""
exit_code = 3
"#;
        let temp_dir = std::env::temp_dir().join("rue-fuzz-test");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut count = 0;
        extract_sources_from_toml(toml, &temp_dir, &mut count).unwrap();

        assert_eq!(count, 2);

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
