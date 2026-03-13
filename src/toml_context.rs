use taplo::dom::{
    node::{Array, DomNode, Str, Table},
    Node,
};
use tower_lsp::lsp_types::{Position, Range};

pub use crate::toml_context_fallback::{
    CrateNameContext, FeatureCompletionContext, VersionContext,
};

pub fn is_in_dependencies_section(text: &str, line: u32) -> bool {
    crate::toml_context_fallback::is_in_dependencies_section(text, line)
}

pub fn is_typing_crate_name(line_text: &str, cursor_char: u32) -> bool {
    crate::toml_context_fallback::is_typing_crate_name(line_text, cursor_char)
}

pub fn get_crate_name_context(line_text: &str, cursor_char: u32) -> CrateNameContext {
    crate::toml_context_fallback::get_crate_name_context(line_text, cursor_char)
}

pub fn get_version_context(text: &str, line: u32, character: u32) -> Option<VersionContext> {
    taplo_version_context(text, line, character)
        .or_else(|| crate::toml_context_fallback::get_version_context(text, line, character))
}

pub fn get_feature_completion_context(
    text: &str,
    line: u32,
    character: u32,
) -> Option<FeatureCompletionContext> {
    taplo_feature_completion_context(text, line, character).or_else(|| {
        crate::toml_context_fallback::get_feature_completion_context(text, line, character)
    })
}

struct FeatureArrayContext {
    feature_prefix: String,
    range: Range,
    selected_features: Vec<String>,
}

fn taplo_version_context(text: &str, line: u32, character: u32) -> Option<VersionContext> {
    let line_starts = line_starts(text);
    let cursor_offset = offset_of(&line_starts, line, character);
    let dom = taplo::parser::parse(text).into_dom();
    let root = dom.as_table()?;

    find_version_context_in_table(root, &mut Vec::new(), text, cursor_offset, &line_starts)
}

fn taplo_feature_completion_context(
    text: &str,
    line: u32,
    character: u32,
) -> Option<FeatureCompletionContext> {
    let line_starts = line_starts(text);
    let cursor_offset = offset_of(&line_starts, line, character);
    let dom = taplo::parser::parse(text).into_dom();
    let root = dom.as_table()?;

    find_feature_context_in_table(root, &mut Vec::new(), text, cursor_offset, &line_starts)
}

fn find_version_context_in_table(
    table: &Table,
    path: &mut Vec<String>,
    text: &str,
    cursor_offset: usize,
    line_starts: &[usize],
) -> Option<VersionContext> {
    let entries = table_entries(table);

    if is_dependency_section_path(path) {
        for (dep_key, node) in &entries {
            if let Some(ctx) =
                version_context_for_dependency(dep_key, node, text, cursor_offset, line_starts)
            {
                return Some(ctx);
            }
        }
    }

    for (key, node) in entries {
        if let Node::Table(child) = node {
            path.push(key);
            let found =
                find_version_context_in_table(&child, path, text, cursor_offset, line_starts);
            path.pop();
            if found.is_some() {
                return found;
            }
        }
    }

    None
}

fn find_feature_context_in_table(
    table: &Table,
    path: &mut Vec<String>,
    text: &str,
    cursor_offset: usize,
    line_starts: &[usize],
) -> Option<FeatureCompletionContext> {
    let entries = table_entries(table);

    if is_dependency_section_path(path) {
        for (dep_key, node) in &entries {
            if let Some(ctx) =
                feature_context_for_dependency(dep_key, node, text, cursor_offset, line_starts)
            {
                return Some(ctx);
            }
        }
    }

    for (key, node) in entries {
        if let Node::Table(child) = node {
            path.push(key);
            let found =
                find_feature_context_in_table(&child, path, text, cursor_offset, line_starts);
            path.pop();
            if found.is_some() {
                return found;
            }
        }
    }

    None
}

fn version_context_for_dependency(
    dep_key: &str,
    node: &Node,
    text: &str,
    cursor_offset: usize,
    line_starts: &[usize],
) -> Option<VersionContext> {
    let value = match node {
        Node::Str(value) => value,
        _ => return None,
    };
    let (content_start, content_end) = string_content_bounds(text, value)?;
    if !offset_in_string(cursor_offset, content_start, content_end) {
        return None;
    }

    Some(VersionContext {
        crate_name: dep_key.to_string(),
        version_prefix: text[content_start..cursor_offset].to_string(),
        range: byte_range_to_lsp_range(line_starts, content_start, content_end),
    })
}

fn feature_context_for_dependency(
    dep_key: &str,
    node: &Node,
    text: &str,
    cursor_offset: usize,
    line_starts: &[usize],
) -> Option<FeatureCompletionContext> {
    let table = match node {
        Node::Table(table) => table,
        _ => return None,
    };

    let entries = table_entries(table);
    let mut package_name = None;
    let mut version_requirement = None;
    let mut feature_array = None;

    for (key, node) in entries {
        match key.as_str() {
            "package" => {
                package_name = node_string_value(&node);
            }
            "version" => {
                version_requirement = node_string_value(&node);
            }
            "features" => {
                if let Node::Array(array) = node {
                    feature_array =
                        parse_feature_array_context(&array, text, cursor_offset, line_starts);
                }
            }
            _ => {}
        }
    }

    let feature_array = feature_array?;
    let version_requirement = version_requirement.filter(|v| !v.trim().is_empty())?;

    Some(FeatureCompletionContext {
        crate_name: package_name.unwrap_or_else(|| dep_key.to_string()),
        version_requirement,
        feature_prefix: feature_array.feature_prefix,
        range: feature_array.range,
        selected_features: feature_array.selected_features,
    })
}

fn parse_feature_array_context(
    array: &Array,
    text: &str,
    cursor_offset: usize,
    line_starts: &[usize],
) -> Option<FeatureArrayContext> {
    let items: Vec<Node> = array.items().read().iter().cloned().collect();
    let mut selected_features = Vec::new();
    let mut current = None;

    for item in items {
        let value = match item {
            Node::Str(value) => value,
            _ => continue,
        };
        let (content_start, content_end) = match string_content_bounds(text, &value) {
            Some(bounds) => bounds,
            None => continue,
        };

        if offset_in_string(cursor_offset, content_start, content_end) {
            current = Some(FeatureArrayContext {
                feature_prefix: text[content_start..cursor_offset].to_string(),
                range: byte_range_to_lsp_range(line_starts, content_start, content_end),
                selected_features,
            });
            selected_features = Vec::new();
        } else {
            let feature_name = value.value().to_string();
            if !feature_name.is_empty() {
                selected_features.push(feature_name);
            }
        }
    }

    current.map(|mut current| {
        current.selected_features.extend(selected_features);
        current
    })
}

fn node_string_value(node: &Node) -> Option<String> {
    match node {
        Node::Str(value) => Some(value.value().to_string()),
        _ => None,
    }
}

fn table_entries(table: &Table) -> Vec<(String, Node)> {
    table
        .entries()
        .read()
        .iter()
        .map(|(key, node)| (key.value().to_string(), node.clone()))
        .collect()
}

fn is_dependency_section_path(path: &[String]) -> bool {
    let path = path.join(".").to_lowercase();
    path == "dependencies"
        || path == "dev-dependencies"
        || path == "build-dependencies"
        || path == "workspace.dependencies"
        || path.ends_with(".dependencies")
        || path.ends_with(".dev-dependencies")
        || path.ends_with(".build-dependencies")
}

fn string_content_bounds(text: &str, value: &Str) -> Option<(usize, usize)> {
    let (start, end) = syntax_bounds(value)?;
    let raw = text.get(start..end)?;

    let (delimiter, open_len) = if raw.starts_with("\"\"\"") || raw.starts_with("'''") {
        (&raw[..3], 3usize)
    } else if raw.starts_with('"') || raw.starts_with('\'') {
        (&raw[..1], 1usize)
    } else {
        return None;
    };

    let close_len = if raw.len() >= open_len && raw.ends_with(delimiter) {
        open_len
    } else {
        0
    };

    let content_start = start + open_len;
    let content_end = end.saturating_sub(close_len);
    if content_end < content_start {
        return None;
    }

    Some((content_start, content_end))
}

fn syntax_bounds(node: &impl DomNode) -> Option<(usize, usize)> {
    node.syntax().map(|syntax| {
        let range = syntax.text_range();
        (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        )
    })
}

fn offset_in_string(offset: usize, start: usize, end: usize) -> bool {
    start <= offset && offset <= end
}

fn byte_range_to_lsp_range(line_starts: &[usize], start: usize, end: usize) -> Range {
    Range::new(position_of(line_starts, start), position_of(line_starts, end))
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn offset_of(line_starts: &[usize], line: u32, character: u32) -> usize {
    line_starts.get(line as usize).copied().unwrap_or(0) + character as usize
}

fn position_of(line_starts: &[usize], offset: usize) -> Position {
    let line = line_starts.partition_point(|&start| start <= offset).saturating_sub(1);
    let character = offset.saturating_sub(line_starts[line]);
    Position::new(line as u32, character as u32)
}

#[cfg(test)]
mod tests {
    use super::{get_feature_completion_context, get_version_context};

    #[test]
    fn finds_shorthand_dependency_version_context() {
        let text = "[dependencies]\nserde = \"1.0\"\n";
        let ctx = get_version_context(text, 1, 11).expect("version context");
        assert_eq!(ctx.crate_name, "serde");
        assert_eq!(ctx.version_prefix, "1.");
    }

    #[test]
    fn finds_inline_dependency_feature_context() {
        let text = "[dependencies]\ntokio = { version = \"1\", features = [\"rt-\", \"macros\"] }\n";
        let ctx = get_feature_completion_context(text, 1, 41).expect("feature context");
        assert_eq!(ctx.crate_name, "tokio");
        assert_eq!(ctx.version_requirement, "1");
        assert_eq!(ctx.feature_prefix, "rt-");
        assert_eq!(ctx.selected_features, vec!["macros"]);
    }
}
