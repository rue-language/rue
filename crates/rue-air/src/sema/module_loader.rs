use std::collections::{HashMap, HashSet};
use std::path::Path;

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

use crate::types::{ModuleExport, ModuleId, Type};

use super::Sema;

impl<'a> Sema<'a> {
    pub(crate) fn ensure_module_loaded(
        &mut self,
        module_id: ModuleId,
        span: Span,
    ) -> CompileResult<()> {
        if self.module_registry.get_def(module_id).is_loaded {
            return Ok(());
        }

        let file_path = self.module_registry.get_def(module_id).file_path.clone();
        let mut visited = HashSet::new();
        self.load_module_exports(module_id, &file_path, span, &mut visited)
    }

    fn load_module_exports(
        &mut self,
        module_id: ModuleId,
        file_path: &str,
        span: Span,
        visited: &mut HashSet<String>,
    ) -> CompileResult<()> {
        if !visited.insert(file_path.to_string()) {
            return Ok(());
        }

        let content = std::fs::read_to_string(file_path).map_err(|_| {
            CompileError::new(
                ErrorKind::ModuleNotFound {
                    path: file_path.to_string(),
                    candidates: vec![],
                },
                span,
            )
        })?;

        let mut functions = HashMap::new();
        let mut structs = HashMap::new();
        let mut enums = HashMap::new();
        let mut constants = HashMap::new();

        for line in content.lines() {
            let line = strip_line_comment(line.trim());
            if line.is_empty() {
                continue;
            }

            let (is_pub, rest) = match line.strip_prefix("pub ") {
                Some(rest) => (true, rest),
                None => (false, line),
            };

            if let Some(name) = extract_fn_name(rest) {
                functions.insert(name.to_string(), (name.to_string(), is_pub));
            } else if let Some(name) = extract_struct_name(rest) {
                structs.insert(name.to_string(), is_pub);
            } else if let Some(name) = extract_enum_name(rest) {
                enums.insert(name.to_string(), is_pub);
            } else if let Some((name, export)) =
                self.extract_const_import(rest, file_path, span, visited, is_pub)?
            {
                constants.insert(name, export);
            }
        }

        let mut def = self.module_registry.get_def(module_id);
        def.functions = functions;
        def.structs = structs;
        def.enums = enums;
        def.constants = constants;
        def.is_loaded = true;
        self.module_registry.update_def(module_id, def);

        Ok(())
    }

    fn extract_const_import(
        &mut self,
        line: &str,
        file_path: &str,
        span: Span,
        visited: &mut HashSet<String>,
        is_pub: bool,
    ) -> CompileResult<Option<(String, ModuleExport)>> {
        let rest = match line.strip_prefix("const ") {
            Some(r) => r,
            None => return Ok(None),
        };

        let eq_pos = match rest.find('=') {
            Some(pos) => pos,
            None => return Ok(None),
        };

        let name = rest[..eq_pos].trim();
        let init = rest[eq_pos + 1..].trim();

        let import_path = match extract_import_path(init) {
            Some(p) => p,
            None => return Ok(None),
        };

        let resolved = self.resolve_import_relative_to(file_path, import_path, span)?;
        let (nested_id, is_new) = self
            .module_registry
            .get_or_create(import_path.to_string(), resolved.clone());

        if is_new {
            self.load_module_exports(nested_id, &resolved, span, visited)?;
        }

        Ok(Some((
            name.to_string(),
            ModuleExport {
                ty: Type::new_module(nested_id),
                is_pub,
            },
        )))
    }

    fn resolve_import_relative_to(
        &self,
        base_file: &str,
        import_path: &str,
        span: Span,
    ) -> CompileResult<String> {
        let base_dir = Path::new(base_file).parent().unwrap_or(Path::new("."));
        let base_name = import_path.strip_suffix(".rue").unwrap_or(import_path);

        let file_path = base_dir.join(format!("{}.rue", base_name));
        if file_path.exists() {
            return Ok(file_path.to_string_lossy().into_owned());
        }

        let facade_path = base_dir.join(format!("_{}.rue", base_name));
        if facade_path.exists() {
            return Ok(facade_path.to_string_lossy().into_owned());
        }

        Err(CompileError::new(
            ErrorKind::ModuleNotFound {
                path: import_path.to_string(),
                candidates: vec![
                    file_path.to_string_lossy().into_owned(),
                    facade_path.to_string_lossy().into_owned(),
                ],
            },
            span,
        ))
    }
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(pos) => line[..pos].trim_end(),
        None => line,
    }
}

fn extract_fn_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("fn ")?;
    let name = rest.split('(').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn extract_struct_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("struct ")?;
    let name = rest.split(|c| c == '{' || c == '(').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn extract_enum_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("enum ")?;
    let name = rest.split('{').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn extract_import_path(init: &str) -> Option<&str> {
    let rest = init.strip_prefix("@import(\"")?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}
