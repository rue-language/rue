//! Diagnostic formatting for compiler errors and warnings.
//!
//! This module provides utilities for formatting compiler diagnostics into
//! human-readable output using annotate-snippets for source annotations.
//!
//! # Example
//!
//! ```ignore
//! use rue_compiler::unstable::{DiagnosticFormatter, SourceInfo};
//!
//! let source_info = SourceInfo::new(&source, "example.rue");
//! let formatter = DiagnosticFormatter::new(&source_info);
//!
//! // Format an error
//! let error_output = formatter.format_error(&error);
//! eprintln!("{}", error_output);
//!
//! // Format warnings
//! let warning_output = formatter.format_warnings(&warnings);
//! eprintln!("{}", warning_output);
//! ```

use std::borrow::Cow;
use std::io::IsTerminal;

use ahash::AHashMap;
use annotate_snippets::{Level, Renderer, Snippet};
use rue_span::offset_to_line_col;

use crate::{CompileError, CompileErrors, CompileWarning, Diagnostic, ErrorCode, FileId, Span};

/// Source text prepared for human snippet rendering.
///
/// Compiler spans are byte offsets into the original source. For terminal
/// diagnostics, however, two byte-preserving source forms render badly:
///
/// - `\t`: annotate-snippets expands the source tab to spaces but computes
///   annotation columns as if the tab had zero width, shifting carets left.
/// - `\r\n`: the literal `\r` is printed in the source line, moving terminal
///   cursors and corrupting the visual diagnostic.
///
/// Normalize only the snippet text and remap original byte offsets into that
/// normalized text. This leaves the parser, semantic spans, and JSON diagnostics
/// on original-source coordinates while making human output stable.
/// A safe, single-column visible glyph for a control character, so terminal
/// diagnostics render it as a picture instead of executing it (RUE-548).
///
/// C0 controls map to their Unicode Control Pictures (U+2400 `␀` … U+241F
/// `␟`), DEL to U+2421 `␡`, and C1 controls (U+0080…U+009F) to U+FFFD `�`.
/// Every result has display width 1, so replacing a control char one-for-one
/// preserves caret/column geometry. Tab, newline, and carriage return are
/// layout-significant and handled by the caller before this is reached.
fn control_glyph(ch: char) -> char {
    match ch as u32 {
        0x7f => '\u{2421}',                                   // ␡ DEL
        n if n < 0x20 => char::from_u32(0x2400 + n).unwrap(), // ␀ … ␟
        _ => '\u{fffd}',                                      // C1 → �
    }
}

/// Is this a control character the human renderer must neutralize? Tab and
/// newline are layout-significant and allowed to pass; everything else
/// `char::is_control` flags (C0 minus tab/newline, DEL, C1) is a
/// terminal-injection hazard (RUE-548).
fn is_unsafe_control_char(c: char) -> bool {
    c.is_control() && c != '\t' && c != '\n'
}

/// Whether any character in `s` needs neutralizing (see [`is_unsafe_control_char`]).
fn has_unsafe_control(s: &str) -> bool {
    s.chars().any(is_unsafe_control_char)
}

/// Neutralize control characters in a human-facing MESSAGE string (a title,
/// label, note, or help), which may interpolate user-controlled data such as
/// module import specifiers or file paths (RUE-548). Source-excerpt text is
/// handled by [`RenderSource`] instead, which also remaps span offsets. Tab
/// and newline are preserved for layout; carriage return is dropped, matching
/// the source renderer. Returns the input unchanged when it is already safe.
///
/// This runs only on the TEXT render path; the JSON diagnostic formatter is a
/// separate type that relies on serde's own control-character escaping, so the
/// underlying error data is never pre-escaped here.
fn sanitize_message(s: &str) -> Cow<'_, str> {
    if !has_unsafe_control(s) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\t' | '\n' => out.push(ch),
            '\r' => {}
            c if c.is_control() => out.push(control_glyph(c)),
            _ => out.push(ch),
        }
    }
    Cow::Owned(out)
}

struct RenderSource<'a> {
    source: Cow<'a, str>,
    byte_map: Option<Vec<usize>>,
}

impl<'a> RenderSource<'a> {
    fn new(source: &'a str) -> Self {
        // annotate-snippets does not materialize the empty line after a final
        // newline. Keep a zero-width marker there so a point span at EOF still
        // has a concrete line on which to draw its caret.
        let needs_eof_marker = source.ends_with(['\n', '\r']);

        // Fast path: no tab (caret geometry) or control char — carriage return
        // and every other terminal-injection hazard (RUE-548) are covered by
        // `is_unsafe_control_char`.
        if !source
            .chars()
            .any(|c| c == '\t' || is_unsafe_control_char(c))
            && !needs_eof_marker
        {
            return Self {
                source: Cow::Borrowed(source),
                byte_map: None,
            };
        }

        let mut rendered = String::with_capacity(source.len());
        let mut byte_map = vec![0; source.len() + 1];

        for (idx, ch) in source.char_indices() {
            let mapped = rendered.len();
            let next_idx = idx + ch.len_utf8();
            for slot in &mut byte_map[idx..next_idx] {
                *slot = mapped;
            }

            match ch {
                '\t' => rendered.push_str("    "),
                '\r' => {
                    // In a CRLF the CR is dropped and the following LF renders
                    // the break (as before). A BARE CR is itself a newline
                    // (spec 2.3:1), so render it as a line break instead of
                    // dropping it — dropping collapsed CR-separated lines into
                    // one, mislocating the diagnostic (RUE-534).
                    if !source[next_idx..].starts_with('\n') {
                        rendered.push('\n');
                    }
                }
                '\n' => rendered.push('\n'),
                // Any other control char is a terminal-injection hazard: render
                // a visible one-column glyph instead (RUE-548). The byte_map
                // records where this original byte lands, so the multi-byte
                // glyph keeps annotation spans aligned to the right column.
                c if c.is_control() => rendered.push(control_glyph(c)),
                _ => rendered.push(ch),
            }
            byte_map[next_idx] = rendered.len();
        }

        if needs_eof_marker {
            rendered.push('\u{200b}');
            byte_map[source.len()] = rendered.len() - '\u{200b}'.len_utf8();
        }

        Self {
            source: Cow::Owned(rendered),
            byte_map: Some(byte_map),
        }
    }

    fn len(&self) -> usize {
        self.source.len()
    }

    fn map_offset(&self, offset: usize) -> usize {
        match &self.byte_map {
            Some(byte_map) => byte_map[offset.min(byte_map.len() - 1)],
            None => offset.min(self.source.len()),
        }
    }

    fn map_span(&self, span: Span, original_source_len: usize) -> (usize, usize) {
        let start = (span.start as usize).min(original_source_len);
        let end = (span.end as usize).min(original_source_len).max(start);
        let mapped_start = self.map_offset(start).min(self.len());
        let mapped_end = self.map_offset(end).min(self.len()).max(mapped_start);
        (mapped_start, mapped_end)
    }
}

/// Return the slice of `source` that becomes rendered line `line_no` (1-based).
///
/// Splits on LF, CR, and CRLF — each one line terminator (spec 2.3:1 / RUE-534)
/// — mirroring how [`RenderSource`] builds the buffer whose lines annotate-
/// snippets counts, so a rendered origin line maps back to the right original
/// line even in CR/CRLF files. Returns `None` if `line_no` is out of range.
fn nth_rendered_line(source: &str, line_no: usize) -> Option<&str> {
    let bytes = source.as_bytes();
    let mut start = 0;
    let mut current = 1;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' | b'\r' => {
                if current == line_no {
                    return Some(&source[start..i]);
                }
                // CRLF is a single terminator: consume the trailing LF too.
                if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                i += 1;
                start = i;
                current += 1;
            }
            _ => i += 1,
        }
    }
    (current == line_no).then(|| &source[start..])
}

/// Map a tab-expanded display column back to the original-source character
/// column on rendered line `line_no` (both 1-based).
///
/// [`RenderSource`] expands each `\t` to four spaces so the human caret aligns
/// (RUE-362); as a side effect annotate-snippets derives the `--> path:line:col`
/// origin coordinate from that widened buffer, pushing the reported column past
/// the token (RUE-556). Only tabs change width (control chars map one glyph to
/// one column), so walking the original line and charging four columns per tab
/// recovers the coordinate JSON diagnostics and editors expect.
fn original_col_for_expanded(source: &str, line_no: usize, expanded_col: usize) -> usize {
    let Some(line) = nth_rendered_line(source, line_no) else {
        return expanded_col;
    };
    let mut rendered_col = 1usize;
    let mut orig_col = 1usize;
    for ch in line.chars() {
        if rendered_col >= expanded_col {
            break;
        }
        rendered_col += if ch == '\t' { 4 } else { 1 };
        orig_col += 1;
    }
    // If the caret sat exactly on a char start, `orig_col` names it. If it fell
    // past the line's end (e.g. an EOF span), `orig_col` is the char after the
    // last, which is the correct one-past-end column.
    orig_col
}

/// Rewrite each `--> path:line:col` origin coordinate in `rendered` so its
/// column is the ORIGINAL-source column rather than the tab-expanded one
/// annotate-snippets derives (RUE-556). The caret/excerpt geometry — which
/// legitimately uses the expanded buffer — is untouched.
///
/// `files` supplies the original source for each rendered path. Because only a
/// tab widens the column, this is a no-op (and cheap early-out) for tab-free
/// input. The path/line/col text is plain even under color, so the rewrite is
/// a byte-exact `replacen` that preserves any surrounding ANSI styling.
fn correct_origin_columns(rendered: String, files: &[(&str, &str)]) -> String {
    if !files.iter().any(|(_, src)| src.contains('\t')) {
        return rendered;
    }
    let mut out = String::with_capacity(rendered.len());
    for line in rendered.split_inclusive('\n') {
        out.push_str(&fix_origin_line(line, files).unwrap_or_else(|| line.to_string()));
    }
    out
}

/// Correct a single rendered line if it is a `--> path:line:col` origin whose
/// column was inflated by tab expansion; otherwise return `None`.
fn fix_origin_line(line: &str, files: &[(&str, &str)]) -> Option<String> {
    let marker = line.find("-->")?;
    if !is_origin_gutter(&line[..marker]) {
        return None;
    }
    let origin_start = skip_origin_prefix(line, marker + "-->".len());

    for (path, source) in files {
        let Some(rendered_path) = line.get(origin_start..origin_start + path.len()) else {
            continue;
        };
        if rendered_path != *path {
            continue;
        }
        let Some(after_path) = line
            .get(origin_start + path.len()..)
            .filter(|suffix| suffix.starts_with(':'))
        else {
            continue;
        };

        // After "<path>:" the origin line is exactly "<line>:<col>" plus any
        // line terminators. A stray path mention elsewhere fails to parse here
        // (trailing text makes the column non-numeric), so it is left alone.
        let after = after_path[1..].trim_end_matches(['\n', '\r']);
        let Some((line_str, col_str)) = after.split_once(':') else {
            continue;
        };
        let (Ok(line_no), Ok(col)) = (line_str.parse::<usize>(), col_str.parse::<usize>()) else {
            continue;
        };
        let new_col = original_col_for_expanded(source, line_no, col);
        if new_col == col {
            continue;
        }

        let col_start = origin_start + path.len() + 1 + line_str.len() + 1;
        let col_end = col_start + col_str.len();
        let mut corrected = line.to_string();
        corrected.replace_range(col_start..col_end, &new_col.to_string());
        return Some(corrected);
    }
    None
}

/// Return the first byte after renderer gutter whitespace and ANSI styling.
fn skip_origin_prefix(line: &str, mut offset: usize) -> usize {
    let bytes = line.as_bytes();
    while offset < bytes.len() {
        if bytes[offset].is_ascii_whitespace() {
            offset += 1;
            continue;
        }
        if bytes[offset] != b'\x1b' || bytes.get(offset + 1) != Some(&b'[') {
            break;
        }
        offset += 2;
        while offset < bytes.len() && !(0x40..=0x7e).contains(&bytes[offset]) {
            offset += 1;
        }
        if offset < bytes.len() {
            offset += 1;
        }
    }
    offset
}

/// Whether the bytes before an origin marker are only renderer gutter text.
fn is_origin_gutter(prefix: &str) -> bool {
    skip_origin_prefix(prefix, 0) == prefix.len()
}

/// Source code information for diagnostic rendering.
///
/// Contains the source text and file path needed for rendering annotated
/// error and warning messages.
#[derive(Debug, Clone)]
pub struct SourceInfo<'a> {
    /// The source code text.
    pub source: &'a str,
    /// The path to the source file (for display in diagnostics).
    pub path: &'a str,
}

impl<'a> SourceInfo<'a> {
    /// Create a new SourceInfo with the given source and file path.
    pub fn new(source: &'a str, path: &'a str) -> Self {
        Self { source, path }
    }
}

/// Color choice for diagnostic output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Automatically detect whether to use colors based on terminal capabilities.
    #[default]
    Auto,
    /// Always use colors.
    Always,
    /// Never use colors.
    Never,
}

/// Formatter for compiler diagnostics.
///
/// Provides methods to format compilation errors and warnings into
/// human-readable strings with annotated source snippets.
pub struct DiagnosticFormatter<'a> {
    source_info: &'a SourceInfo<'a>,
    renderer: Renderer,
}

impl<'a> DiagnosticFormatter<'a> {
    /// Create a new diagnostic formatter for the given source info.
    ///
    /// By default, uses automatic color detection based on whether stderr is a terminal.
    pub fn new(source_info: &'a SourceInfo<'a>) -> Self {
        Self::with_color_choice(source_info, ColorChoice::Auto)
    }

    /// Create a new diagnostic formatter with explicit color choice.
    pub fn with_color_choice(source_info: &'a SourceInfo<'a>, color_choice: ColorChoice) -> Self {
        let use_color = match color_choice {
            ColorChoice::Auto => std::io::stderr().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
        let renderer = if use_color {
            Renderer::styled()
        } else {
            Renderer::plain()
        };
        Self {
            source_info,
            renderer,
        }
    }

    /// Format a compilation error into a string.
    ///
    /// The error is formatted with its error code, e.g.:
    /// `error[E0206]: type mismatch: expected i32, found bool`
    ///
    /// Internal compiler errors (ICEs) receive special formatting with
    /// bug report instructions.
    pub fn format_error(&self, error: &CompileError) -> String {
        // Format with error code: error[E0XXX]: message
        let message_with_code = format!("[{}]: {}", error.kind.code(), error.kind);

        // Check if this is an internal compiler error
        let is_ice = matches!(
            error.kind.code(),
            ErrorCode::INTERNAL_ERROR | ErrorCode::INTERNAL_CODEGEN_ERROR
        );

        let mut output = self.format_diagnostic_impl(
            Level::Error,
            &message_with_code,
            error.span(),
            error.diagnostic(),
        );

        // Add ICE-specific notes and helps
        if is_ice {
            output.push_str("\nnote: this is a bug in the Rue compiler\n");
            output.push_str(
                "help: please report this issue at https://github.com/rue-language/rue/issues\n",
            );
        }

        output
    }

    /// Format multiple compilation errors into a string.
    ///
    /// Each error is formatted on its own line(s). A summary line is added at
    /// the end if there are multiple errors showing the total count.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        if errors.is_empty() {
            return String::new();
        }

        let mut output = String::new();
        for error in errors.iter() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_error(error));
        }

        // Add summary line if multiple errors
        if errors.len() > 1 {
            output.push_str(&format!(
                "\nerror: aborting due to {} previous errors\n",
                errors.len()
            ));
        }

        output
    }

    /// Format all warnings, adding line numbers when multiple variables share the same name.
    ///
    /// This improves error messages by disambiguating when there are multiple unused
    /// variables with the same name (e.g., shadowed variables in different scopes).
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        if warnings.is_empty() {
            return String::new();
        }

        // Count occurrences of each unused variable name
        let mut var_name_counts: AHashMap<&str, usize> = AHashMap::new();
        for warning in warnings {
            if let Some(name) = warning.kind.unused_variable_name() {
                *var_name_counts.entry(name).or_insert(0) += 1;
            }
        }

        // Format each warning, adding line number if there are duplicates
        let mut output = String::new();
        for warning in warnings {
            let needs_line_number = warning
                .kind
                .unused_variable_name()
                .is_some_and(|name| var_name_counts.get(name).copied().unwrap_or(0) > 1);

            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_warning_internal(warning, needs_line_number));
        }
        output
    }

    /// Format a single warning into a string.
    pub fn format_warning(&self, warning: &CompileWarning) -> String {
        self.format_warning_internal(warning, false)
    }

    fn format_warning_internal(
        &self,
        warning: &CompileWarning,
        include_line_number: bool,
    ) -> String {
        // Get the message, optionally with line number for disambiguation
        let message = if include_line_number {
            if let Some(span) = warning.span() {
                let line = span.line_number(self.source_info.source);
                warning.kind.format_with_line(Some(line))
            } else {
                warning.to_string()
            }
        } else {
            warning.to_string()
        };

        self.format_diagnostic_impl(
            Level::Warning,
            &message,
            warning.span(),
            warning.diagnostic(),
        )
    }

    /// Internal implementation for formatting diagnostics.
    fn format_diagnostic_impl(
        &self,
        level: Level,
        message: &str,
        span: Option<Span>,
        diagnostic: &Diagnostic,
    ) -> String {
        // For diagnostics without a primary span, promote the first attached
        // label to the rendered snippet. Labels carry their own spans, so
        // dropping them here would silently hide source context.
        // Neutralize control bytes in every user-facing string before it
        // reaches the terminal (RUE-548). Titles, labels, notes, and helps can
        // interpolate module specifiers / paths; the source excerpt is handled
        // by RenderSource. Collect owned copies up front so the snippet
        // builder (which borrows &str) has something to point at.
        let message = sanitize_message(message);
        let sanitized_labels: Vec<Cow<str>> = diagnostic
            .labels
            .iter()
            .map(|l| sanitize_message(&l.message))
            .collect();
        let sanitized_notes: Vec<Cow<str>> = diagnostic
            .notes
            .iter()
            .map(|n| sanitize_message(&n.0))
            .collect();
        let sanitized_helps: Vec<Cow<str>> = diagnostic
            .helps
            .iter()
            .map(|h| sanitize_message(&h.0))
            .collect();

        let promoted_label = span.is_none().then(|| diagnostic.labels.first()).flatten();
        let promoted_label_idx = promoted_label.is_some().then_some(0usize);
        let Some(span) = span.or_else(|| promoted_label.map(|label| label.span)) else {
            let mut report = level.title(message.as_ref());
            // Add notes and helps as footers
            for note in &sanitized_notes {
                report = report.footer(Level::Note.title(note.as_ref()));
            }
            for help in &sanitized_helps {
                report = report.footer(Level::Help.title(help.as_ref()));
            }
            return format!("{}", self.renderer.render(report));
        };

        let render_source = RenderSource::new(self.source_info.source);
        let source_len = self.source_info.source.len();
        let (start, end) = render_source.map_span(span, source_len);

        // Build snippet with primary annotation
        let primary_annotation = level.span(start..end);
        let primary_annotation = if let Some(idx) = promoted_label_idx {
            primary_annotation.label(sanitized_labels[idx].as_ref())
        } else {
            primary_annotation
        };
        let mut snippet = Snippet::source(render_source.source.as_ref())
            .origin(self.source_info.path)
            .fold(true)
            .annotation(primary_annotation);

        // Add secondary labels as Info annotations
        for (idx, label) in diagnostic
            .labels
            .iter()
            .enumerate()
            .skip(usize::from(promoted_label.is_some()))
        {
            let (label_start, label_end) = render_source.map_span(label.span, source_len);
            snippet = snippet.annotation(
                Level::Info
                    .span(label_start..label_end)
                    .label(sanitized_labels[idx].as_ref()),
            );
        }

        let mut report = level.title(message.as_ref()).snippet(snippet);

        // Add notes and helps as footers
        for note in &sanitized_notes {
            report = report.footer(Level::Note.title(note.as_ref()));
        }
        for help in &sanitized_helps {
            report = report.footer(Level::Help.title(help.as_ref()));
        }

        let rendered = format!("{}", self.renderer.render(report));
        correct_origin_columns(
            rendered,
            &[(self.source_info.path, self.source_info.source)],
        )
    }
}

// ============================================================================
// Multi-File Diagnostic Formatter
// ============================================================================

/// A diagnostic formatter that supports diagnostics spanning multiple source files.
///
/// While [`DiagnosticFormatter`] works with a single source file, this formatter
/// can render errors that reference multiple files. This is useful for multi-file
/// compilation where an error might reference a type defined in one file while
/// being used in another.
///
/// # Example
///
/// ```ignore
/// use rue_compiler::FileId;
/// use rue_compiler::unstable::{MultiFileFormatter, SourceInfo};
///
/// // Create source infos for each file
/// let sources = vec![
///     (FileId::new(1), SourceInfo::new(&source1, "main.rue")),
///     (FileId::new(2), SourceInfo::new(&source2, "utils.rue")),
/// ];
///
/// let formatter = MultiFileFormatter::new(sources);
/// let error_output = formatter.format_error(&error);
/// eprintln!("{}", error_output);
/// ```
pub struct MultiFileFormatter<'a> {
    /// Mapping from FileId to source information.
    sources: AHashMap<FileId, SourceInfo<'a>>,
    /// The renderer for formatting diagnostics.
    renderer: Renderer,
}

impl<'a> MultiFileFormatter<'a> {
    /// Create a new multi-file formatter from an iterator of (FileId, SourceInfo) pairs.
    ///
    /// By default, uses automatic color detection based on whether stderr is a terminal.
    pub fn new(sources: impl IntoIterator<Item = (FileId, SourceInfo<'a>)>) -> Self {
        Self::with_color_choice(sources, ColorChoice::Auto)
    }

    /// Create a new multi-file formatter with explicit color choice.
    pub fn with_color_choice(
        sources: impl IntoIterator<Item = (FileId, SourceInfo<'a>)>,
        color_choice: ColorChoice,
    ) -> Self {
        let use_color = match color_choice {
            ColorChoice::Auto => std::io::stderr().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
        let renderer = if use_color {
            Renderer::styled()
        } else {
            Renderer::plain()
        };
        Self {
            sources: sources.into_iter().collect(),
            renderer,
        }
    }

    /// Get the source info for a file ID, if it exists.
    fn get_source(&self, file_id: FileId) -> Option<&SourceInfo<'a>> {
        self.sources.get(&file_id)
    }

    /// Get a fallback source info for a span whose file ID has no entry.
    ///
    /// Tries `FileId::DEFAULT` first (single-file paths register their source
    /// there or under a real ID). If that is absent, the only safe guess is
    /// when exactly one source is registered; with multiple sources, picking
    /// an arbitrary one would attribute the diagnostic to the wrong file
    /// nondeterministically (hash-map iteration order — RUE-175), so this
    /// returns None and the caller renders the message without a snippet.
    fn fallback_source(&self) -> Option<&SourceInfo<'a>> {
        self.sources.get(&FileId::DEFAULT).or_else(|| {
            if self.sources.len() == 1 {
                self.sources.values().next()
            } else {
                None
            }
        })
    }

    /// Format a compilation error into a string.
    ///
    /// The error is formatted with its error code, e.g.:
    /// `error[E0206]: type mismatch: expected i32, found bool`
    ///
    /// If the error or its labels reference multiple files, each file's
    /// snippet is shown separately.
    ///
    /// Internal compiler errors (ICEs) receive special formatting with
    /// bug report instructions.
    pub fn format_error(&self, error: &CompileError) -> String {
        let message_with_code = format!("[{}]: {}", error.kind.code(), error.kind);

        // Check if this is an internal compiler error
        let is_ice = matches!(
            error.kind.code(),
            ErrorCode::INTERNAL_ERROR | ErrorCode::INTERNAL_CODEGEN_ERROR
        );

        let mut output = self.format_diagnostic_impl(
            Level::Error,
            &message_with_code,
            error.span(),
            error.diagnostic(),
        );

        // Add ICE-specific notes and helps
        if is_ice {
            output.push_str("\nnote: this is a bug in the Rue compiler\n");
            output.push_str(
                "help: please report this issue at https://github.com/rue-language/rue/issues\n",
            );
        }

        output
    }

    /// Format multiple compilation errors into a string.
    ///
    /// Each error is formatted on its own line(s). A summary line is added at
    /// the end if there are multiple errors showing the total count.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        if errors.is_empty() {
            return String::new();
        }

        let mut output = String::new();
        for error in errors.iter() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_error(error));
        }

        if errors.len() > 1 {
            output.push_str(&format!(
                "\nerror: aborting due to {} previous errors\n",
                errors.len()
            ));
        }

        output
    }

    /// Format all warnings, adding line numbers when multiple variables share the same name.
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        if warnings.is_empty() {
            return String::new();
        }

        // Count occurrences of each unused variable name
        let mut var_name_counts: AHashMap<&str, usize> = AHashMap::new();
        for warning in warnings {
            if let Some(name) = warning.kind.unused_variable_name() {
                *var_name_counts.entry(name).or_insert(0) += 1;
            }
        }

        // Format each warning
        let mut output = String::new();
        for warning in warnings {
            let needs_line_number = warning
                .kind
                .unused_variable_name()
                .is_some_and(|name| var_name_counts.get(name).copied().unwrap_or(0) > 1);

            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&self.format_warning_internal(warning, needs_line_number));
        }
        output
    }

    /// Format a single warning into a string.
    pub fn format_warning(&self, warning: &CompileWarning) -> String {
        self.format_warning_internal(warning, false)
    }

    fn format_warning_internal(
        &self,
        warning: &CompileWarning,
        include_line_number: bool,
    ) -> String {
        // Get the message, optionally with line number for disambiguation
        let message = if include_line_number {
            if let Some(span) = warning.span() {
                if let Some(source_info) = self
                    .get_source(span.file_id)
                    .or_else(|| self.fallback_source())
                {
                    let line = span.line_number(source_info.source);
                    warning.kind.format_with_line(Some(line))
                } else {
                    warning.to_string()
                }
            } else {
                warning.to_string()
            }
        } else {
            warning.to_string()
        };

        self.format_diagnostic_impl(
            Level::Warning,
            &message,
            warning.span(),
            warning.diagnostic(),
        )
    }

    /// Internal implementation for formatting diagnostics with multi-file support.
    fn format_diagnostic_impl(
        &self,
        level: Level,
        message: &str,
        span: Option<Span>,
        diagnostic: &Diagnostic,
    ) -> String {
        // Neutralize control bytes in every user-facing string before it
        // reaches the terminal (RUE-548); see the single-file twin. Owned
        // copies collected up front so the &str-borrowing snippet builder and
        // `file_spans` have stable referents.
        let message = sanitize_message(message);
        let sanitized_labels: Vec<Cow<str>> = diagnostic
            .labels
            .iter()
            .map(|l| sanitize_message(&l.message))
            .collect();
        let sanitized_notes: Vec<Cow<str>> = diagnostic
            .notes
            .iter()
            .map(|n| sanitize_message(&n.0))
            .collect();
        let sanitized_helps: Vec<Cow<str>> = diagnostic
            .helps
            .iter()
            .map(|h| sanitize_message(&h.0))
            .collect();

        // For diagnostics without a primary span, promote the first attached
        // label to the rendered snippet. Labels carry their own file IDs and
        // spans, so dropping them here would silently hide source context.
        let promoted_label = span.is_none().then(|| diagnostic.labels.first()).flatten();
        let Some(primary_span) = span.or_else(|| promoted_label.map(|label| label.span)) else {
            let mut report = level.title(message.as_ref());
            for note in &sanitized_notes {
                report = report.footer(Level::Note.title(note.as_ref()));
            }
            for help in &sanitized_helps {
                report = report.footer(Level::Help.title(help.as_ref()));
            }
            return format!("{}", self.renderer.render(report));
        };

        // Collect all file IDs we need to show
        let mut file_spans: AHashMap<FileId, Vec<(Span, Option<&str>, Level)>> = AHashMap::new();

        // Add primary span (promoted label is index 0 when present).
        file_spans.entry(primary_span.file_id).or_default().push((
            primary_span,
            promoted_label.map(|_| sanitized_labels[0].as_ref()),
            level,
        ));

        // Add secondary labels
        for (idx, label) in diagnostic
            .labels
            .iter()
            .enumerate()
            .skip(usize::from(promoted_label.is_some()))
        {
            file_spans.entry(label.span.file_id).or_default().push((
                label.span,
                Some(sanitized_labels[idx].as_ref()),
                Level::Info,
            ));
        }

        // Build report with snippets for each file
        let mut report = level.title(message.as_ref());

        // Process files in a deterministic order (by FileId)
        let mut file_ids: Vec<_> = file_spans.keys().copied().collect();
        file_ids.sort_by_key(|id| id.0);

        let mut render_sources = Vec::with_capacity(file_ids.len());
        for file_id in &file_ids {
            let source_info = self.get_source(*file_id).or_else(|| self.fallback_source());

            let Some(source_info) = source_info else {
                // No source available for this file. Refuse to guess a file
                // (see fallback_source); surface the problem instead of
                // silently misattributing the span (RUE-175).
                report = report.footer(Level::Note.title(
                    "source snippet unavailable: span has an unknown file id \
                     (this is a bug in the Rue compiler)",
                ));
                continue;
            };

            render_sources.push((*file_id, source_info, RenderSource::new(source_info.source)));
        }

        for (file_id, source_info, render_source) in &render_sources {
            let spans = &file_spans[file_id];

            // Build snippet with all annotations for this file
            let mut snippet = Snippet::source(render_source.source.as_ref())
                .origin(source_info.path)
                .fold(true);

            let source_len = source_info.source.len();
            for (span, label, span_level) in spans {
                let (start, end) = render_source.map_span(*span, source_len);

                let annotation = span_level.span(start..end);
                let annotation = if let Some(label_text) = label {
                    annotation.label(label_text)
                } else {
                    annotation
                };
                snippet = snippet.annotation(annotation);
            }

            report = report.snippet(snippet);
        }

        // Add notes and helps as footers
        for note in &sanitized_notes {
            report = report.footer(Level::Note.title(note.as_ref()));
        }
        for help in &sanitized_helps {
            report = report.footer(Level::Help.title(help.as_ref()));
        }

        let rendered = format!("{}", self.renderer.render(report));
        let files: Vec<(&str, &str)> = render_sources
            .iter()
            .map(|(_, source_info, _)| (source_info.path, source_info.source))
            .collect();
        correct_origin_columns(rendered, &files)
    }
}

// ============================================================================
// JSON Diagnostic Output
// ============================================================================

/// A diagnostic formatted for JSON output.
///
/// This structure is designed to be compatible with common editor protocols
/// (LSP, cargo's JSON format) while containing all information needed for
/// rich diagnostic display.
#[derive(Debug)]
pub struct JsonDiagnostic {
    /// Error or warning code (e.g., "E0206").
    pub code: String,
    /// The diagnostic message.
    pub message: String,
    /// Severity level: "error" or "warning".
    pub severity: &'static str,
    /// Primary and secondary spans with labels.
    pub spans: Vec<JsonSpan>,
    /// Suggested fixes that can be applied.
    pub suggestions: Vec<JsonSuggestion>,
    /// Additional notes providing context.
    pub notes: Vec<String>,
    /// Additional help messages.
    pub helps: Vec<String>,
}

/// A span in JSON format with file location and labels.
#[derive(Debug)]
pub struct JsonSpan {
    /// Source file path.
    pub file: String,
    /// Start byte offset (0-indexed).
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
    /// Line number (1-indexed).
    pub line: u32,
    /// Column number (1-indexed).
    pub column: u32,
    /// Label text for this span.
    pub label: Option<String>,
    /// Whether this is the primary span.
    pub primary: bool,
}

/// A suggested fix in JSON format.
#[derive(Debug)]
pub struct JsonSuggestion {
    /// Human-readable description.
    pub message: String,
    /// File containing the span.
    pub file: String,
    /// Start byte offset.
    pub start: u32,
    /// End byte offset.
    pub end: u32,
    /// Replacement text.
    pub replacement: String,
    /// Applicability level.
    pub applicability: String,
}

/// Clamp a diagnostic span to the source it will be published against.
///
/// Compiler spans normally already satisfy these bounds, but diagnostics can
/// also be produced while recovering from invalid input or from a stale
/// source projection. JSON is a versioned consumer-facing surface, so never
/// publish a range that a consumer cannot safely slice or whose end precedes
/// its start.
fn normalize_json_range(span: Span, source: &str) -> (u32, u32) {
    let source_len = source.len().min(u32::MAX as usize) as u32;
    let start = span.start.min(source_len);
    let end = span.end.min(source_len).max(start);
    (start, end)
}

/// Build one JSON span using normalized source coordinates.
fn make_json_span(
    path: &str,
    source: &str,
    span: Span,
    label: Option<String>,
    primary: bool,
) -> JsonSpan {
    let (start, end) = normalize_json_range(span, source);
    let (line, column) = offset_to_line_col(source, start);
    JsonSpan {
        file: path.to_string(),
        start,
        end,
        line,
        column,
        label,
        primary,
    }
}

/// Assemble the published span list, promoting the first emitted label when
/// the declared primary is absent or cannot be resolved to a source.
fn finalize_json_spans(primary: Option<JsonSpan>, mut labels: Vec<JsonSpan>) -> Vec<JsonSpan> {
    let mut spans = Vec::with_capacity(primary.is_some() as usize + labels.len());
    if let Some(mut primary) = primary {
        primary.primary = true;
        primary.label = None;
        spans.push(primary);
    } else if let Some(first) = labels.first_mut() {
        first.primary = true;
        first.label = None;
    }
    spans.extend(labels);
    spans
}

impl JsonDiagnostic {
    /// Serialize this diagnostic to a JSON string.
    pub fn to_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "code".to_string(),
            serde_json::Value::String(self.code.clone()),
        );
        obj.insert(
            "message".to_string(),
            serde_json::Value::String(self.message.clone()),
        );
        obj.insert(
            "severity".to_string(),
            serde_json::Value::String(self.severity.to_string()),
        );

        // Spans
        let spans: Vec<serde_json::Value> = self
            .spans
            .iter()
            .map(|s| {
                let mut span = serde_json::Map::new();
                span.insert(
                    "file".to_string(),
                    serde_json::Value::String(s.file.clone()),
                );
                span.insert(
                    "start".to_string(),
                    serde_json::Value::Number(s.start.into()),
                );
                span.insert("end".to_string(), serde_json::Value::Number(s.end.into()));
                span.insert("line".to_string(), serde_json::Value::Number(s.line.into()));
                span.insert(
                    "column".to_string(),
                    serde_json::Value::Number(s.column.into()),
                );
                if let Some(label) = &s.label {
                    span.insert(
                        "label".to_string(),
                        serde_json::Value::String(label.clone()),
                    );
                } else {
                    span.insert("label".to_string(), serde_json::Value::Null);
                }
                span.insert("primary".to_string(), serde_json::Value::Bool(s.primary));
                serde_json::Value::Object(span)
            })
            .collect();
        obj.insert("spans".to_string(), serde_json::Value::Array(spans));

        // Suggestions
        let suggestions: Vec<serde_json::Value> = self
            .suggestions
            .iter()
            .map(|s| {
                let mut sugg = serde_json::Map::new();
                sugg.insert(
                    "message".to_string(),
                    serde_json::Value::String(s.message.clone()),
                );
                sugg.insert(
                    "file".to_string(),
                    serde_json::Value::String(s.file.clone()),
                );
                sugg.insert(
                    "start".to_string(),
                    serde_json::Value::Number(s.start.into()),
                );
                sugg.insert("end".to_string(), serde_json::Value::Number(s.end.into()));
                sugg.insert(
                    "replacement".to_string(),
                    serde_json::Value::String(s.replacement.clone()),
                );
                sugg.insert(
                    "applicability".to_string(),
                    serde_json::Value::String(s.applicability.clone()),
                );
                serde_json::Value::Object(sugg)
            })
            .collect();
        obj.insert(
            "suggestions".to_string(),
            serde_json::Value::Array(suggestions),
        );

        // Notes and helps
        let notes: Vec<serde_json::Value> = self
            .notes
            .iter()
            .map(|n| serde_json::Value::String(n.clone()))
            .collect();
        obj.insert("notes".to_string(), serde_json::Value::Array(notes));

        let helps: Vec<serde_json::Value> = self
            .helps
            .iter()
            .map(|h| serde_json::Value::String(h.clone()))
            .collect();
        obj.insert("helps".to_string(), serde_json::Value::Array(helps));

        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Formats diagnostics as JSON for machine consumption.
///
/// Use this formatter when outputting to tools like editors, CI systems,
/// or any context requiring machine-readable output.
pub struct JsonDiagnosticFormatter<'a> {
    source_info: &'a SourceInfo<'a>,
}

impl<'a> JsonDiagnosticFormatter<'a> {
    /// Create a new JSON diagnostic formatter.
    pub fn new(source_info: &'a SourceInfo<'a>) -> Self {
        Self { source_info }
    }

    /// Format a compile error as JSON.
    pub fn format_error(&self, error: &CompileError) -> JsonDiagnostic {
        let diag = error.diagnostic();
        let primary_span = error.span().map(|span| {
            make_json_span(
                self.source_info.path,
                self.source_info.source,
                span,
                None,
                true,
            )
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .map(|label| {
                make_json_span(
                    self.source_info.path,
                    self.source_info.source,
                    label.span,
                    Some(label.message.clone()),
                    false,
                )
            })
            .collect();
        let spans = finalize_json_spans(primary_span, secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .map(|s| JsonSuggestion {
                message: s.message.clone(),
                file: self.source_info.path.to_string(),
                start: s.span.start,
                end: s.span.end,
                replacement: s.replacement.clone(),
                applicability: s.applicability.to_string(),
            })
            .collect();

        JsonDiagnostic {
            code: format!("{}", error.kind.code()),
            message: format!("{}", error.kind),
            severity: "error",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format a compile warning as JSON.
    pub fn format_warning(&self, warning: &CompileWarning) -> JsonDiagnostic {
        let diag = warning.diagnostic();
        let primary_span = warning.span().map(|span| {
            make_json_span(
                self.source_info.path,
                self.source_info.source,
                span,
                None,
                true,
            )
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .map(|label| {
                make_json_span(
                    self.source_info.path,
                    self.source_info.source,
                    label.span,
                    Some(label.message.clone()),
                    false,
                )
            })
            .collect();
        let spans = finalize_json_spans(primary_span, secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .map(|s| JsonSuggestion {
                message: s.message.clone(),
                file: self.source_info.path.to_string(),
                start: s.span.start,
                end: s.span.end,
                replacement: s.replacement.clone(),
                applicability: s.applicability.to_string(),
            })
            .collect();

        JsonDiagnostic {
            code: String::new(), // Warnings don't have codes yet
            message: format!("{}", warning.kind),
            severity: "warning",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format multiple errors as a JSON array string.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        let diagnostics: Vec<String> = errors
            .iter()
            .map(|e| self.format_error(e).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }

    /// Format multiple warnings as a JSON array string.
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        let diagnostics: Vec<String> = warnings
            .iter()
            .map(|w| self.format_warning(w).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }
}

// ============================================================================
// Multi-File JSON Diagnostic Formatter
// ============================================================================

/// Formats diagnostics as JSON with support for multiple source files.
///
/// This formatter maps FileIds to their source information, enabling correct
/// file path and line/column output for diagnostics spanning multiple files.
///
/// # Example
///
/// ```ignore
/// use rue_compiler::FileId;
/// use rue_compiler::unstable::{MultiFileJsonFormatter, SourceInfo};
///
/// let sources = vec![
///     (FileId::new(1), SourceInfo::new(&source1, "main.rue")),
///     (FileId::new(2), SourceInfo::new(&source2, "utils.rue")),
/// ];
///
/// let formatter = MultiFileJsonFormatter::new(sources);
/// let json = formatter.format_error(&error).to_json();
/// ```
pub struct MultiFileJsonFormatter<'a> {
    /// Mapping from FileId to source information.
    sources: AHashMap<FileId, SourceInfo<'a>>,
}

impl<'a> MultiFileJsonFormatter<'a> {
    /// Create a new multi-file JSON diagnostic formatter.
    pub fn new(sources: impl IntoIterator<Item = (FileId, SourceInfo<'a>)>) -> Self {
        Self {
            sources: sources.into_iter().collect(),
        }
    }

    /// Get the source info for a file ID, if it exists.
    fn get_source(&self, file_id: FileId) -> Option<&SourceInfo<'a>> {
        self.sources.get(&file_id)
    }

    /// Get a fallback source info (for FileId::DEFAULT or when no specific source is found).
    ///
    /// Like [`MultiFileFormatter::fallback_source`], this only guesses when
    /// the choice is unambiguous: `FileId::DEFAULT` if registered, else the
    /// sole registered source. With multiple sources an unknown file ID must
    /// not be attributed to an arbitrary file (RUE-175).
    fn fallback_source(&self) -> Option<&SourceInfo<'a>> {
        self.sources.get(&FileId::DEFAULT).or_else(|| {
            if self.sources.len() == 1 {
                self.sources.values().next()
            } else {
                None
            }
        })
    }

    /// Get the path and source for a span, using the span's file ID.
    fn source_for_span(&self, span: Span) -> Option<(&str, &str)> {
        self.get_source(span.file_id)
            .or_else(|| self.fallback_source())
            .map(|info| (info.path, info.source))
    }

    /// Format a compile error as JSON.
    pub fn format_error(&self, error: &CompileError) -> JsonDiagnostic {
        let diag = error.diagnostic();

        let primary_span = error.span().and_then(|span| {
            self.source_for_span(span)
                .map(|(path, source)| make_json_span(path, source, span, None, true))
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .filter_map(|label| {
                self.source_for_span(label.span).map(|(path, source)| {
                    make_json_span(path, source, label.span, Some(label.message.clone()), false)
                })
            })
            .collect();
        let spans = finalize_json_spans(primary_span, secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .filter_map(|s| {
                self.source_for_span(s.span)
                    .map(|(path, _)| JsonSuggestion {
                        message: s.message.clone(),
                        file: path.to_string(),
                        start: s.span.start,
                        end: s.span.end,
                        replacement: s.replacement.clone(),
                        applicability: s.applicability.to_string(),
                    })
            })
            .collect();

        JsonDiagnostic {
            code: format!("{}", error.kind.code()),
            message: format!("{}", error.kind),
            severity: "error",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format a compile warning as JSON.
    pub fn format_warning(&self, warning: &CompileWarning) -> JsonDiagnostic {
        let diag = warning.diagnostic();

        let primary_span = warning.span().and_then(|span| {
            self.source_for_span(span)
                .map(|(path, source)| make_json_span(path, source, span, None, true))
        });

        let secondary_spans: Vec<JsonSpan> = diag
            .labels
            .iter()
            .filter_map(|label| {
                self.source_for_span(label.span).map(|(path, source)| {
                    make_json_span(path, source, label.span, Some(label.message.clone()), false)
                })
            })
            .collect();
        let spans = finalize_json_spans(primary_span, secondary_spans);

        let suggestions: Vec<JsonSuggestion> = diag
            .suggestions
            .iter()
            .filter_map(|s| {
                self.source_for_span(s.span)
                    .map(|(path, _)| JsonSuggestion {
                        message: s.message.clone(),
                        file: path.to_string(),
                        start: s.span.start,
                        end: s.span.end,
                        replacement: s.replacement.clone(),
                        applicability: s.applicability.to_string(),
                    })
            })
            .collect();

        JsonDiagnostic {
            code: String::new(), // Warnings don't have codes yet
            message: format!("{}", warning.kind),
            severity: "warning",
            spans,
            suggestions,
            notes: diag.notes.iter().map(|n| n.0.clone()).collect(),
            helps: diag.helps.iter().map(|h| h.0.clone()).collect(),
        }
    }

    /// Format multiple errors as a JSON array string.
    pub fn format_errors(&self, errors: &CompileErrors) -> String {
        let diagnostics: Vec<String> = errors
            .iter()
            .map(|e| self.format_error(e).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }

    /// Format multiple warnings as a JSON array string.
    pub fn format_warnings(&self, warnings: &[CompileWarning]) -> String {
        let diagnostics: Vec<String> = warnings
            .iter()
            .map(|w| self.format_warning(w).to_json())
            .collect();
        format!("[{}]", diagnostics.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, Suggestion, WarningKind};

    #[test]
    fn test_format_error_with_span() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Should include error code
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
        assert!(output.contains("expected i32"));
        assert!(output.contains("found bool"));
        assert!(output.contains("test.rue"));
    }

    #[test]
    fn test_format_zero_width_eof_span_with_and_without_trailing_newline() {
        for (source, origin, excerpt, caret, expected_line_col) in [
            (
                "fn main() -> i32 { 0 }",
                "--> test.rue:1:23",
                "1 | fn main() -> i32 { 0 }",
                "  |                       ^",
                (1, 23),
            ),
            (
                "fn main() -> i32 { 0 }\n",
                "--> test.rue:2:1",
                "2 | \u{200b}",
                "  | ^",
                (2, 1),
            ),
        ] {
            let source_info = SourceInfo::new(source, "test.rue");
            let text = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);
            let at_eof = source.len() as u32;
            let error = CompileError::new(ErrorKind::NoMainFunction, Span::new(at_eof, at_eof));

            let output = text.format_error(&error);
            assert!(output.contains(origin), "missing origin in:\n{output}");
            assert!(output.contains(excerpt), "missing excerpt in:\n{output}");
            assert!(output.contains(caret), "missing EOF caret in:\n{output}");

            let json = JsonDiagnosticFormatter::new(&source_info).format_error(&error);
            let span = &json.spans[0];
            assert_eq!((span.start, span.end), (at_eof, at_eof));
            assert_eq!((span.line, span.column), expected_line_col);
            assert!(span.primary);
        }
    }

    #[test]
    fn test_format_multibyte_utf8_span_pins_text_caret_and_json_coordinates() {
        let source = "fn main() -> i32 {\n    let π = true;\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let text = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);
        let start = source.find("true").unwrap() as u32;
        let error = CompileError::new(ErrorKind::NoMainFunction, Span::new(start, start + 4));

        let output = text.format_error(&error);
        assert!(output.contains("2 |     let π = true;"));
        assert!(
            output.contains("  |             ^^^^"),
            "caret drifted after UTF-8 text:\n{output}"
        );

        let json = JsonDiagnosticFormatter::new(&source_info).format_error(&error);
        let span = &json.spans[0];
        assert_eq!((span.start, span.end), (32, 36));
        assert_eq!((span.line, span.column), (2, 13));
    }

    #[test]
    fn test_format_multiline_span_pins_text_excerpt_and_json_range() {
        let source = "fn main() -> i32 {\n    first\n    second\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let text = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);
        let start = source.find("first").unwrap() as u32;
        let end = source.find("second").unwrap() as u32 + 3;
        let error = CompileError::new(ErrorKind::NoMainFunction, Span::new(start, end));

        let output = text.format_error(&error);
        assert!(output.contains("2 | /     first\n3 | |     second"));
        assert!(
            output.contains("  | |_______^"),
            "multiline underline changed:\n{output}"
        );

        let json = JsonDiagnosticFormatter::new(&source_info).format_error(&error);
        let span = &json.spans[0];
        assert_eq!((span.start, span.end), (23, 36));
        assert_eq!((span.line, span.column), (2, 5));
    }

    #[test]
    fn test_format_error_expands_tabs_before_mapping_caret() {
        let source = "fn main() -> i32 {\n\treturn nope;\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let start = source.find("nope").unwrap() as u32;
        let error = CompileError::new(
            ErrorKind::UndefinedVariable("nope".to_string()),
            Span::new(start, start + 4),
        );

        let output = formatter.format_error(&error);

        assert!(output.contains("2 |     return nope;"));
        assert!(
            output.contains("  |            ^^^^"),
            "caret should stay under `nope` after expanding the leading tab:\n{}",
            output
        );
    }

    #[test]
    fn test_fix_origin_line_anchors_suffix_colliding_paths_in_registration_order() {
        let tabbed = "fn main() -> i32 {\n\treturn nope;\n}";
        let plain = "fn main() -> i32 {\n    return 0;\n}";
        let rendered = "  --> lib/main.rue:2:12\n";

        for files in [
            vec![("main.rue", plain), ("lib/main.rue", tabbed)],
            vec![("lib/main.rue", tabbed), ("main.rue", plain)],
        ] {
            assert_eq!(
                fix_origin_line(rendered, &files),
                Some("  --> lib/main.rue:2:9\n".to_string()),
                "registration order must not let main.rue match inside lib/main.rue"
            );
        }
    }

    #[test]
    fn test_fix_origin_line_rejects_same_length_wrong_path() {
        let wrong_source = "fn main() -> i32 {\n\t\treturn nope;\n}";
        let correct_source = "fn main() -> i32 {\n\treturn nope;\n}";
        let rendered = "  --> lib/main.rue:2:12\n";

        // `app/main.rue` has the same byte length as the rendered path, but
        // its source would produce a different non-noop correction. Path
        // equality must be checked before parsing the coordinate for it.
        let files = [
            ("app/main.rue", wrong_source),
            ("lib/main.rue", correct_source),
        ];
        assert_eq!(
            fix_origin_line(rendered, &files),
            Some("  --> lib/main.rue:2:9\n".to_string())
        );
    }

    #[test]
    fn test_fix_origin_line_continues_after_noop_candidate_and_preserves_windows_paths() {
        let tabbed = "fn main() -> i32 {\n\treturn nope;\n}";
        let plain = "fn main() -> i32 {\n    return 0;\n}";

        // Duplicate path entries model an earlier candidate that parses but
        // cannot change the reported column; the later candidate is still
        // considered. This also pins exact path anchoring for a Windows path.
        let path = r"C:\project\lib\main.rue";
        let rendered = format!("  --> {path}:2:12\n");
        let files = [(path, plain), (path, tabbed)];
        assert_eq!(
            fix_origin_line(&rendered, &files),
            Some(format!("  --> {path}:2:9\n")),
        );
    }

    #[test]
    fn test_fix_origin_line_single_file_controls() {
        let tabbed = "fn main() -> i32 {\n\treturn nope;\n}";
        let plain = "fn main() -> i32 {\n    return nope;\n}";

        assert_eq!(
            fix_origin_line("  --> main.rue:2:12\n", &[("main.rue", tabbed)]),
            Some("  --> main.rue:2:9\n".to_string())
        );
        assert_eq!(
            fix_origin_line("  --> main.rue:2:12\n", &[("main.rue", plain)]),
            None
        );
    }

    #[test]
    fn test_tab_origin_column_matches_original_source_not_expanded() {
        // RUE-556: the `--> path:line:col` origin must name the ORIGINAL-source
        // column (the tab is one character), matching JSON diagnostics and what
        // editors expect — even though the caret uses the tab-expanded excerpt.
        let source = "fn main() -> i32 {\n\treturn nope;\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let start = source.find("nope").unwrap() as u32;
        let error = CompileError::new(
            ErrorKind::UndefinedVariable("nope".to_string()),
            Span::new(start, start + 4),
        );
        let output = formatter.format_error(&error);

        // `\t` + "return " = 7 chars before `nope` -> column 9 in the original.
        assert_eq!(
            crate::Span::new(start, start + 4).line_number(source),
            2,
            "sanity: span is on line 2"
        );
        assert!(
            output.contains("--> test.rue:2:9"),
            "origin should report the original-source column 9, not the \
             tab-expanded 12:\n{}",
            output
        );
        assert!(
            !output.contains("--> test.rue:2:12"),
            "origin must not report the tab-expanded column:\n{}",
            output
        );
        // Caret alignment is still driven by the expanded excerpt.
        assert!(
            output.contains("  |            ^^^^"),
            "caret must remain under `nope`:\n{}",
            output
        );
    }

    #[test]
    fn test_space_indent_origin_column_is_unchanged() {
        // The correction must be a no-op when there are no tabs: spaces are real
        // characters, so the origin column already equals the original column.
        let source = "fn main() -> i32 {\n    return nope;\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let start = source.find("nope").unwrap() as u32;
        let error = CompileError::new(
            ErrorKind::UndefinedVariable("nope".to_string()),
            Span::new(start, start + 4),
        );
        let output = formatter.format_error(&error);

        // Four spaces + "return " = 11 chars before `nope` -> column 12.
        assert!(
            output.contains("--> test.rue:2:12"),
            "space-indented origin column should be untouched:\n{}",
            output
        );
    }

    #[test]
    fn test_original_col_for_expanded_maps_tab_columns() {
        // Unit-level check of the inverse mapping: `\treturn nope;`, the token
        // `nope` sits at expanded column 12 but original column 9.
        let source = "fn main() {\n\treturn nope;\n}";
        assert_eq!(original_col_for_expanded(source, 2, 12), 9);
        // Two leading tabs: expanded column 16 -> original column 10.
        let two = "fn main() {\n\t\treturn nope;\n}";
        assert_eq!(original_col_for_expanded(two, 2, 16), 10);
        // A column before the first tab is unchanged.
        assert_eq!(original_col_for_expanded(source, 1, 4), 4);
    }

    #[test]
    fn test_format_error_strips_crlf_without_shifting_span() {
        let source = "fn main() -> i32 {\r\n    let x: i32 = true;\r\n    x\r\n}\r\n";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let start = source.find("let").unwrap() as u32;
        let end = source.find(';').unwrap() as u32 + 1;
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(start, end),
        );

        let output = formatter.format_error(&error);

        assert!(!output.contains('\r'), "diagnostic leaked CR:\n{}", output);
        assert!(output.contains("2 |     let x: i32 = true;"));
        assert!(output.contains("  |     ^^^^^^^^^^^^^^^^^^"));
    }

    #[test]
    fn test_format_error_without_span() {
        let source = "fn foo() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        let output = formatter.format_error(&error);
        // Should include error code
        assert!(output.contains("[E0200]"));
        assert!(output.contains("no main function"));
    }

    #[test]
    fn test_format_error_without_span_promotes_label() {
        let source = "fn foo() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::NoMainFunction)
            .with_label("candidate function is here", Span::new(3, 6));

        let output = formatter.format_error(&error);
        assert!(output.contains("[E0200]"));
        assert!(output.contains("no main function"));
        assert!(output.contains("test.rue"));
        assert!(output.contains("candidate function is here"));
    }

    #[test]
    fn test_format_warning() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::new(23, 24),
        );

        let output = formatter.format_warning(&warning);
        assert!(output.contains("unused variable"));
        assert!(output.contains("'x'"));
    }

    #[test]
    fn test_format_warnings_with_duplicates() {
        let source = "fn main() -> i32 {\n    let x = 1;\n    let x = 2;\n    0\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let warnings = vec![
            CompileWarning::new(
                WarningKind::UnusedVariable("x".to_string()),
                Span::new(23, 24),
            ),
            CompileWarning::new(
                WarningKind::UnusedVariable("x".to_string()),
                Span::new(36, 37),
            ),
        ];

        let output = formatter.format_warnings(&warnings);
        // Should include line numbers for disambiguation
        assert!(output.contains("line"));
    }

    #[test]
    fn test_format_warnings_empty() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let output = formatter.format_warnings(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn test_format_error_with_help() {
        let source = "fn main() -> i32 { x = 1; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::AssignToImmutable("x".to_string()),
            Span::new(19, 20),
        )
        .with_help("consider making `x` mutable: `let mut x`");

        let output = formatter.format_error(&error);
        assert!(output.contains("help"));
        assert!(output.contains("mutable"));
    }

    #[test]
    fn test_format_error_with_label() {
        let source = "fn main() -> i32 { if true { 1 } else { false } }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(40, 45),
        )
        .with_label("then branch is here", Span::new(29, 30));

        let output = formatter.format_error(&error);
        assert!(output.contains("then branch"));
    }

    #[test]
    fn test_format_errors_empty() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let errors = CompileErrors::new();
        let output = formatter.format_errors(&errors);
        assert!(output.is_empty());
    }

    #[test]
    fn test_format_errors_single() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        ));

        let output = formatter.format_errors(&errors);
        assert!(output.contains("type mismatch"));
        // Single error should not have summary line
        assert!(!output.contains("aborting"));
    }

    #[test]
    fn test_format_errors_multiple() {
        let source = "fn main() -> i32 {\n    let x = 1 + true;\n    let y = false - 1;\n    0\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(32, 36),
        ));
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "bool".to_string(),
                found: "i32".to_string(),
            },
            Span::new(58, 59),
        ));

        let output = formatter.format_errors(&errors);
        // Should contain both errors
        assert!(output.contains("expected i32, found bool"));
        assert!(output.contains("expected bool, found i32"));
        // Should have summary line
        assert!(output.contains("aborting due to 2 previous errors"));
    }

    #[test]
    fn test_color_choice_never() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Never);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Output should not contain ANSI escape codes
        assert!(!output.contains("\x1b["));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_format_error_with_invalid_span() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        // Span that extends beyond source length
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(20, 1000), // end is way beyond source length
        );

        // Should not panic, should clamp to valid range
        let output = formatter.format_error(&error);
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_format_error_with_reversed_span() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        // Span with start > end (should be clamped so start == end)
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(20, 10), // start > end
        );

        // Should not panic
        let output = formatter.format_error(&error);
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_color_choice_always() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::with_color_choice(&source_info, ColorChoice::Always);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Output should contain ANSI escape codes
        assert!(output.contains("\x1b["));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_ice_formatting() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::InternalError(
            "unexpected type in codegen".to_string(),
        ));

        let output = formatter.format_error(&error);
        // Should contain ICE error code
        assert!(output.contains("[E9000]"));
        assert!(output.contains("internal compiler error"));
        // Should contain ICE-specific note and help
        assert!(output.contains("note: this is a bug in the Rue compiler"));
        assert!(output.contains("help: please report this issue"));
        assert!(output.contains("github.com/rue-language/rue/issues"));
    }

    #[test]
    fn test_ice_codegen_formatting() {
        let source = "fn main() -> i32 { 42 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::InternalCodegenError(
            "failed to emit instruction".to_string(),
        ));

        let output = formatter.format_error(&error);
        // Should contain ICE codegen error code
        assert!(output.contains("[E9001]"));
        assert!(output.contains("internal codegen error"));
        // Should contain ICE-specific note and help
        assert!(output.contains("note: this is a bug in the Rue compiler"));
        assert!(output.contains("help: please report this issue"));
    }

    #[test]
    fn test_non_ice_no_extra_formatting() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = DiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let output = formatter.format_error(&error);
        // Non-ICE errors should not have ICE-specific messages
        assert!(!output.contains("note: this is a bug in the Rue compiler"));
        assert!(!output.contains("please report this issue"));
    }

    // ========================================================================
    // JSON Formatting Tests
    // ========================================================================

    #[test]
    fn test_json_format_error() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.severity, "error");
        assert_eq!(json_diag.code, "E0206");
        assert!(json_diag.message.contains("type mismatch"));
        assert_eq!(json_diag.spans.len(), 1);
        assert_eq!(json_diag.spans[0].file, "test.rue");
        assert_eq!(json_diag.spans[0].start, 23);
        assert_eq!(json_diag.spans[0].end, 27);
        assert!(json_diag.spans[0].primary);
    }

    #[test]
    fn test_json_format_error_line_col() {
        let source = "fn main() -> i32 {\n    1 + true\n}";
        //                            ^--- line 2, col 9 (0-indexed: 23)
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(27, 31), // "true" on line 2
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.spans[0].line, 2);
        assert!(json_diag.spans[0].column > 1); // Column should be > 1 (indented)
    }

    #[test]
    fn test_json_format_error_with_suggestion() {
        let source = "fn main() -> i32 { x = 1; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::AssignToImmutable("x".to_string()),
            Span::new(19, 20),
        )
        .with_suggestion(Suggestion::machine_applicable(
            "add mut",
            Span::new(4, 5),
            "mut x",
        ));

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.suggestions.len(), 1);
        assert_eq!(json_diag.suggestions[0].message, "add mut");
        assert_eq!(json_diag.suggestions[0].replacement, "mut x");
        assert_eq!(json_diag.suggestions[0].applicability, "MachineApplicable");
    }

    #[test]
    fn test_json_format_warning() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::new(23, 24),
        );

        let json_diag = formatter.format_warning(&warning);
        assert_eq!(json_diag.severity, "warning");
        assert!(json_diag.message.contains("unused variable"));
    }

    #[test]
    fn test_json_spanless_diagnostics_promote_labels() {
        let source_info = SourceInfo::new("fn main() -> i32 { 0 }", "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::without_span(ErrorKind::NoMainFunction)
            .with_label("candidate", Span::new(3, 7))
            .with_label("related", Span::new(8, 12));
        let json_error = formatter.format_error(&error);
        assert_eq!(json_error.spans.len(), 2);
        assert!(json_error.spans[0].primary);
        assert_eq!(json_error.spans[0].label, None);
        assert!(!json_error.spans[1].primary);
        assert_eq!(json_error.spans[1].label.as_deref(), Some("related"));

        let warning = CompileWarning::without_span(WarningKind::UnusedVariable("x".into()))
            .with_label("candidate", Span::new(3, 7));
        let json_warning = formatter.format_warning(&warning);
        assert_eq!(json_warning.spans.len(), 1);
        assert!(json_warning.spans[0].primary);
        assert_eq!(json_warning.spans[0].label, None);
    }

    #[test]
    fn test_json_spans_clamp_reversed_and_out_of_range_ranges() {
        let source = "fn main() -> i32 { 0 }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::NoMainFunction,
            Span::new(source.len() as u32 + 10, 1),
        );
        let json_error = formatter.format_error(&error);
        assert_eq!(json_error.spans[0].start as usize, source.len());
        assert_eq!(json_error.spans[0].end as usize, source.len());
        assert_eq!(json_error.spans[0].line, 1);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".into()),
            Span::new(10, 1_000),
        );
        let json_warning = formatter.format_warning(&warning);
        assert_eq!(json_warning.spans[0].start, 10);
        assert_eq!(json_warning.spans[0].end as usize, source.len());
        assert_eq!(
            (json_warning.spans[0].line, json_warning.spans[0].column),
            offset_to_line_col(source, 10)
        );
    }

    #[test]
    fn test_json_to_string() {
        let source = "fn main() -> i32 { 1 + true }";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(23, 27),
        );

        let json_diag = formatter.format_error(&error);
        let json_str = json_diag.to_json();

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["severity"], "error");
        assert_eq!(parsed["code"], "E0206");
        assert!(parsed["spans"].is_array());
        assert_eq!(parsed["spans"][0]["primary"], true);
    }

    #[test]
    fn test_json_format_errors_array() {
        let source = "fn main() -> i32 {\n    1 + true\n}";
        let source_info = SourceInfo::new(source, "test.rue");
        let formatter = JsonDiagnosticFormatter::new(&source_info);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::new(27, 31),
        ));

        let json_str = formatter.format_errors(&errors);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    // ========================================================================
    // MultiFileFormatter tests
    // ========================================================================

    #[test]
    fn test_multi_file_formatter_single_file() {
        let source = "fn main() -> i32 { 1 + true }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 23, 27),
        );

        let output = formatter.format_error(&error);
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
        assert!(output.contains("test.rue"));
    }

    #[test]
    fn test_multi_file_formatter_multiple_files() {
        let source1 = "fn main() -> i32 { helper() }";
        let source2 = "fn helper() -> bool { true }";
        let sources = vec![
            (FileId::new(1), SourceInfo::new(source1, "main.rue")),
            (FileId::new(2), SourceInfo::new(source2, "helper.rue")),
        ];
        let formatter = MultiFileFormatter::new(sources);

        // Error in file 1 with label pointing to file 2
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 19, 27),
        )
        .with_label(
            "function returns bool here",
            Span::with_file(FileId::new(2), 15, 19),
        );

        let output = formatter.format_error(&error);
        // Should contain both file names
        assert!(output.contains("main.rue"));
        assert!(output.contains("helper.rue"));
        assert!(output.contains("function returns bool here"));
    }

    #[test]
    fn test_multi_file_formatter_without_span() {
        let source = "fn foo() -> i32 { 42 }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        let output = formatter.format_error(&error);
        assert!(output.contains("[E0200]"));
        assert!(output.contains("no main function"));
    }

    #[test]
    fn test_multi_file_formatter_without_span_promotes_label() {
        let source1 = "fn main() -> i32 { helper() }";
        let source2 = "fn helper() -> bool { true }";
        let sources = vec![
            (FileId::new(1), SourceInfo::new(source1, "main.rue")),
            (FileId::new(2), SourceInfo::new(source2, "helper.rue")),
        ];
        let formatter = MultiFileFormatter::new(sources);

        let error = CompileError::without_span(ErrorKind::NoMainFunction).with_label(
            "candidate function is here",
            Span::with_file(FileId::new(2), 3, 9),
        );

        let output = formatter.format_error(&error);
        assert!(output.contains("[E0200]"));
        assert!(output.contains("no main function"));
        assert!(output.contains("helper.rue"));
        assert!(output.contains("candidate function is here"));
    }

    #[test]
    fn test_multi_file_formatter_format_errors() {
        let source = "fn main() -> i32 { 1 + true }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 23, 27),
        ));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let output = formatter.format_errors(&errors);
        assert!(output.contains("type mismatch"));
        assert!(output.contains("no main function"));
        assert!(output.contains("aborting due to 2 previous errors"));
    }

    #[test]
    fn test_multi_file_formatter_format_warnings() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        let warnings = vec![CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::with_file(FileId::new(1), 23, 24),
        )];

        let output = formatter.format_warnings(&warnings);
        assert!(output.contains("unused variable"));
        assert!(output.contains("'x'"));
    }

    #[test]
    fn test_multi_file_formatter_color_choice() {
        let source = "fn main() -> i32 { 1 + true }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::with_color_choice(sources, ColorChoice::Never);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 23, 27),
        );

        let output = formatter.format_error(&error);
        // Output should not contain ANSI escape codes
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn test_multi_file_formatter_invalid_span() {
        let source = "fn main() -> i32 { 42 }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        // Span that extends beyond source length
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 20, 1000),
        );

        // Should not panic, should clamp to valid range
        let output = formatter.format_error(&error);
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
    }

    #[test]
    fn test_multi_file_formatter_reversed_span() {
        let source = "fn main() -> i32 { 42 }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::new(sources);

        // Span with start > end
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 20, 10),
        );

        // Should not panic
        let output = formatter.format_error(&error);
        assert!(output.contains("[E0206]"));
        assert!(output.contains("type mismatch"));
    }

    // ========================================================================
    // Control-byte sanitization for terminal safety (RUE-548)
    // ========================================================================

    /// Count C0 control bytes (0x00–0x1F) other than tab/newline, plus DEL —
    /// the bytes a terminal would interpret as commands. Color is disabled in
    /// these tests so the renderer's own ANSI escapes never appear.
    fn unsafe_control_bytes(s: &str) -> usize {
        s.bytes()
            .filter(|&b| (b < 0x20 && b != b'\t' && b != b'\n') || b == 0x7f)
            .count()
    }

    #[test]
    fn test_control_glyph_maps_to_pictures() {
        assert_eq!(control_glyph('\u{1b}'), '␛'); // ESC  -> U+241B
        assert_eq!(control_glyph('\u{0}'), '␀'); //  NUL  -> U+2400
        assert_eq!(control_glyph('\u{7}'), '␇'); //  BEL  -> U+2407
        assert_eq!(control_glyph('\u{7f}'), '␡'); // DEL  -> U+2421
        assert_eq!(control_glyph('\u{85}'), '\u{fffd}'); // C1 -> replacement
    }

    #[test]
    fn test_sanitize_message_neutralizes_controls() {
        let s = "cannot find module '\u{1b}[2J\u{1b}[Hevil'";
        let out = sanitize_message(s);
        assert_eq!(unsafe_control_bytes(&out), 0, "escape survived: {out:?}");
        assert!(out.contains('␛'), "expected ESC picture: {out:?}");
        // NUL is neutralized too.
        assert_eq!(unsafe_control_bytes(&sanitize_message("a\u{0}b")), 0);
        // A safe string is returned borrowed, unchanged.
        assert!(matches!(
            sanitize_message("plain 'text'"),
            Cow::Borrowed("plain 'text'")
        ));
    }

    #[test]
    fn test_source_excerpt_strips_control_bytes() {
        // ESC bytes hidden in a trailing comment, exactly the reported repro.
        let source = "fn main() -> i32 {\n    nope // \u{1b}[2J\u{1b}[H injected\n}";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::with_color_choice(sources, ColorChoice::Never);
        // `nope` at byte offset 23..27.
        let error = CompileError::new(
            ErrorKind::UndefinedVariable("nope".to_string()),
            Span::with_file(FileId::new(1), 23, 27),
        );
        let output = formatter.format_error(&error);
        assert_eq!(
            unsafe_control_bytes(&output),
            0,
            "raw control bytes reached the terminal: {output:?}"
        );
        assert!(
            output.contains('␛'),
            "ESC not rendered as a picture: {output:?}"
        );
    }

    #[test]
    fn test_source_excerpt_caret_alignment_preserved() {
        // A control byte inside a string literal PRECEDES the erroring token on
        // the same line. The 1-byte ESC renders as a 1-column glyph, so the
        // caret must still underline `nope`, not drift. (spec: escaped source
        // still points to the correct span.)
        let source = "fn main() -> i32 {\n    let s = \"\u{1b}ab\"; nope\n}";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileFormatter::with_color_choice(sources, ColorChoice::Never);
        // `nope` at byte offset 38..42 (after the ESC-bearing string literal).
        let error = CompileError::new(
            ErrorKind::UndefinedVariable("nope".to_string()),
            Span::with_file(FileId::new(1), 38, 42),
        );
        let output = formatter.format_error(&error);
        assert_eq!(unsafe_control_bytes(&output), 0);
        // The caret line underlines exactly the 4 columns of `nope`, and its
        // leading indent matches `nope`'s column on the rendered source line
        // (the ESC glyph occupies one column, like the original byte).
        let lines: Vec<&str> = output.lines().collect();
        // Match the source EXCERPT line by content unique to it (`let s`), not
        // the error title, which also contains `nope`.
        let src_line = lines
            .iter()
            .find(|l| l.contains("let s"))
            .expect("source line");
        let caret_line = lines.iter().find(|l| l.contains('^')).expect("caret line");
        // Strip the `N | ` / `  | ` gutter, then compare DISPLAY columns (char
        // counts, since the only wide-in-bytes char here is the 1-column ESC
        // glyph) — byte offsets would differ by the glyph's extra bytes.
        let after_gutter =
            |l: &str| -> String { l.split_once("| ").map_or(l, |(_, rest)| rest).to_string() };
        let src_body = after_gutter(src_line);
        let caret_body = after_gutter(caret_line);
        let nope_col = src_body[..src_body.find("nope").unwrap()].chars().count();
        let caret_col = caret_body[..caret_body.find('^').unwrap()].chars().count();
        assert_eq!(
            caret_col, nope_col,
            "caret drifted: src {src_line:?} caret {caret_line:?}"
        );
        assert_eq!(
            caret_line.matches('^').count(),
            4,
            "caret width != len(nope): {caret_line:?}"
        );
    }

    // ========================================================================
    // MultiFileJsonFormatter tests
    // ========================================================================

    #[test]
    fn test_multi_file_json_formatter_single_file() {
        let source = "fn main() -> i32 { 1 + true }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileJsonFormatter::new(sources);

        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 23, 27),
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.severity, "error");
        assert_eq!(json_diag.code, "E0206");
        assert_eq!(json_diag.spans.len(), 1);
        assert_eq!(json_diag.spans[0].file, "test.rue");
        assert!(json_diag.spans[0].primary);
    }

    #[test]
    fn test_multi_file_json_formatter_multiple_files() {
        let source1 = "fn main() -> i32 { helper() }";
        let source2 = "fn helper() -> bool { true }";
        let sources = vec![
            (FileId::new(1), SourceInfo::new(source1, "main.rue")),
            (FileId::new(2), SourceInfo::new(source2, "helper.rue")),
        ];
        let formatter = MultiFileJsonFormatter::new(sources);

        // Error in file 1 with label pointing to file 2
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 19, 27),
        )
        .with_label(
            "function returns bool here",
            Span::with_file(FileId::new(2), 15, 19),
        );

        let json_diag = formatter.format_error(&error);
        assert_eq!(json_diag.spans.len(), 2);

        // Primary span should be from main.rue
        let primary = json_diag.spans.iter().find(|s| s.primary).unwrap();
        assert_eq!(primary.file, "main.rue");

        // Secondary span should be from helper.rue
        let secondary = json_diag.spans.iter().find(|s| !s.primary).unwrap();
        assert_eq!(secondary.file, "helper.rue");
        assert_eq!(
            secondary.label,
            Some("function returns bool here".to_string())
        );
    }

    #[test]
    fn test_multi_file_json_unresolved_primary_promotes_resolvable_labels() {
        let sources = vec![
            (FileId::new(1), SourceInfo::new("fn main() {}", "main.rue")),
            (
                FileId::new(2),
                SourceInfo::new("fn helper() {}", "helper.rue"),
            ),
        ];
        let formatter = MultiFileJsonFormatter::new(sources);
        let unknown = FileId::new(99);

        let error = CompileError::new(ErrorKind::NoMainFunction, Span::with_file(unknown, 0, 5))
            .with_label("first", Span::with_file(FileId::new(2), 3, 9))
            .with_label("second", Span::with_file(FileId::new(1), 3, 7));
        let json_error = formatter.format_error(&error);
        assert_eq!(json_error.spans.len(), 2);
        assert_eq!(json_error.spans[0].file, "helper.rue");
        assert!(json_error.spans[0].primary);
        assert_eq!(json_error.spans[0].label, None);
        assert_eq!(json_error.spans[1].file, "main.rue");
        assert!(!json_error.spans[1].primary);
        assert_eq!(json_error.spans[1].label.as_deref(), Some("second"));

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".into()),
            Span::with_file(unknown, 0, 5),
        )
        .with_label("first warning", Span::with_file(FileId::new(2), 3, 9));
        let json_warning = formatter.format_warning(&warning);
        assert_eq!(json_warning.spans.len(), 1);
        assert_eq!(json_warning.spans[0].file, "helper.rue");
        assert!(json_warning.spans[0].primary);
        assert_eq!(json_warning.spans[0].label, None);

        let unresolved =
            CompileError::new(ErrorKind::NoMainFunction, Span::with_file(unknown, 0, 5));
        assert!(formatter.format_error(&unresolved).spans.is_empty());
    }

    #[test]
    fn test_multi_file_json_spans_clamp_ranges_for_errors_and_warnings() {
        let source = "fn main() {}";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "main.rue"))];
        let formatter = MultiFileJsonFormatter::new(sources);

        let error = CompileError::new(
            ErrorKind::NoMainFunction,
            Span::with_file(FileId::new(1), 50, 1),
        );
        let json_error = formatter.format_error(&error);
        assert_eq!(json_error.spans[0].start as usize, source.len());
        assert_eq!(json_error.spans[0].end as usize, source.len());

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".into()),
            Span::with_file(FileId::new(1), 2, 50),
        );
        let json_warning = formatter.format_warning(&warning);
        assert_eq!(json_warning.spans[0].start, 2);
        assert_eq!(json_warning.spans[0].end as usize, source.len());
    }

    #[test]
    fn test_multi_file_json_formatter_format_errors() {
        let source = "fn main() -> i32 { 1 + true }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileJsonFormatter::new(sources);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            Span::with_file(FileId::new(1), 23, 27),
        ));

        let json_str = formatter.format_errors(&errors);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_multi_file_json_formatter_format_warning() {
        let source = "fn main() -> i32 { let x = 42; 0 }";
        let sources = vec![(FileId::new(1), SourceInfo::new(source, "test.rue"))];
        let formatter = MultiFileJsonFormatter::new(sources);

        let warning = CompileWarning::new(
            WarningKind::UnusedVariable("x".to_string()),
            Span::with_file(FileId::new(1), 23, 24),
        );

        let json_diag = formatter.format_warning(&warning);
        assert_eq!(json_diag.severity, "warning");
        assert!(json_diag.message.contains("unused variable"));
        assert_eq!(json_diag.spans.len(), 1);
        assert_eq!(json_diag.spans[0].file, "test.rue");
    }
}
