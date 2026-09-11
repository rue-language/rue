//! The opt-in, source-independent module manifest format.
//!
//! A manifest is a build-system input, not a source observation.  It names the
//! root, the declared source spelling for each logical module, and the exact
//! decoded import spelling used to reach each target.  Bytes, canonical paths,
//! provenance, and trust are deliberately absent: those facts are captured by
//! the current host request and are never accepted from serialized data.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ModuleId;

pub const EXPLICIT_MODULE_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitModuleManifest {
    pub version: u32,
    pub root: String,
    pub modules: Vec<ManifestModule>,
    pub imports: Vec<ManifestImport>,
    /// Logical standard-library requirements are intentionally separate from
    /// project module entries. They are satisfied by the independently
    /// supplied trusted std root and never become semantic roots.
    #[serde(default)]
    pub std_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestModule {
    pub module: String,
    /// A relative source spelling under the captured project root. The host
    /// validates the resulting bytes and provenance for every request.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestImport {
    pub importer: String,
    /// Exact decoded value of the source `@import` string literal.
    pub literal: String,
    /// `None` records a deliberately missing import and is distinct from an
    /// omitted key, which is a stale/incomplete manifest error.
    pub target: Option<String>,
}

/// A typed module reference used only in the import binding part of the wire
/// format.  Module declarations retain their logical identity verbatim, so a
/// project module whose identity starts with `std:` remains legal.  Binding
/// references use an escaped type tag and therefore cannot confuse that
/// project identity with a captured trusted standard-library module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestModuleReference {
    Project(String),
    Standard(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl ExplicitModuleManifest {
    pub fn new(
        root: impl Into<String>,
        modules: Vec<ManifestModule>,
        imports: Vec<ManifestImport>,
    ) -> Self {
        Self {
            version: EXPLICIT_MODULE_MANIFEST_VERSION,
            root: root.into(),
            modules,
            imports,
            std_requirements: Vec::new(),
        }
    }

    /// Parse and validate the bounded serialized representation.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError(
                "module manifest exceeds the 16 MiB limit".into(),
            ));
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError(format!("invalid module manifest: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        self.validate()?;
        serde_json::to_vec_pretty(self)
            .map_err(|error| ManifestError(format!("could not encode module manifest: {error}")))
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version != EXPLICIT_MODULE_MANIFEST_VERSION {
            return Err(ManifestError(format!(
                "unsupported module manifest version {} (expected {})",
                self.version, EXPLICIT_MODULE_MANIFEST_VERSION
            )));
        }
        let root = valid_module_id(&self.root, "root")?;
        let mut modules = std::collections::BTreeSet::new();
        for entry in &self.modules {
            let module = valid_module_id(&entry.module, "module")?;
            if !modules.insert(module.as_str().to_owned()) {
                return Err(ManifestError(format!(
                    "duplicate module identity {:?}",
                    entry.module
                )));
            }
            validate_path(&entry.path, "module source path")?;
        }
        if !modules.contains(root.as_str()) {
            return Err(ManifestError(format!(
                "manifest root {:?} is not declared",
                self.root
            )));
        }
        let mut imports = std::collections::BTreeSet::new();
        for entry in &self.imports {
            decode_module_reference(&entry.importer)
                .map_err(|error| ManifestError(format!("invalid importer: {error}")))?;
            if entry.literal.contains('\0') {
                return Err(ManifestError(
                    "import literal contains a reserved NUL byte".into(),
                ));
            }
            if !imports.insert((entry.importer.clone(), entry.literal.clone())) {
                return Err(ManifestError(format!(
                    "duplicate import binding ({:?}, {:?})",
                    entry.importer, entry.literal
                )));
            }
            if let Some(target) = &entry.target {
                decode_module_reference(target)
                    .map_err(|error| ManifestError(format!("invalid import target: {error}")))?;
            }
        }
        let mut requirements = std::collections::BTreeSet::new();
        for requirement in &self.std_requirements {
            validate_path(requirement, "standard-library requirement")?;
            if !requirements.insert(requirement) {
                return Err(ManifestError(format!(
                    "duplicate standard-library requirement {:?}",
                    requirement
                )));
            }
        }
        Ok(())
    }

    pub fn module_id(value: &str) -> Result<ModuleId, ManifestError> {
        valid_module_id(value, "module")
    }
}

fn valid_module_id(value: &str, role: &str) -> Result<ModuleId, ManifestError> {
    if value.contains('\0') {
        return Err(ManifestError(format!(
            "{role} identity contains a reserved NUL byte"
        )));
    }
    let id = ModuleId::from_logical_path(value)
        .map_err(|error| ManifestError(format!("invalid {role} identity {value:?}: {error}")))?;
    if id.as_str() != value {
        return Err(ManifestError(format!(
            "{role} identity {:?} is not normalized; use {:?}",
            value,
            id.as_str()
        )));
    }
    Ok(id)
}

/// Encode a logical module reference with an unambiguous type tag.  Every
/// non-unreserved byte is escaped, including `%`, so the result is reversible
/// for every legal project identity.
pub fn encode_module_reference(module: &ModuleId) -> String {
    let (prefix, value) = if module.is_trusted_standard_library() {
        (
            "std:",
            module
                .as_str()
                .strip_prefix(crate::TRUSTED_STANDARD_LIBRARY_NAMESPACE)
                .unwrap_or(module.as_str()),
        )
    } else {
        ("project:", module.as_str())
    };
    format!("{prefix}{}", escape_reference_bytes(value.as_bytes()))
}

pub fn decode_module_reference(value: &str) -> Result<ManifestModuleReference, ManifestError> {
    let (kind, encoded) = value.split_once(':').ok_or_else(|| {
        ManifestError("module reference must use project: or std: encoding".into())
    })?;
    let decoded = unescape_reference_bytes(encoded)?;
    if decoded.is_empty() || decoded.contains('\0') {
        return Err(ManifestError(
            "module reference has an empty or reserved identity".into(),
        ));
    }
    match kind {
        "project" => {
            let id = valid_module_id(&decoded, "project module reference")?;
            Ok(ManifestModuleReference::Project(id.as_str().to_owned()))
        }
        "std" => {
            validate_path(&decoded, "standard-library module reference")?;
            Ok(ManifestModuleReference::Standard(decoded))
        }
        _ => Err(ManifestError(format!(
            "module reference uses unknown type tag {kind:?}"
        ))),
    }
}

fn escape_reference_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            output.push(*byte as char);
        } else {
            output.push('%');
            output.push(
                char::from_digit((byte >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            output.push(
                char::from_digit((byte & 0x0f) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    output
}

fn unescape_reference_bytes(value: &str) -> Result<String, ManifestError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(ManifestError(
                "module reference has an incomplete escape".into(),
            ));
        }
        let high = hex_digit(bytes[index + 1])?;
        let low = hex_digit(bytes[index + 2])?;
        decoded.push(high << 4 | low);
        index += 3;
    }
    String::from_utf8(decoded)
        .map_err(|_| ManifestError("module reference is not valid UTF-8".into()))
}

fn hex_digit(byte: u8) -> Result<u8, ManifestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ManifestError(
            "module reference has an invalid escape".into(),
        )),
    }
}

fn validate_path(value: &str, role: &str) -> Result<(), ManifestError> {
    let path = std::path::Path::new(value);
    if value.is_empty() || value.contains('\0') || path.is_absolute() {
        return Err(ManifestError(format!(
            "{role} must be a non-empty relative path"
        )));
    }
    if path
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(ManifestError(format!("{role} may not contain '..'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_deterministic_and_sparse() {
        let mut manifest = ExplicitModuleManifest::new(
            "root",
            vec![
                ManifestModule {
                    module: "root".into(),
                    path: "main.rue".into(),
                },
                ManifestModule {
                    module: "lib.rue".into(),
                    path: "lib.rue".into(),
                },
            ],
            vec![ManifestImport {
                importer: "project:root".into(),
                literal: "lib.rue".into(),
                target: Some("project:lib.rue".into()),
            }],
        );
        manifest.std_requirements.push("option.rue".into());
        let bytes = manifest.to_json_bytes().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("source_bytes"));
        assert_eq!(ExplicitModuleManifest::parse(&bytes).unwrap(), manifest);
    }

    #[test]
    fn rejects_duplicate_and_unsafe_entries() {
        let mut manifest = ExplicitModuleManifest::new(
            "root",
            vec![ManifestModule {
                module: "root".into(),
                path: "../main.rue".into(),
            }],
            vec![],
        );
        assert!(manifest.validate().is_err());
        manifest.modules.push(ManifestModule {
            module: "root".into(),
            path: "other.rue".into(),
        });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn typed_references_round_trip_reserved_project_identity() {
        let project = ModuleId::from_logical_path("std:ordinary.rue").unwrap();
        let wire = encode_module_reference(&project);
        assert_eq!(wire, "project:std%3Aordinary.rue");
        assert_eq!(
            decode_module_reference(&wire).unwrap(),
            ManifestModuleReference::Project("std:ordinary.rue".into())
        );
    }

    #[test]
    fn duplicate_binding_keys_are_rejected_even_when_targets_match() {
        let manifest = ExplicitModuleManifest::new(
            "root",
            vec![ManifestModule {
                module: "root".into(),
                path: "main.rue".into(),
            }],
            vec![
                ManifestImport {
                    importer: "project:root".into(),
                    literal: "x".into(),
                    target: None,
                },
                ManifestImport {
                    importer: "project:root".into(),
                    literal: "x".into(),
                    target: None,
                },
            ],
        );
        assert!(manifest.validate().is_err());
    }
}
