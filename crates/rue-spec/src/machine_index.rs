//! Versioned machine-readable compiler-error and specification index.
//!
//! This is a thin projection of the compiler's `ErrorCode` declarations, the
//! specification's rule markers, and spec-case diagnostic assertions. It does
//! not infer semantic relationships from prose.

use crate::traceability::{SpecParagraph, parse_spec_paragraphs};
use rue_error::error_code_metadata;
use rue_test_runner::{load_test_files, runs_on_required_ci};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const MACHINE_INDEX_SCHEMA_VERSION: u32 = 1;

const WEBSITE_CONFIG: &str = include_str!("website-config.toml");
const SPEC_RULE_TEMPLATE: &str = include_str!("spec-rule-template.html");
const SPEC_ROUTE_ROOT: &str = include_str!("spec-route-root.txt");

#[derive(Debug)]
struct WebsiteAuthorities {
    base_url: String,
    anchor_prefix: String,
    spec_route_root: String,
}

#[derive(Debug, Serialize)]
struct MachineIndex {
    schema_version: u32,
    errors: Vec<ErrorEntry>,
    spec_rules: Vec<SpecRuleEntry>,
    error_spec_relationships: Vec<ErrorSpecRelationship>,
}

#[derive(Debug, Serialize)]
struct ErrorEntry {
    code: String,
    name: &'static str,
    title: String,
    source_path: &'static str,
}

#[derive(Debug, Serialize)]
struct SpecRuleEntry {
    id: String,
    title: String,
    category: String,
    source_path: String,
    anchor: String,
    canonical_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct RelationshipEvidence {
    case: String,
    source_path: String,
}

#[derive(Debug, Serialize)]
struct ErrorSpecRelationship {
    error_code: String,
    spec_id: String,
    evidence: Vec<RelationshipEvidence>,
}

fn is_normative(paragraph: &SpecParagraph) -> bool {
    matches!(
        paragraph.category.as_str(),
        "normative" | "legality-rule" | "dynamic-semantics" | "syntax" | "undefined-behavior"
    )
}

fn website_authorities() -> Result<WebsiteAuthorities, String> {
    let config: toml::Value = toml::from_str(WEBSITE_CONFIG)
        .map_err(|error| format!("invalid website/config.toml: {error}"))?;
    let base_url = config
        .get("base_url")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "website/config.toml has no string base_url".to_string())?
        .trim_end_matches('/')
        .to_string();
    let path_slugification = config
        .get("slugify")
        .and_then(|value| value.get("paths"))
        .and_then(toml::Value::as_str);
    if path_slugification != Some("on") {
        return Err("website/config.toml must declare slugify.paths = \"on\"".to_string());
    }

    let marker = "{{ id }}";
    let id_start = SPEC_RULE_TEMPLATE
        .find("id=\"")
        .ok_or_else(|| "spec rule template has no id attribute".to_string())?
        + "id=\"".len();
    let id_tail = &SPEC_RULE_TEMPLATE[id_start..];
    let marker_start = id_tail
        .find(marker)
        .ok_or_else(|| "spec rule template id does not contain {{ id }}".to_string())?;
    let anchor_prefix = id_tail[..marker_start].to_string();
    let expected_href = format!("href=\"#{anchor_prefix}{marker}\"");
    if !SPEC_RULE_TEMPLATE.contains(&expected_href) {
        return Err(format!(
            "spec rule template href does not match id prefix {anchor_prefix:?}"
        ));
    }
    let spec_route_root = SPEC_ROUTE_ROOT.trim();
    if !valid_route(spec_route_root) || spec_route_root.is_empty() {
        return Err(
            "website/spec-route-root.txt must contain a non-empty lower-case ASCII route"
                .to_string(),
        );
    }
    Ok(WebsiteAuthorities {
        base_url,
        anchor_prefix,
        spec_route_root: spec_route_root.to_string(),
    })
}

fn valid_route(route: &str) -> bool {
    route.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'/'
    }) && !route.starts_with('/')
        && !route.ends_with('/')
        && !route.contains("//")
}

/// Project a spec-source path through the exact Zola subset independently
/// modeled by `scripts/gazette-corpus-diff.py::route_of`: directory names are
/// copied verbatim, while only a page file stem is ASCII-case-folded. Anything
/// beyond lower-case ASCII route components fails closed rather than guessing
/// at Zola's slug crate behavior.
fn content_route(source_path: &str) -> Result<String, String> {
    let stem = source_path
        .strip_suffix(".md")
        .ok_or_else(|| format!("specification source is not Markdown: {source_path}"))?;
    let route = if stem == "_index" {
        String::new()
    } else if let Some(section) = stem.strip_suffix("/_index") {
        section.to_string()
    } else {
        let (head, name) = stem.rsplit_once('/').unwrap_or(("", stem));
        if head.is_empty() {
            name.to_ascii_lowercase()
        } else {
            format!("{head}/{}", name.to_ascii_lowercase())
        }
    };
    if !route.is_empty() && !valid_route(&route) {
        return Err(format!(
            "{source_path} projects to /{route}, outside the proven Zola route subset: directory components must be lower-case ASCII letters, digits, or '-', and only a page file stem may fold ASCII case"
        ));
    }
    Ok(route)
}

fn canonical_url(
    authorities: &WebsiteAuthorities,
    source_path: &str,
    id: &str,
) -> Result<String, String> {
    let page = content_route(source_path)?;
    let route = if page.is_empty() {
        format!("{}/", authorities.spec_route_root)
    } else {
        format!("{}/{page}/", authorities.spec_route_root)
    };
    Ok(format!(
        "{}/{route}#{}{id}",
        authorities.base_url, authorities.anchor_prefix
    ))
}

/// Extract explicitly asserted compiler codes, not incidental mentions in
/// descriptions or source. A token is exactly `E` plus four decimal digits.
fn asserted_error_codes(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut codes = BTreeSet::new();
    for start in 0..bytes.len().saturating_sub(4) {
        if bytes[start] != b'E' || !bytes[start + 1..start + 5].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let preceded_by_word = start > 0 && bytes[start - 1].is_ascii_alphanumeric();
        let followed_by_word = bytes.get(start + 5).is_some_and(u8::is_ascii_alphanumeric);
        if !preceded_by_word && !followed_by_word {
            codes.insert(text[start..start + 5].to_string());
        }
    }
    codes
}

fn diagnostic_header_error_codes(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let code = line
                .strip_prefix("error: [")
                .or_else(|| line.strip_prefix("error["))?
                .get(..5)?;
            (asserted_error_codes(code).len() == 1).then(|| code.to_string())
        })
        .collect()
}

pub fn generate(spec_dir: &Path, cases_dir: &Path) -> Result<Vec<u8>, String> {
    let website = website_authorities()?;
    let error_metadata = error_code_metadata();
    let known_codes: BTreeSet<String> = error_metadata
        .iter()
        .map(|entry| entry.code.to_string())
        .collect();
    if known_codes.len() != error_metadata.len() {
        return Err("compiler error metadata contains duplicate codes".to_string());
    }

    let errors = error_metadata
        .iter()
        .map(|entry| ErrorEntry {
            code: entry.code.to_string(),
            name: entry.name,
            title: entry.title.clone(),
            source_path: entry.source_path,
        })
        .collect();

    let (paragraphs, duplicates) = parse_spec_paragraphs(spec_dir)?;
    if !duplicates.is_empty() {
        return Err(format!(
            "duplicate specification rule IDs: {}",
            duplicates.join(", ")
        ));
    }
    let spec_rules = paragraphs
        .values()
        .filter(|paragraph| is_normative(paragraph))
        .map(|paragraph| {
            if paragraph.title.is_empty() {
                return Err(format!(
                    "normative specification rule {} has no enclosing heading",
                    paragraph.id
                ));
            }
            let source_path = format!("docs/spec/src/{}", paragraph.source_path);
            Ok(SpecRuleEntry {
                id: paragraph.id.clone(),
                title: paragraph.title.clone(),
                category: paragraph.category.clone(),
                source_path,
                anchor: format!("{}{}", website.anchor_prefix, paragraph.id),
                canonical_url: canonical_url(&website, &paragraph.source_path, &paragraph.id)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut relationships: BTreeMap<(String, String), BTreeSet<RelationshipEvidence>> =
        BTreeMap::new();
    for (case_id, test_file) in load_test_files(cases_dir)? {
        // `load_test_files` returns the normalized corpus-relative file ID
        // without `.toml`; use that authority rather than a sandbox path.
        let source_path = format!("crates/rue-spec/cases/{case_id}.toml");
        for case in test_file.case {
            // Match traceability's evidence boundary: skipped,
            // allowed-to-fail preview, and CI-unreachable cases do not prove a
            // language rule and therefore cannot prove an error relationship.
            if case.skip
                || (case.preview.is_some() && !case.preview_should_pass)
                || !runs_on_required_ci(&case.only_on)
            {
                continue;
            }
            let mut codes = BTreeSet::new();
            for assertion in case.error_contains.iter() {
                codes.extend(asserted_error_codes(assertion));
            }
            if let Some(assertion) = &case.expected_error {
                codes.extend(diagnostic_header_error_codes(assertion));
            }
            if let Some(code) = &case.expected_error_code {
                // `load_test_files` validates typed declarations against the
                // compiler-owned inventory after parameter expansion. Union
                // them with schema-v1's existing exact structural evidence;
                // never infer a new relationship from prose.
                codes.insert(code.clone());
            }
            for code in codes {
                if !known_codes.contains(&code) {
                    return Err(format!(
                        "{}::{} asserts unknown compiler error code {code}",
                        test_file.section.id, case.name
                    ));
                }
                for spec_id in &case.spec {
                    let Some(paragraph) = paragraphs.get(spec_id) else {
                        return Err(format!(
                            "{}::{} cites unknown specification rule {spec_id}",
                            test_file.section.id, case.name
                        ));
                    };
                    if !is_normative(paragraph) {
                        continue;
                    }
                    relationships
                        .entry((code.clone(), spec_id.clone()))
                        .or_default()
                        .insert(RelationshipEvidence {
                            case: format!("{}::{}", test_file.section.id, case.name),
                            source_path: source_path.clone(),
                        });
                }
            }
        }
    }
    let error_spec_relationships = relationships
        .into_iter()
        .map(|((error_code, spec_id), evidence)| ErrorSpecRelationship {
            error_code,
            spec_id,
            evidence: evidence.into_iter().collect(),
        })
        .collect();

    let index = MachineIndex {
        schema_version: MACHINE_INDEX_SCHEMA_VERSION,
        errors,
        spec_rules,
        error_spec_relationships,
    };
    let mut bytes = serde_json::to_vec_pretty(&index).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn asserted_codes_require_exact_tokens() {
        assert_eq!(
            asserted_error_codes("error[E0206], then E0400"),
            BTreeSet::from(["E0206".to_string(), "E0400".to_string()])
        );
        assert!(asserted_error_codes("RUE0206 E02061 XE0206").is_empty());
    }

    #[test]
    fn golden_stderr_uses_only_diagnostic_headers() {
        let golden = r#"error: [E0201]: undefined variable; see E0400 for another diagnostic
 --> main.rue:1:20
  |
1 | fn main() { let marker = "E0400"; missing; }
  |                          ^^^^^ incidental source excerpt
"#;
        assert_eq!(
            diagnostic_header_error_codes(golden),
            BTreeSet::from(["E0201".to_string()])
        );
    }

    #[test]
    fn canonical_urls_follow_the_spec_site_path_and_anchor_contract() {
        let authorities = website_authorities().unwrap();
        assert_eq!(authorities.base_url, "https://rue-lang.dev");
        assert_eq!(authorities.anchor_prefix, "r-");
        assert_eq!(authorities.spec_route_root, "spec");
        assert_eq!(
            canonical_url(&authorities, "appendices/B-undefined-behavior.md", "B.2:2").unwrap(),
            "https://rue-lang.dev/spec/appendices/b-undefined-behavior/#r-B.2:2",
        );
        assert_eq!(
            canonical_url(&authorities, "03-types/_index.md", "3.1:1").unwrap(),
            "https://rue-lang.dev/spec/03-types/#r-3.1:1",
        );
        assert_eq!(
            canonical_url(&authorities, "_index.md", "1.1:1").unwrap(),
            "https://rue-lang.dev/spec/#r-1.1:1",
        );
    }

    #[test]
    fn routes_fail_closed_outside_the_proven_zola_subset() {
        assert_eq!(
            content_route("appendices/A-grammar.md").unwrap(),
            "appendices/a-grammar",
        );
        for path in [
            "Upper/page.md",
            "under_score/page.md",
            "dots.v2/page.md",
            "unicode-é/page.md",
            "plain/Under_Score.md",
            "plain/Dots.v2.md",
            "plain/nonascii-é.md",
        ] {
            assert!(content_route(path).is_err(), "unexpectedly routed {path}");
        }
    }

    #[test]
    fn index_bytes_are_reproducible_and_typed_evidence_is_additive() {
        let spec_dir = tempfile::tempdir().unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            spec_dir.path().join("01-test.md"),
            "# Test Rules\n\n{{ rule(id=\"1.1:1\", cat=\"legality-rule\") }}\nRule.\n",
        )
        .unwrap();
        fs::write(
            cases_dir.path().join("test.toml"),
            r#"[section]
id = "test.section"
name = "Test"

[[case]]
name = "typed_failure"
source = "fn main() { missing }"
compile_fail = true
expected_error_code = "E0201"
spec = ["1.1:1"]

[[case]]
name = "schema_v1_structural_evidence"
source = "fn main() { missing }"
compile_fail = true
error_contains = "E0206"
spec = ["1.1:1"]
"#,
        )
        .unwrap();

        let first = generate(spec_dir.path(), cases_dir.path()).unwrap();
        let second = generate(spec_dir.path(), cases_dir.path()).unwrap();
        assert_eq!(first, second);
        let text = String::from_utf8(first).unwrap();
        assert!(text.contains("\"schema_version\": 1"));
        assert!(text.contains("\"title\": \"Test Rules\""));
        assert!(text.contains("\"error_code\": \"E0201\""));
        assert!(text.contains("https://rue-lang.dev/spec/01-test/#r-1.1:1"));
        let index: serde_json::Value = serde_json::from_str(&text).unwrap();
        let relationships = index["error_spec_relationships"].as_array().unwrap();
        assert_eq!(relationships.len(), 2);
        assert_eq!(relationships[0]["error_code"], "E0201");
        assert_eq!(relationships[0]["evidence"].as_array().unwrap().len(), 1);
        assert_eq!(relationships[1]["error_code"], "E0206");
        assert_eq!(relationships[1]["evidence"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn index_rejects_unknown_typed_diagnostic_evidence() {
        let spec_dir = tempfile::tempdir().unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            spec_dir.path().join("01-test.md"),
            "# Test Rules\n\n{{ rule(id=\"1.1:1\", cat=\"legality-rule\") }}\nRule.\n",
        )
        .unwrap();
        fs::write(
            cases_dir.path().join("test.toml"),
            r#"[section]
id = "test.section"
name = "Test"

[[case]]
name = "unknown_code"
source = "fn main() { missing }"
compile_fail = true
expected_error_code = "E9999"
spec = ["1.1:1"]
"#,
        )
        .unwrap();
        let error = generate(spec_dir.path(), cases_dir.path()).unwrap_err();
        assert!(error.contains("unknown `expected_error_code` \"E9999\""));
    }
}
