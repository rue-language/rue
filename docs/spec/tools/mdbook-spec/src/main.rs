//! mdbook-spec: A preprocessor for the Rue Language Specification.
//!
//! This preprocessor handles rule definitions using the `r[rule.id]` syntax,
//! converting them into styled, linkable HTML blocks.
//!
//! This tool is inspired by and derived from the mdbook-spec tool used by the
//! Rust Reference: <https://github.com/rust-lang/reference/tree/master/tools/mdbook-spec>
//!
//! Licensed under Apache-2.0 OR MIT, same as the original.

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;

/// Regex to match rule definitions: `r[rule.id]` or `r[rule.id#category]` at the start of a line.
/// The category is optional and can be: normative, informative, syntax, example
static RULE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^r\[([^#\]]+)(?:#([a-z]+))?\]$").unwrap());

/// Regex to match rule references in link definitions: `[text]: rule.id`
static RULE_LINK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\[([^\]]+)\]:\s+([a-z][a-z0-9._-]+)$").unwrap());

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("supports") => {
            // Check if we support the given renderer
            let renderer = args.next().expect("renderer name");
            if renderer == "html" {
                process::exit(0);
            } else {
                process::exit(1);
            }
        }
        Some(arg) => {
            eprintln!("unknown argument: {arg}");
            process::exit(1);
        }
        None => {
            // Run as preprocessor
            if let Err(e) = run_preprocessor() {
                eprintln!("Error: {e:?}");
                process::exit(1);
            }
        }
    }
}

fn run_preprocessor() -> Result<()> {
    // Read JSON input from stdin
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    // Parse the input: [context, book]
    let input: serde_json::Value = serde_json::from_str(&input)?;
    let book_value = input
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Expected [context, book] array"))?;

    let mut book: Book = serde_json::from_value(book_value.clone())?;

    // Process the book
    let spec = Spec::new(&book);
    spec.process_book(&mut book);

    // Output the modified book
    serde_json::to_writer(io::stdout(), &book)?;
    io::stdout().flush()?;

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Book {
    items: Vec<BookItem>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum BookItem {
    Chapter(Chapter),
    Separator,
    PartTitle(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Chapter {
    name: String,
    content: String,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    sub_items: Vec<BookItem>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

struct Spec {
    /// Map from rule ID to the chapter path where it's defined.
    rules: BTreeMap<String, PathBuf>,
}

impl Spec {
    fn new(book: &Book) -> Self {
        let mut spec = Spec {
            rules: BTreeMap::new(),
        };
        spec.collect_rules_from_items(&book.items);
        spec
    }

    fn collect_rules_from_items(&mut self, items: &[BookItem]) {
        for item in items {
            if let BookItem::Chapter(chapter) = item {
                if let Some(path) = &chapter.path {
                    for cap in RULE_RE.captures_iter(&chapter.content) {
                        let rule_id = cap[1].to_string();
                        self.rules.insert(rule_id, path.clone());
                    }
                }
                self.collect_rules_from_items(&chapter.sub_items);
            }
        }
    }

    fn process_book(&self, book: &mut Book) {
        self.process_items(&mut book.items);
    }

    fn process_items(&self, items: &mut [BookItem]) {
        for item in items {
            if let BookItem::Chapter(chapter) = item {
                self.process_chapter(chapter);
                self.process_items(&mut chapter.sub_items);
            }
        }
    }

    fn process_chapter(&self, chapter: &mut Chapter) {
        let path = chapter.path.clone().unwrap_or_default();

        // Render rule definitions
        chapter.content = self.render_rules(&chapter.content);

        // Convert rule link references
        chapter.content = self.convert_rule_links(&chapter.content, &path);
    }

    /// Render rule definitions as HTML blocks.
    ///
    /// The HTML is designed to work with a CSS grid layout where rule IDs
    /// appear in the left margin, similar to the Rust Reference.
    fn render_rules(&self, content: &str) -> String {
        RULE_RE
            .replace_all(content, |caps: &Captures| {
                let rule_id = &caps[1];
                let anchor = format!("r-{rule_id}");
                let hash = "#";

                // Simple structure: div with anchor and link
                // CSS grid positions this in the left margin
                format!(
                    r#"<div class="rule" id="{anchor}"><a class="rule-link" href="{hash}{anchor}">[{rule_id}]</a></div>
"#
                )
            })
            .into_owned()
    }

    /// Convert rule link references to actual links.
    fn convert_rule_links(&self, content: &str, current_path: &PathBuf) -> String {
        RULE_LINK_RE
            .replace_all(content, |caps: &Captures| {
                let link_text = &caps[1];
                let rule_id = &caps[2];

                if let Some(target_path) = self.rules.get(rule_id) {
                    // Calculate relative path from current chapter to target
                    let rel_path = Self::relative_path(current_path, target_path);
                    let anchor = format!("r-{rule_id}");
                    let hash = "#";
                    format!("[{link_text}]: {rel_path}{hash}{anchor}")
                } else {
                    // Keep as-is if not a known rule
                    caps[0].to_string()
                }
            })
            .into_owned()
    }

    /// Calculate relative path from one chapter to another.
    ///
    /// Given paths like "03-types/01-integer-types.md" and "04-expressions/02-arithmetic.md",
    /// computes the relative path needed to link from the first to the second.
    fn relative_path(from: &PathBuf, to: &PathBuf) -> String {
        // Convert to .html extension
        let to_html = to.with_extension("html");

        // Count directory depth of 'from' to know how many "../" we need
        let from_depth = from.parent().map(|p| p.components().count()).unwrap_or(0);

        // Build the relative path: "../" for each level up, then the target path
        let mut rel = String::new();
        for _ in 0..from_depth {
            rel.push_str("../");
        }
        rel.push_str(&to_html.to_string_lossy());

        rel
    }
}
