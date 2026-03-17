/// Port of complete-crate/src/tomlParser.ts
///
/// Analyses a Cargo.toml document at a given cursor position and returns the
/// type of completion that should be offered.
use tower_lsp::lsp_types::{Position, Range};

// ── public output types ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CrateNameContext {
    pub prefix: String,
    pub start_character: u32,
    pub end_character: u32,
}

#[derive(Debug)]
pub struct FeatureCompletionContext {
    pub crate_name: String,
    pub version_requirement: String,
    pub feature_prefix: String,
    pub range: Range,
    pub selected_features: Vec<String>,
}

#[derive(Debug)]
pub struct VersionContext {
    pub crate_name: String,
    pub version_prefix: String,
    pub range: Range,
}

#[derive(Debug)]
pub enum CompletionContext {
    CrateName(CrateNameContext),
    Version(VersionContext),
    Feature(FeatureCompletionContext),
}

// ── byte-offset helpers ────────────────────────────────────────────────────

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

fn offset_of(ls: &[usize], line: u32, character: u32) -> usize {
    ls.get(line as usize).copied().unwrap_or(0) + character as usize
}

fn position_of(ls: &[usize], offset: usize) -> Position {
    let line = ls.partition_point(|&s| s <= offset).saturating_sub(1);
    let character = offset.saturating_sub(ls[line]);
    Position::new(line as u32, character as u32)
}

// ── character classification ───────────────────────────────────────────────

fn is_crate_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_str_delim(b: u8) -> bool {
    b == b'"' || b == b'\''
}

// ── comment stripping ──────────────────────────────────────────────────────

/// Returns a sub-slice of `line` with everything from the first unquoted `#`
/// onwards removed.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut delim = b'"';
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if delim == b'"' && b == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if b == delim && !escaped {
                in_str = false;
            }
            escaped = false;
            continue;
        }
        if is_str_delim(b) {
            in_str = true;
            delim = b;
            escaped = false;
            continue;
        }
        if b == b'#' {
            return &line[..i];
        }
    }
    line
}

// ── public API: section detection ─────────────────────────────────────────

/// Scans upward from `line` to find the nearest `[section]` header.
/// Returns `true` if it belongs to a dependencies section.
fn is_in_dependencies_section(text: &str, line: u32) -> bool {
    let all_lines: Vec<&str> = text.lines().take(line as usize + 1).collect();
    let start = all_lines.len().saturating_sub(1);

    for i in (0..=start).rev() {
        let trimmed = strip_comment(all_lines[i]).trim();
        // A section header looks like [foo] or [[foo]] after stripping comments.
        // We only care about single-bracket headers.
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") && trimmed.ends_with(']') {
            let inner = trimmed[1..trimmed.len() - 1].trim().to_lowercase();
            return inner == "dependencies"
                || inner == "dev-dependencies"
                || inner == "build-dependencies"
                || inner == "workspace.dependencies"
                || inner.ends_with(".dependencies")
                || inner.ends_with(".dev-dependencies")
                || inner.ends_with(".build-dependencies");
        }
    }
    false
}

// ── public API: crate-name context ────────────────────────────────────────

/// Returns `true` when the cursor is on the key side of an assignment
/// (i.e. no `=` has appeared yet on this line before the cursor).
fn is_typing_crate_name(line_text: &str, cursor_char: u32) -> bool {
    let cursor = (cursor_char as usize).min(line_text.len());
    let before = &line_text[..cursor];
    if before.contains('=') {
        return false;
    }
    before.trim().bytes().all(is_crate_char)
}

/// Returns the crate-name prefix/extent at the cursor.
fn get_crate_name_context(line_text: &str, cursor_char: u32) -> CrateNameContext {
    let bytes = line_text.as_bytes();
    let cursor = (cursor_char as usize).min(bytes.len());

    let mut start = cursor;
    while start > 0 && is_crate_char(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < bytes.len() && is_crate_char(bytes[end]) {
        end += 1;
    }

    CrateNameContext {
        prefix: line_text[start..cursor].to_string(),
        start_character: start as u32,
        end_character: end as u32,
    }
}

// ── public API: version-string context ────────────────────────────────────

/// Detects `crate_name = "version_here"` where cursor is inside the string.
/// Returns `None` for keys like `version`, `path`, `git` (handled elsewhere).
fn get_simple_version_context(text: &str, line: u32, character: u32) -> Option<VersionContext> {
    let all_lines: Vec<&str> = text.lines().collect();
    let line_text = all_lines.get(line as usize)?;
    let stripped = strip_comment(line_text);
    let bytes = stripped.as_bytes();
    let cursor = (character as usize).min(bytes.len());

    // Find `=` on this line
    let eq_pos = bytes.iter().position(|&b| b == b'=')?;
    if cursor <= eq_pos {
        return None;
    }

    // Key before `=` must be a valid bare crate name
    let key = stripped[..eq_pos].trim();
    if key.is_empty() || !key.bytes().all(is_crate_char) {
        return None;
    }
    // Skip TOML meta-keys that aren't crate names
    if matches!(
        key,
        "version" | "path" | "git" | "branch" | "tag" | "rev" | "edition"
    ) {
        return None;
    }

    // After `=`, skip whitespace, expect an opening quote
    let mut q = eq_pos + 1;
    while q < bytes.len() && bytes[q] == b' ' {
        q += 1;
    }
    if q >= bytes.len() || !is_str_delim(bytes[q]) {
        return None;
    }
    let delim = bytes[q];
    let content_start = q + 1;

    // Find closing quote
    let mut q_end = content_start;
    while q_end < bytes.len() && bytes[q_end] != delim {
        q_end += 1;
    }
    let content_end = q_end;

    // Cursor must be inside the string content
    if cursor < content_start || cursor > content_end {
        return None;
    }

    let ls = line_starts(text);
    let abs_content_start = offset_of(&ls, line, content_start as u32);
    let abs_content_end = offset_of(&ls, line, content_end as u32);

    Some(VersionContext {
        crate_name: key.to_string(),
        version_prefix: stripped[content_start..cursor].to_string(),
        range: Range::new(
            position_of(&ls, abs_content_start),
            position_of(&ls, abs_content_end),
        ),
    })
}

// ── feature-completion context (port of TypeScript) ───────────────────────

fn skip_trivia(text: &[u8], mut cursor: usize, end: usize) -> usize {
    while cursor < end {
        let b = text[cursor];
        if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' || b == b',' {
            cursor += 1;
            continue;
        }
        if b == b'#' {
            cursor += 1;
            while cursor < end && text[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        break;
    }
    cursor
}

fn parse_bare_key(text: &[u8], offset: usize, end: usize) -> Option<(&str, usize)> {
    let start = offset;
    let mut cursor = offset;
    while cursor < end && is_crate_char(text[cursor]) {
        cursor += 1;
    }
    if cursor == start {
        return None;
    }
    Some((std::str::from_utf8(&text[start..cursor]).ok()?, cursor))
}

struct ParsedStr {
    value: String,
    content_start: usize,
    content_end: usize,
    next_offset: usize,
}

fn parse_toml_str(text: &[u8], offset: usize, end: usize) -> Option<ParsedStr> {
    if offset >= end || !is_str_delim(text[offset]) {
        return None;
    }
    let delim = text[offset];
    let content_start = offset + 1;
    let mut cursor = content_start;
    let mut escaped = false;
    let mut value = String::new();

    while cursor < end {
        let b = text[cursor];
        if delim == b'"' && b == b'\\' && !escaped {
            escaped = true;
            cursor += 1;
            if cursor < end {
                value.push(text[cursor] as char);
                cursor += 1;
            }
            continue;
        }
        if b == delim && !escaped {
            return Some(ParsedStr {
                value,
                content_start,
                content_end: cursor,
                next_offset: cursor + 1,
            });
        }
        value.push(b as char);
        escaped = false;
        cursor += 1;
    }
    // Unclosed string — cursor is still a valid completion position
    Some(ParsedStr {
        value,
        content_start,
        content_end: end,
        next_offset: end,
    })
}

fn find_matching(text: &[u8], open_at: usize, open: u8, close: u8, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = open_at;
    let mut in_str = false;
    let mut delim = b'"';
    let mut escaped = false;
    let mut in_comment = false;

    while cursor < end {
        let b = text[cursor];
        if in_comment {
            if b == b'\n' {
                in_comment = false;
            }
            cursor += 1;
            continue;
        }
        if in_str {
            if delim == b'"' && b == b'\\' && !escaped {
                escaped = true;
                cursor += 1;
                continue;
            }
            if b == delim && !escaped {
                in_str = false;
            }
            escaped = false;
            cursor += 1;
            continue;
        }
        if is_str_delim(b) {
            in_str = true;
            delim = b;
            escaped = false;
            cursor += 1;
            continue;
        }
        if b == b'#' {
            in_comment = true;
            cursor += 1;
            continue;
        }
        if b == open {
            depth += 1;
            cursor += 1;
            continue;
        }
        if b == close {
            if depth == 0 {
                return None;
            } // shouldn't happen
            depth -= 1;
            if depth == 0 {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn skip_toml_value(text: &[u8], offset: usize, end: usize) -> usize {
    if offset >= end {
        return end;
    }
    let b = text[offset];
    if is_str_delim(b) {
        return parse_toml_str(text, offset, end)
            .map(|s| s.next_offset)
            .unwrap_or(end);
    }
    if b == b'[' {
        return find_matching(text, offset, b'[', b']', end)
            .map(|i| i + 1)
            .unwrap_or(end);
    }
    if b == b'{' {
        return find_matching(text, offset, b'{', b'}', end)
            .map(|i| i + 1)
            .unwrap_or(end);
    }
    let mut cursor = offset;
    while cursor < end {
        let cur = text[cursor];
        if cur == b',' || cur == b'}' {
            return cursor;
        }
        if cur == b'#' {
            while cursor < end && text[cursor] != b'\n' {
                cursor += 1;
            }
            return cursor;
        }
        cursor += 1;
    }
    end
}

struct FeatureArray {
    selected_features: Vec<String>,
    feature_prefix: String,
    content_start: usize,
    content_end: usize,
    next_offset: usize,
}

fn parse_feature_array(
    text: &[u8],
    offset: usize,
    end: usize,
    cursor_offset: usize,
) -> Option<FeatureArray> {
    if offset >= end || text[offset] != b'[' {
        return None;
    }
    let close = find_matching(text, offset, b'[', b']', end)?;

    let mut selected = Vec::new();
    let mut prefix: Option<String> = None;
    let mut cs = 0usize;
    let mut ce = 0usize;
    let mut cursor = offset + 1;

    while cursor < close {
        cursor = skip_trivia(text, cursor, close);
        if cursor >= close {
            break;
        }
        if !is_str_delim(text[cursor]) {
            cursor += 1;
            continue;
        }

        let parsed = parse_toml_str(text, cursor, close)?;
        let inside = cursor_offset >= parsed.content_start && cursor_offset <= parsed.content_end;

        if inside {
            prefix = Some(
                std::str::from_utf8(&text[parsed.content_start..cursor_offset])
                    .unwrap_or("")
                    .to_string(),
            );
            cs = parsed.content_start;
            ce = parsed.content_end;
        } else if !parsed.value.is_empty() {
            selected.push(parsed.value);
        }
        cursor = parsed.next_offset;
    }

    prefix.map(|p| FeatureArray {
        selected_features: selected,
        feature_prefix: p,
        content_start: cs,
        content_end: ce,
        next_offset: close + 1,
    })
}

struct InlineTable {
    package_name: Option<String>,
    version: Option<String>,
    /// Range of the version string content (inside quotes), if cursor is inside it.
    version_cursor: Option<(String, usize, usize)>, // (prefix, content_start, content_end)
    feature_array: Option<FeatureArray>,
}

fn parse_inline_table(text: &[u8], open: usize, close: usize, cursor_offset: usize) -> InlineTable {
    let mut result = InlineTable {
        package_name: None,
        version: None,
        version_cursor: None,
        feature_array: None,
    };
    let mut cursor = open + 1;

    while cursor < close {
        cursor = skip_trivia(text, cursor, close);
        if cursor >= close {
            break;
        }

        let Some((key, next)) = parse_bare_key(text, cursor, close) else {
            cursor += 1;
            continue;
        };
        cursor = skip_trivia(text, next, close);

        if cursor >= close || text[cursor] != b'=' {
            cursor = skip_toml_value(text, cursor, close);
            continue;
        }
        cursor = skip_trivia(text, cursor + 1, close);
        let val_start = cursor;

        if (key == "package" || key == "version")
            && val_start < close
            && is_str_delim(text[val_start])
        {
            if let Some(s) = parse_toml_str(text, val_start, close) {
                if key == "package" {
                    result.package_name = Some(s.value.clone());
                } else {
                    result.version = Some(s.value.clone());
                    // Check if cursor is inside the version string
                    if cursor_offset >= s.content_start && cursor_offset <= s.content_end {
                        let prefix = std::str::from_utf8(&text[s.content_start..cursor_offset])
                            .unwrap_or("")
                            .to_string();
                        result.version_cursor = Some((prefix, s.content_start, s.content_end));
                    }
                }
                cursor = s.next_offset;
                continue;
            }
        }

        if key == "features" && val_start < close && text[val_start] == b'[' {
            result.feature_array = parse_feature_array(text, val_start, close, cursor_offset);
            cursor = result
                .feature_array
                .as_ref()
                .map(|f| f.next_offset)
                .unwrap_or_else(|| skip_toml_value(text, val_start, close));
            continue;
        }

        cursor = skip_toml_value(text, val_start, close);
    }
    result
}

struct InlineDepStart {
    dep_key: String,
    open_brace: usize,
}

fn find_inline_dep_start(text: &str, line: u32, character: u32) -> Option<InlineDepStart> {
    let bytes = text.as_bytes();
    let ls = line_starts(text);
    let cursor_offset = offset_of(&ls, line, character);

    for l in (0..=(line as usize)).rev() {
        let line_start = ls[l];
        let line_end = ls.get(l + 1).copied().unwrap_or(text.len());
        let raw_line = &text[line_start..line_end];
        let stripped = strip_comment(raw_line);
        let trimmed = stripped.trim();

        // If we passed a section header on a previous line, stop searching
        if l < line as usize && trimmed.starts_with('[') && trimmed.ends_with(']') {
            return None;
        }

        // Must have `key = {`
        let eq_pos = match stripped.find('=') {
            Some(p) => p,
            None => continue,
        };
        let before_eq = stripped[..eq_pos].trim();
        if before_eq.is_empty() || !before_eq.bytes().all(is_crate_char) {
            continue;
        }

        let after_eq = &stripped[eq_pos + 1..];
        let brace_rel = match after_eq.find('{') {
            Some(p) => p,
            None => continue,
        };
        let brace_col = eq_pos + 1 + brace_rel;
        let brace_offset = line_start + brace_col;

        if brace_offset >= bytes.len() {
            continue;
        }

        let close = match find_matching(bytes, brace_offset, b'{', b'}', bytes.len()) {
            Some(c) => c,
            None => continue,
        };

        if cursor_offset > brace_offset && cursor_offset <= close {
            return Some(InlineDepStart {
                dep_key: before_eq.to_string(),
                open_brace: brace_offset,
            });
        }
    }
    None
}

struct ParsedInlineDep {
    dep_key: String,
    table: InlineTable,
    ls: Vec<usize>,
}

fn parse_inline_dep(text: &str, line: u32, character: u32) -> Option<ParsedInlineDep> {
    let dep_start = find_inline_dep_start(text, line, character)?;
    let bytes = text.as_bytes();
    let ls = line_starts(text);
    let cursor_offset = offset_of(&ls, line, character);

    let close = find_matching(bytes, dep_start.open_brace, b'{', b'}', bytes.len())?;
    let table = parse_inline_table(bytes, dep_start.open_brace, close, cursor_offset);

    Some(ParsedInlineDep {
        dep_key: dep_start.dep_key,
        table,
        ls,
    })
}

/// Returns version-completion context if the cursor is inside
/// `version = "..."` within an inline dependency table like `serde = { version = "..." }`.
fn get_inline_version_context(parsed: &ParsedInlineDep) -> Option<VersionContext> {
    let (prefix, content_start, content_end) = parsed.table.version_cursor.as_ref()?;

    Some(VersionContext {
        crate_name: parsed
            .table
            .package_name
            .clone()
            .unwrap_or_else(|| parsed.dep_key.clone()),
        version_prefix: prefix.clone(),
        range: Range::new(
            position_of(&parsed.ls, *content_start),
            position_of(&parsed.ls, *content_end),
        ),
    })
}

/// Main entry point: returns feature-completion context if the cursor is
/// inside a `features = ["..."]` array inside an inline dependency table.
fn get_feature_completion_context(parsed: &ParsedInlineDep) -> Option<FeatureCompletionContext> {
    let fa = parsed.table.feature_array.as_ref()?;
    let version = parsed.table.version.as_ref()?.trim();
    if version.is_empty() {
        return None;
    }

    let range = Range::new(
        position_of(&parsed.ls, fa.content_start),
        position_of(&parsed.ls, fa.content_end),
    );

    Some(FeatureCompletionContext {
        crate_name: parsed
            .table
            .package_name
            .clone()
            .unwrap_or_else(|| parsed.dep_key.clone()),
        version_requirement: version.to_string(),
        feature_prefix: fa.feature_prefix.clone(),
        range,
        selected_features: fa.selected_features.clone(),
    })
}

fn get_crate_name_completion_context(
    line_text: &str,
    cursor_char: u32,
) -> Option<CrateNameContext> {
    if !is_typing_crate_name(line_text, cursor_char) {
        return None;
    }

    let ctx = get_crate_name_context(line_text, cursor_char);
    (ctx.prefix.len() >= 2).then_some(ctx)
}
pub fn get_completion_context(text: &str, line: u32, character: u32) -> Option<CompletionContext> {
    if !is_in_dependencies_section(text, line) {
        return None;
    }

    // 在内联表内：CrateName / SimpleVersion 不可能出现
    if let Some(parsed) = parse_inline_dep(text, line, character) {
        if let Some(ctx) = get_feature_completion_context(&parsed) {
            return Some(CompletionContext::Feature(ctx));
        }
        return get_inline_version_context(&parsed).map(CompletionContext::Version);
    }

    // 不在内联表：Feature / InlineVersion 不可能出现
    let line_text = text.lines().nth(line as usize).unwrap_or("");
    if let Some(ctx) = get_crate_name_completion_context(line_text, character) {
        return Some(CompletionContext::CrateName(ctx));
    }
    get_simple_version_context(text, line, character).map(CompletionContext::Version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_at(text: &str) -> Option<CompletionContext> {
        let marker = "<|>";
        let offset = text.find(marker).expect("cursor marker must exist");
        let prefix = &text[..offset];
        let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32;
        let character = prefix.rsplit('\n').next().unwrap_or(prefix).len() as u32;

        let mut clean = text.to_string();
        clean.replace_range(offset..offset + marker.len(), "");

        get_completion_context(&clean, line, character)
    }

    #[test]
    fn returns_none_outside_dependencies_section() {
        let text = r#"[package]
na<|>me = "demo"
"#;

        assert!(context_at(text).is_none());
    }

    #[test]
    fn returns_none_for_short_crate_name_prefix() {
        let text = r#"[dependencies]
s<|>
"#;

        assert!(context_at(text).is_none());
    }

    #[test]
    fn returns_crate_name_context_for_dependency_key_prefix() {
        let text = r#"[dependencies]
se<|>
"#;

        match context_at(text) {
            Some(CompletionContext::CrateName(ctx)) => {
                assert_eq!(ctx.prefix, "se");
                assert_eq!(ctx.start_character, 0);
                assert_eq!(ctx.end_character, 2);
            }
            other => panic!("expected crate-name context, got {other:?}"),
        }
    }

    #[test]
    fn returns_version_context_for_simple_dependency_string() {
        let text = r#"[dependencies]
serde = "1.<|>"
"#;

        match context_at(text) {
            Some(CompletionContext::Version(ctx)) => {
                assert_eq!(ctx.crate_name, "serde");
                assert_eq!(ctx.version_prefix, "1.");
            }
            other => panic!("expected version context, got {other:?}"),
        }
    }

    #[test]
    fn returns_version_context_for_inline_dependency_version() {
        let text = r#"[dependencies]
serde = { version = "1.<|>" }
"#;

        match context_at(text) {
            Some(CompletionContext::Version(ctx)) => {
                assert_eq!(ctx.crate_name, "serde");
                assert_eq!(ctx.version_prefix, "1.");
            }
            other => panic!("expected inline version context, got {other:?}"),
        }
    }

    #[test]
    fn inline_version_uses_package_name_when_present() {
        let text = r#"[dependencies]
serde_json_alias = { package = "serde_json", version = "1.<|>" }
"#;

        match context_at(text) {
            Some(CompletionContext::Version(ctx)) => {
                assert_eq!(ctx.crate_name, "serde_json");
                assert_eq!(ctx.version_prefix, "1.");
            }
            other => panic!("expected package-aware version context, got {other:?}"),
        }
    }

    #[test]
    fn returns_feature_context_for_inline_feature_array() {
        let text = r#"[dependencies]
serde = {
  version = "1",
  features = [
    "alloc",
    "de<|>"
  ],
}
"#;

        match context_at(text) {
            Some(CompletionContext::Feature(ctx)) => {
                assert_eq!(ctx.crate_name, "serde");
                assert_eq!(ctx.version_requirement, "1");
                assert_eq!(ctx.feature_prefix, "de");
                assert_eq!(ctx.selected_features, vec!["alloc".to_string()]);
            }
            other => panic!("expected feature context, got {other:?}"),
        }
    }

    #[test]
    fn recognizes_workspace_and_dev_dependency_sections() {
        let workspace_text = r#"[workspace.dependencies]
se<|>
"#;
        match context_at(workspace_text) {
            Some(CompletionContext::CrateName(ctx)) => assert_eq!(ctx.prefix, "se"),
            other => panic!("expected workspace crate-name context, got {other:?}"),
        }

        let dev_text = r#"[dev-dependencies]
serde = "1.<|>"
"#;
        match context_at(dev_text) {
            Some(CompletionContext::Version(ctx)) => {
                assert_eq!(ctx.crate_name, "serde");
                assert_eq!(ctx.version_prefix, "1.");
            }
            other => panic!("expected dev-dependency version context, got {other:?}"),
        }
    }

    #[test]
    fn prefers_feature_context_in_multiline_inline_dependency_with_comments() {
        let text = r#"[dependencies]
serde = { # comment
  version = "1",
  features = [
    "alloc", # keep this selected
    "de<|>"
  ],
}
"#;

        match context_at(text) {
            Some(CompletionContext::Feature(ctx)) => {
                assert_eq!(ctx.crate_name, "serde");
                assert_eq!(ctx.feature_prefix, "de");
                assert_eq!(ctx.selected_features, vec!["alloc".to_string()]);
            }
            other => panic!("expected feature context to win, got {other:?}"),
        }
    }
}
