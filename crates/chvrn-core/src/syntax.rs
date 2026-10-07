use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::TextSnapshot;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub bytes: Range<usize>,
    pub kind: HighlightKind,
}

#[derive(Debug)]
pub struct SyntaxError {
    pub path: PathBuf,
    message: String,
}

impl SyntaxError {
    fn new(path: &Path, error: impl fmt::Display) -> Self {
        Self {
            path: path.to_owned(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "syntax error for {}: {}",
            self.path.display(),
            self.message
        )
    }
}

impl std::error::Error for SyntaxError {}

#[derive(Clone)]
pub struct SyntaxCatalog {
    syntaxes: Arc<SyntaxSet>,
}

static BUNDLED: LazyLock<Arc<SyntaxSet>> =
    LazyLock::new(|| Arc::new(two_face::syntax::extra_newlines()));

static SCOPE_KINDS: LazyLock<Vec<(Scope, HighlightKind)>> = LazyLock::new(|| {
    [
        ("comment", HighlightKind::Comment),
        ("string", HighlightKind::String),
        ("constant.numeric", HighlightKind::Number),
        ("keyword", HighlightKind::Keyword),
        ("storage", HighlightKind::Keyword),
        ("constant.language", HighlightKind::Keyword),
        ("entity.name.function", HighlightKind::Function),
        ("support.function", HighlightKind::Function),
        ("variable.function", HighlightKind::Function),
        ("entity.name.type", HighlightKind::Type),
        ("entity.name.class", HighlightKind::Type),
        ("entity.name.struct", HighlightKind::Type),
        ("entity.name.enum", HighlightKind::Type),
        ("entity.name.trait", HighlightKind::Type),
        ("entity.name.interface", HighlightKind::Type),
        ("support.type", HighlightKind::Type),
        ("support.class", HighlightKind::Type),
        ("variable", HighlightKind::Identifier),
        ("entity.name", HighlightKind::Identifier),
        ("entity.other.attribute-name", HighlightKind::Identifier),
        ("constant.other", HighlightKind::Identifier),
        ("punctuation", HighlightKind::Punctuation),
    ]
    .into_iter()
    .map(|(scope, kind)| (Scope::new(scope).expect("valid built-in scope"), kind))
    .collect()
});

impl SyntaxCatalog {
    pub fn bundled() -> Self {
        Self {
            syntaxes: Arc::clone(&BUNDLED),
        }
    }

    pub fn configured() -> Result<Self, SyntaxError> {
        let nonempty_env = |name| std::env::var_os(name).filter(|value| !value.is_empty());
        let directory = nonempty_env("CHVRN_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                nonempty_env("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("chvrn"))
            })
            .or_else(|| nonempty_env("HOME").map(|path| PathBuf::from(path).join(".config/chvrn")))
            .map(|path| path.join("syntaxes"));
        let Some(directory) = directory else {
            return Ok(Self::bundled());
        };
        match std::fs::metadata(&directory) {
            Ok(_) => Self::with_custom_syntaxes(&directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::bundled()),
            Err(error) => Err(SyntaxError::new(&directory, error)),
        }
    }

    pub fn with_custom_syntaxes(directory: &Path) -> Result<Self, SyntaxError> {
        let metadata =
            std::fs::metadata(directory).map_err(|error| SyntaxError::new(directory, error))?;
        if !metadata.is_dir() {
            return Err(SyntaxError::new(directory, "expected a syntax directory"));
        }
        let mut builder = BUNDLED.as_ref().clone().into_builder();
        builder
            .add_from_folder(directory, true)
            .map_err(|error| SyntaxError::new(directory, error))?;
        Ok(Self {
            syntaxes: Arc::new(builder.build()),
        })
    }

    pub fn highlight(
        &self,
        path: &Path,
        source: &TextSnapshot,
    ) -> Result<Vec<HighlightSpan>, SyntaxError> {
        self.highlight_fragment(path, source.text().lines().next(), source)
    }

    pub fn highlight_fragment(
        &self,
        path: &Path,
        first_line: Option<&str>,
        source: &TextSnapshot,
    ) -> Result<Vec<HighlightSpan>, SyntaxError> {
        let Some(syntax) = self.syntax_for(path, first_line) else {
            return Ok(Vec::new());
        };
        let mut parser = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut spans = Vec::new();
        let mut offset = 0;
        for line in LinesWithEndings::from(source.text()) {
            let operations = parser
                .parse_line(line, &self.syntaxes)
                .map_err(|error| SyntaxError::new(path, error))?;
            let mut previous = 0;
            for (position, operation) in operations {
                append_span(&mut spans, offset + previous..offset + position, &stack);
                stack
                    .apply(&operation)
                    .map_err(|error| SyntaxError::new(path, error))?;
                previous = position;
            }
            append_span(&mut spans, offset + previous..offset + line.len(), &stack);
            offset += line.len();
        }
        Ok(spans)
    }

    fn syntax_for(&self, path: &Path, first_line: Option<&str>) -> Option<&SyntaxReference> {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| self.syntaxes.find_syntax_by_extension(name))
            .or_else(|| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(|extension| self.syntaxes.find_syntax_by_extension(extension))
            })
            .or_else(|| {
                let first_line = first_line?.trim_start_matches('\u{feff}');
                first_line
                    .starts_with("#!")
                    .then(|| self.syntaxes.find_syntax_by_first_line(first_line))
                    .flatten()
            })
    }
}

fn append_span(spans: &mut Vec<HighlightSpan>, bytes: Range<usize>, stack: &ScopeStack) {
    if bytes.is_empty() {
        return;
    }
    let mut kind = None;
    for scope in stack.scopes.iter().rev() {
        if let Some((_, matched)) = SCOPE_KINDS
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
        {
            kind = Some(*matched);
            if *matched != HighlightKind::Punctuation {
                break;
            }
        }
    }
    let Some(kind) = kind else {
        return;
    };
    if let Some(previous) = spans.last_mut() {
        if previous.kind == kind && previous.bytes.end == bytes.start {
            previous.bytes.end = bytes.end;
            return;
        }
    }
    spans.push(HighlightSpan { bytes, kind });
}
