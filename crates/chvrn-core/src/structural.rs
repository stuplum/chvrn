use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::TextSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Python,
    Json,
}

impl Language {
    pub fn for_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "py" => Some(Self::Python),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript | Self::Jsx => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }

    fn highlight_query(self) -> &'static str {
        match self {
            Self::Rust => {
                "(function_item name: (identifier) @function) (integer_literal) @number (string_literal) @string (line_comment) @comment (type_identifier) @type \"fn\" @keyword \"let\" @keyword \"pub\" @keyword \"impl\" @keyword \"struct\" @keyword \"return\" @keyword"
            }
            Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Jsx => {
                "(identifier) @identifier (string) @string (number) @number (comment) @comment \"const\" @keyword \"function\" @keyword \"return\" @keyword"
            }
            Self::Python => {
                "(identifier) @identifier (integer) @number (string) @string (comment) @comment \"def\" @keyword \"return\" @keyword \"class\" @keyword"
            }
            Self::Json => "(string) @string (number) @number",
        }
    }

    fn compiled_query(self) -> Result<&'static Query, StructuralError> {
        static RUST: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(&Language::Rust.grammar(), Language::Rust.highlight_query()).ok()
        });
        static TYPESCRIPT: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(
                &Language::TypeScript.grammar(),
                Language::TypeScript.highlight_query(),
            )
            .ok()
        });
        static TSX: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(&Language::Tsx.grammar(), Language::Tsx.highlight_query()).ok()
        });
        static JAVASCRIPT: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(
                &Language::JavaScript.grammar(),
                Language::JavaScript.highlight_query(),
            )
            .ok()
        });
        static PYTHON: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(
                &Language::Python.grammar(),
                Language::Python.highlight_query(),
            )
            .ok()
        });
        static JSON: LazyLock<Option<Query>> = LazyLock::new(|| {
            Query::new(&Language::Json.grammar(), Language::Json.highlight_query()).ok()
        });
        let slot = match self {
            Self::Rust => &RUST,
            Self::TypeScript => &TYPESCRIPT,
            Self::Tsx => &TSX,
            Self::JavaScript | Self::Jsx => &JAVASCRIPT,
            Self::Python => &PYTHON,
            Self::Json => &JSON,
        };
        slot.as_ref().ok_or(StructuralError::ParseFailure)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralChangeKind {
    Move,
    Reflow,
    Renamed,
    ChangedTokens,
}

pub struct StructuralChange {
    pub kind: StructuralChangeKind,
    pub before: Range<usize>,
    pub after: Range<usize>,
}

pub struct StructuralDiff {
    changes: Vec<StructuralChange>,
}

impl StructuralDiff {
    pub fn changes(&self) -> &[StructuralChange] {
        &self.changes
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StructuralError {
    UnsupportedLanguage,
    ParseFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightKind {
    Keyword,
    Identifier,
    String,
    Number,
    Comment,
    Type,
    Function,
    Punctuation,
}

pub struct HighlightSpan {
    pub bytes: Range<usize>,
    pub kind: HighlightKind,
}

pub struct StructuralAnalysis;

struct Token<'a> {
    kind: &'static str,
    text: &'a str,
    name: bool,
}

struct SyntaxUnit<'a> {
    kind: &'static str,
    name: Option<&'a str>,
    text: &'a str,
    range: Range<usize>,
    tokens: Vec<Token<'a>>,
}

fn parse(language: Language, source: &TextSnapshot) -> Result<Tree, StructuralError> {
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .map_err(|_| StructuralError::ParseFailure)?;
    parser
        .parse(source.text(), None)
        .ok_or(StructuralError::ParseFailure)
}

fn declaration_name(node: Node<'_>) -> Option<Node<'_>> {
    let kind = node.kind();
    if !kind.ends_with("_declaration")
        && !kind.ends_with("_definition")
        && !kind.ends_with("_item")
        && !matches!(kind, "variable_declarator" | "export_statement")
    {
        return None;
    }
    if let Some(name) = node.child_by_field_name("name") {
        return Some(name);
    }
    for index in 0..node.named_child_count() {
        if let Some(found) = declaration_name(node.named_child(index)?) {
            return Some(found);
        }
    }
    None
}

fn collect_tokens<'a>(
    node: Node<'_>,
    source: &'a str,
    name: Option<Range<usize>>,
    output: &mut Vec<Token<'a>>,
) {
    if node.child_count() == 0 {
        let range = node.byte_range();
        if !range.is_empty() {
            output.push(Token {
                kind: node.kind(),
                text: &source[range.clone()],
                name: name.as_ref() == Some(&range),
            });
        }
        return;
    }
    for index in 0..node.child_count() {
        if let Some(child) = node.child(index) {
            collect_tokens(child, source, name.clone(), output);
        }
    }
}

fn units<'a>(source: &'a TextSnapshot, tree: &Tree) -> Vec<SyntaxUnit<'a>> {
    let root = tree.root_node();
    let mut units = Vec::new();
    for index in 0..root.named_child_count() {
        let Some(node) = root.named_child(index) else {
            continue;
        };
        let range = node.byte_range();
        let name_range = declaration_name(node).map(|name| name.byte_range());
        let mut tokens = Vec::new();
        collect_tokens(node, source.text(), name_range.clone(), &mut tokens);
        units.push(SyntaxUnit {
            kind: node.kind(),
            name: name_range.map(|name| &source.text()[name]),
            text: &source.text()[range.clone()],
            range,
            tokens,
        });
    }
    units
}

fn same_tokens(left: &SyntaxUnit<'_>, right: &SyntaxUnit<'_>) -> bool {
    left.tokens.len() == right.tokens.len()
        && left
            .tokens
            .iter()
            .zip(&right.tokens)
            .all(|(a, b)| a.kind == b.kind && a.text == b.text)
}

fn renamed(left: &SyntaxUnit<'_>, right: &SyntaxUnit<'_>) -> bool {
    left.kind == right.kind
        && left.name.is_some()
        && right.name.is_some()
        && left.name != right.name
        && left.tokens.len() == right.tokens.len()
        && left
            .tokens
            .iter()
            .zip(&right.tokens)
            .filter(|(a, b)| a.kind != b.kind || a.text != b.text)
            .count()
            == 1
        && left.tokens.iter().zip(&right.tokens).all(|(a, b)| {
            (a.kind == b.kind && a.text == b.text) || (a.kind == b.kind && a.name && b.name)
        })
}

fn pair_units(before: &[SyntaxUnit<'_>], after: &[SyntaxUnit<'_>]) -> Vec<Option<usize>> {
    let mut paired = vec![None; before.len()];
    let mut used = vec![false; after.len()];
    for (old_index, old) in before.iter().enumerate() {
        if let Some((new_index, _)) = after.iter().enumerate().find(|(index, new)| {
            !used[*index] && old.kind == new.kind && old.name.is_some() && old.name == new.name
        }) {
            paired[old_index] = Some(new_index);
            used[new_index] = true;
        }
    }
    for (old_index, old) in before.iter().enumerate() {
        if paired[old_index].is_some() {
            continue;
        }
        if let Some((new_index, _)) = after
            .iter()
            .enumerate()
            .find(|(index, new)| !used[*index] && renamed(old, new))
        {
            paired[old_index] = Some(new_index);
            used[new_index] = true;
        }
    }
    for (old_index, old) in before.iter().enumerate() {
        if paired[old_index].is_some() {
            continue;
        }
        if let Some((new_index, _)) = after
            .iter()
            .enumerate()
            .find(|(index, new)| !used[*index] && old.kind == new.kind)
        {
            paired[old_index] = Some(new_index);
            used[new_index] = true;
        }
    }
    paired
}

fn stable_order(paired: &[Option<usize>]) -> Vec<bool> {
    let entries: Vec<_> = paired
        .iter()
        .enumerate()
        .filter_map(|(old, &new)| new.map(|new| (old, new)))
        .collect();
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = vec![None; entries.len()];
    for (index, &(_, new)) in entries.iter().enumerate() {
        let position = tails.partition_point(|&candidate| entries[candidate].1 < new);
        if position > 0 {
            previous[index] = Some(tails[position - 1]);
        }
        if position == tails.len() {
            tails.push(index);
        } else {
            tails[position] = index;
        }
    }
    let mut stable = vec![false; paired.len()];
    let mut index = tails.last().copied();
    while let Some(current) = index {
        stable[entries[current].0] = true;
        index = previous[current];
    }
    stable
}

impl StructuralAnalysis {
    pub fn compare(
        language: Option<Language>,
        before: &TextSnapshot,
        after: &TextSnapshot,
    ) -> Result<StructuralDiff, StructuralError> {
        let language = language.ok_or(StructuralError::UnsupportedLanguage)?;
        let before_tree = parse(language, before)?;
        let after_tree = parse(language, after)?;
        let old_units = units(before, &before_tree);
        let new_units = units(after, &after_tree);
        let paired = pair_units(&old_units, &new_units);
        let stable = stable_order(&paired);
        let mut changes = Vec::new();
        let mut used = vec![false; new_units.len()];
        for (old_index, new_index) in paired.iter().enumerate() {
            let old = &old_units[old_index];
            if let Some(new_index) = new_index {
                used[*new_index] = true;
                let new = &new_units[*new_index];
                let kind = if old.text == new.text {
                    if stable[old_index] {
                        continue;
                    }
                    StructuralChangeKind::Move
                } else if renamed(old, new) {
                    StructuralChangeKind::Renamed
                } else if same_tokens(old, new) {
                    StructuralChangeKind::Reflow
                } else {
                    StructuralChangeKind::ChangedTokens
                };
                changes.push(StructuralChange {
                    kind,
                    before: old.range.clone(),
                    after: new.range.clone(),
                });
            } else {
                changes.push(StructuralChange {
                    kind: StructuralChangeKind::ChangedTokens,
                    before: old.range.clone(),
                    after: after.as_bytes().len()..after.as_bytes().len(),
                });
            }
        }
        for (index, new) in new_units.iter().enumerate() {
            if !used[index] {
                changes.push(StructuralChange {
                    kind: StructuralChangeKind::ChangedTokens,
                    before: before.as_bytes().len()..before.as_bytes().len(),
                    after: new.range.clone(),
                });
            }
        }
        Ok(StructuralDiff { changes })
    }
}

pub fn highlight(
    language: Option<Language>,
    text: &TextSnapshot,
) -> Result<Vec<HighlightSpan>, StructuralError> {
    let language = language.ok_or(StructuralError::UnsupportedLanguage)?;
    let tree = parse(language, text)?;
    let query = language.compiled_query()?;
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), text.as_bytes());
    let mut spans = Vec::new();
    while let Some((matched, capture_index)) = captures.next() {
        let capture = matched.captures[*capture_index];
        let kind = match query.capture_names()[capture.index as usize] {
            "keyword" => HighlightKind::Keyword,
            "identifier" => HighlightKind::Identifier,
            "string" => HighlightKind::String,
            "number" => HighlightKind::Number,
            "comment" => HighlightKind::Comment,
            "type" => HighlightKind::Type,
            "function" => HighlightKind::Function,
            _ => HighlightKind::Punctuation,
        };
        spans.push(HighlightSpan {
            bytes: capture.node.byte_range(),
            kind,
        });
    }
    spans.sort_by_key(|span| (span.bytes.start, span.bytes.end));
    spans.dedup_by(|current, previous| {
        if current.bytes == previous.bytes {
            if current.kind == HighlightKind::Function {
                previous.kind = HighlightKind::Function;
            }
            true
        } else {
            false
        }
    });
    Ok(spans)
}
