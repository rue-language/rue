//! Deterministic website pages projected from the compiler's error inventory.

use crate::machine_index::canonical_spec_path;
use crate::traceability::parse_spec_paragraphs;
use rue_error::{error_code_explanation, error_code_metadata};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const OUTPUT_DIR: &str = "website/content/errors";
const STAGING_DIR: &str = "website/content/.error-pages-staging";
const BACKUP_DIR: &str = "website/content/.error-pages-backup";

#[derive(Debug, PartialEq, Eq)]
pub struct GeneratedPage {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

fn front_matter_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                return format!("{value:?}");
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

pub fn generate(spec_dir: &Path) -> Result<Vec<GeneratedPage>, String> {
    let metadata = error_code_metadata();
    let (spec_paragraphs, duplicate_spec_ids) = parse_spec_paragraphs(spec_dir)?;
    if !duplicate_spec_ids.is_empty() {
        return Err(format!(
            "duplicate specification rule IDs: {}",
            duplicate_spec_ids.join(", ")
        ));
    }
    let mut codes = BTreeSet::new();
    for entry in metadata {
        if !codes.insert(entry.code.0) {
            return Err(format!(
                "compiler error metadata contains duplicate code {}",
                entry.code
            ));
        }
    }

    let mut index = String::from(
        "+++\ntitle = \"Compiler errors\"\ndescription = \"Stable Rue compiler diagnostic codes\"\ntemplate = \"errors/section.html\"\nsort_by = \"none\"\n+++\n\nEvery active Rue compiler diagnostic code has a permanent number. A retired number is never reassigned.\n\n| Code | Title | Category | Stability |\n| --- | --- | --- | --- |\n",
    );
    let mut pages = Vec::with_capacity(metadata.len() + 1);
    for entry in metadata {
        let code = entry.code.to_string();
        index.push_str(&format!(
            "| [{code}](/errors/{code}/) | {} | {} | {} |\n",
            entry.title,
            entry.category.title(),
            entry.stability.title()
        ));

        let page_title = format!("{code}: {}", entry.title);
        let mut page = format!(
            "+++\ntitle = {}\ndescription = {}\npath = {}\n+++\n\n| Code | Category | Stability |\n| --- | --- | --- |\n| `{code}` | {} | {} |\n\n",
            front_matter_string(&page_title),
            front_matter_string(&format!("Rue compiler error {code}: {}", entry.title)),
            front_matter_string(&format!("errors/{code}")),
            entry.category.title(),
            entry.stability.title(),
        );

        if let Some(explanation) = error_code_explanation(entry.code) {
            page.push_str("## Explanation\n\n");
            page.push_str(explanation.explanation);
            page.push_str("\n\n## Likely cause\n\n");
            page.push_str(explanation.likely_cause);
            if !explanation.examples.is_empty() {
                page.push_str("\n\n## Examples\n");
                for example in explanation.examples {
                    if example.source.contains("```") {
                        return Err(format!(
                            "example {:?} for {code} contains an unsupported Markdown fence",
                            example.title
                        ));
                    }
                    page.push_str(&format!(
                        "\n### {}\n\n```rue\n{}\n```\n",
                        example.title, example.source
                    ));
                }
            }
            if !explanation.references.is_empty() {
                page.push_str("\n## References\n");
                for reference in explanation.references {
                    let rule = reference.rule.ok_or_else(|| {
                        format!(
                            "error-code reference {:?} for {code} has no specification rule",
                            reference.title
                        )
                    })?;
                    let relative =
                        reference
                            .path
                            .strip_prefix("docs/spec/src/")
                            .ok_or_else(|| {
                                format!(
                                    "unsupported error-code reference path {:?} for {code}",
                                    reference.path
                                )
                            })?;
                    if !spec_dir.join(relative).is_file() {
                        return Err(format!(
                            "error-code reference {:?} for {code} does not exist under {}",
                            reference.path,
                            spec_dir.display()
                        ));
                    }
                    let paragraph = spec_paragraphs.get(rule).ok_or_else(|| {
                        format!(
                            "error-code reference {:?} for {code} names unknown specification rule {rule}",
                            reference.title
                        )
                    })?;
                    if paragraph.source_path != relative {
                        return Err(format!(
                            "error-code reference {:?} for {code} places specification rule {rule} in {:?}, but its canonical source is {:?}",
                            reference.title, relative, paragraph.source_path
                        ));
                    }
                    let href = canonical_spec_path(reference.path, rule)?;
                    page.push_str(&format!("\n- [{}]({href})\n", reference.title));
                }
            }
        } else {
            page.push_str(
                "No extended explanation exists for this diagnostic yet. The code and its stability remain part of the public compiler interface.\n",
            );
        }

        pages.push(GeneratedPage {
            path: PathBuf::from(&code).join("index.md"),
            bytes: page.into_bytes(),
        });
    }
    pages.insert(
        0,
        GeneratedPage {
            path: PathBuf::from("_index.md"),
            bytes: index.into_bytes(),
        },
    );
    Ok(pages)
}

fn require_directory_or_absent(path: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing to replace non-directory generated path {}",
            path.display()
        ));
    }
    Ok(true)
}

fn remove_generated_directory(path: &Path) -> Result<(), String> {
    if require_directory_or_absent(path)? {
        fs::remove_dir_all(path)
            .map_err(|error| format!("failed to clean {}: {error}", path.display()))?;
    }
    Ok(())
}

fn write_to_paths(
    output: &Path,
    staging: &Path,
    backup: &Path,
    spec_dir: &Path,
) -> Result<(), String> {
    let pages = generate(spec_dir)?;
    remove_generated_directory(staging)?;
    fs::create_dir_all(staging)
        .map_err(|error| format!("failed to create {}: {error}", staging.display()))?;
    for page in pages {
        let path = staging.join(page.path);
        fs::create_dir_all(path.parent().expect("generated page always has a parent"))
            .map_err(|error| format!("failed to create parent for {}: {error}", path.display()))?;
        fs::write(&path, page.bytes)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    }

    remove_generated_directory(backup)?;
    let had_output = require_directory_or_absent(output)?;
    if had_output {
        fs::rename(output, backup).map_err(|error| {
            format!(
                "failed to stage replacement of {} as {}: {error}",
                output.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging, output) {
        if had_output {
            let _ = fs::rename(backup, output);
        }
        return Err(format!(
            "failed to install generated pages at {}: {error}",
            output.display()
        ));
    }
    remove_generated_directory(backup)?;
    Ok(())
}

pub fn write(spec_dir: &Path) -> Result<(), String> {
    write_to_paths(
        Path::new(OUTPUT_DIR),
        Path::new(STAGING_DIR),
        Path::new(BACKUP_DIR),
        spec_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_error::RETIRED_ERROR_CODES;
    use std::collections::BTreeMap;

    fn spec_fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let mut rules_by_path = BTreeMap::<&str, BTreeSet<&str>>::new();
        for metadata in error_code_metadata() {
            let Some(explanation) = error_code_explanation(metadata.code) else {
                continue;
            };
            for reference in explanation.references {
                rules_by_path
                    .entry(reference.path)
                    .or_default()
                    .insert(reference.rule.expect("website references require a rule"));
            }
        }
        for (path, rules) in rules_by_path {
            let relative = path
                .strip_prefix("docs/spec/src/")
                .expect("website references must name specification sources");
            let fixture_path = directory.path().join(relative);
            fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
            let mut source = String::from("# Fixture\n");
            for rule in rules {
                source.push_str(&format!(
                    "\n{{{{ rule(id=\"{rule}\", cat=\"legality-rule\") }}}}\nRule.\n"
                ));
            }
            fs::write(fixture_path, source).unwrap();
        }
        directory
    }

    #[test]
    fn pages_are_complete_unique_active_only_and_reproducible() {
        let fixture = spec_fixture();
        let first = generate(fixture.path()).unwrap();
        let second = generate(fixture.path()).unwrap();
        assert_eq!(first, second, "generation must be byte-reproducible");
        assert_eq!(first.len(), error_code_metadata().len() + 1);

        let by_path: BTreeMap<_, _> = first
            .iter()
            .map(|page| {
                (
                    page.path.clone(),
                    String::from_utf8(page.bytes.clone()).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            by_path.len(),
            first.len(),
            "generated routes must be unique"
        );
        let index = &by_path[Path::new("_index.md")];
        for entry in error_code_metadata() {
            let code = entry.code.to_string();
            assert!(index.contains(&format!("(/errors/{code}/)")));
            assert!(by_path.contains_key(&PathBuf::from(code).join("index.md")));
        }
        for retired in RETIRED_ERROR_CODES {
            let code = retired.to_string();
            assert!(
                !index.contains(&code),
                "retired code {code} has an index row"
            );
            assert!(
                !by_path.contains_key(&PathBuf::from(&code).join("index.md")),
                "retired code {code} has a generated route"
            );
        }
    }

    #[test]
    fn generated_internal_links_use_existing_routes() {
        let fixture = spec_fixture();
        let pages = generate(fixture.path()).unwrap();
        let expected_spec_links = error_code_metadata()
            .iter()
            .filter_map(|metadata| error_code_explanation(metadata.code))
            .flat_map(|explanation| explanation.references)
            .map(|reference| {
                canonical_spec_path(
                    reference.path,
                    reference.rule.expect("website references require a rule"),
                )
                .unwrap()
            })
            .collect::<BTreeSet<_>>();
        for page in pages {
            let text = String::from_utf8(page.bytes).unwrap();
            for href in text
                .split("](")
                .skip(1)
                .filter_map(|tail| tail.split(')').next())
            {
                if let Some(code) = href
                    .strip_prefix("/errors/")
                    .and_then(|path| path.strip_suffix('/'))
                {
                    assert!(
                        error_code_metadata()
                            .iter()
                            .any(|entry| entry.code.to_string() == code),
                        "index links only to active generated error routes"
                    );
                } else if href.starts_with("/spec/") {
                    assert!(
                        expected_spec_links.contains(href),
                        "explanation links only to declared specification references"
                    );
                } else {
                    panic!("unsupported generated internal link: {href}");
                }
            }
        }
    }

    #[test]
    fn generator_has_no_second_error_registry() {
        let generator = include_str!("error_pages.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let template = include_str!("error-page-template.html");
        let build_script = include_str!("website-build.sh");
        assert!(generator.contains("error_code_metadata()"));
        assert!(generator.contains("error_code_explanation(entry.code)"));
        assert!(template.contains("section.content | safe"));
        assert!(!template.contains("/errors/"));
        for (name, source) in [
            ("generator", generator),
            ("error-page template", template),
            ("website build", build_script),
        ] {
            assert!(!source.contains("ErrorCode::"), "{name} copies typed codes");
            assert!(
                !source.as_bytes().windows(5).any(|window| {
                    window[0] == b'E' && window[1..].iter().all(u8::is_ascii_digit)
                }),
                "{name} hand-maintains an error-code literal"
            );
        }
    }

    #[test]
    fn staged_write_replaces_only_after_every_page_is_ready() {
        let fixture = spec_fixture();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("errors");
        let staging = root.path().join("staging");
        let backup = root.path().join("backup");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("stale.md"), "stale").unwrap();

        write_to_paths(&output, &staging, &backup, fixture.path()).unwrap();

        assert!(!output.join("stale.md").exists());
        assert!(output.join("_index.md").is_file());
        assert!(!staging.exists());
        assert!(!backup.exists());
    }
}
