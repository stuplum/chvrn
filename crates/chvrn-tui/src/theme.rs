use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use ratatui::style::{Color, Modifier, Style};
use toml::{Table, Value};

pub const BUNDLED_THEMES: &[&str] = &[
    "chvrn",
    "darcula",
    "catppuccin_latte",
    "catppuccin_frappe",
    "catppuccin_macchiato",
    "catppuccin_mocha",
    "gruvbox",
    "tokyonight",
    "github_light",
    "solarized_dark",
    "solarized_light",
];

#[derive(Clone, Debug)]
pub struct Theme {
    name: String,
    source_path: Option<PathBuf>,
    styles: HashMap<String, Style>,
}

#[derive(Debug)]
pub struct ThemeError {
    message: String,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ThemeError {}

impl ThemeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn invalid(source: &str, context: &str, message: impl fmt::Display) -> Self {
        Self::new(format!("Theme {source}, {context}: {message}"))
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "chvrn".to_owned(),
            source_path: None,
            styles: HashMap::new(),
        }
    }
}

impl Theme {
    pub fn load(selection: &str, directory: Option<&Path>) -> Result<Self, ThemeError> {
        if selection == "chvrn" {
            return Ok(Self::default());
        }
        let loader = Loader { directory };
        let source = loader.resolve(selection, None, &[])?;
        let source_path = match &source.id {
            SourceId::File(path) => Some(path.clone()),
            SourceId::Bundled(_) => None,
        };
        let label = source.id.to_string();
        let raw = loader.inherit(source, &mut Vec::new())?;
        let mut palette = HashMap::with_capacity(raw.palette.len());
        for (name, value) in raw.palette {
            let color = literal_color(value.as_str().ok_or_else(|| {
                ThemeError::invalid(
                    &label,
                    &format!("palette.{name}"),
                    "expected a colour string",
                )
            })?)
            .map_err(|error| ThemeError::invalid(&label, &format!("palette.{name}"), error))?;
            palette.insert(name, color);
        }
        let mut styles = HashMap::with_capacity(raw.styles.len());
        for (scope, value) in raw.styles {
            if scope == "rainbow" {
                let values = value.as_array().ok_or_else(|| {
                    ThemeError::invalid(&label, &scope, "expected an array of styles")
                })?;
                for (index, value) in values.iter().enumerate() {
                    let scope = format!("rainbow.{index}");
                    let style = parse_style(value, &palette)
                        .map_err(|error| ThemeError::invalid(&label, &scope, error))?;
                    styles.insert(scope, style);
                }
            } else {
                let style = parse_style(&value, &palette)
                    .map_err(|error| ThemeError::invalid(&label, &scope, error))?;
                styles.insert(scope, style);
            }
        }
        Ok(Self {
            name: selection.to_owned(),
            source_path,
            styles,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    pub fn style(&self, mut scope: &str) -> Style {
        loop {
            if let Some(style) = self.styles.get(scope) {
                return *style;
            }
            match scope.rsplit_once('.') {
                Some((parent, _)) => scope = parent,
                None => return Style::default(),
            }
        }
    }

    pub(crate) fn exact_style(&self, scope: &str) -> Option<Style> {
        self.styles.get(scope).copied()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SourceId {
    File(PathBuf),
    Bundled(String),
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(formatter, "{}", path.display()),
            Self::Bundled(name) => write!(formatter, "bundled:{name}"),
        }
    }
}

struct Source {
    id: SourceId,
    content: Cow<'static, str>,
}

#[derive(Default)]
struct RawTheme {
    styles: Table,
    palette: Table,
}

struct Loader<'a> {
    directory: Option<&'a Path>,
}

impl Loader<'_> {
    fn file(&self, path: &Path) -> Result<Option<Source>, ThemeError> {
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ThemeError::new(format!(
                    "Cannot open theme {}: {error}",
                    path.display()
                )));
            }
        };
        let content = fs::read_to_string(&path).map_err(|error| {
            ThemeError::new(format!("Cannot read theme {}: {error}", path.display()))
        })?;
        Ok(Some(Source {
            id: SourceId::File(path),
            content: Cow::Owned(content),
        }))
    }

    fn resolve(
        &self,
        selection: &str,
        relative_to: Option<&Path>,
        stack: &[SourceId],
    ) -> Result<Source, ThemeError> {
        let path = Path::new(selection);
        let explicit = path
            .extension()
            .is_some_and(|extension| extension == "toml")
            || path.components().count() > 1
            || path.is_absolute();
        let mut cycle = None;
        if selection == "chvrn" {
            return Err(ThemeError::new(format!(
                "Theme inherits='chvrn' is unsupported{}: chvrn uses the built-in legacy renderer palette, not an inheritable Helix theme file",
                inheritance_context(stack)
            )));
        } else if explicit {
            let path = relative_to.map_or_else(|| path.to_path_buf(), |base| base.join(path));
            match self.file(&path)? {
                Some(source) if stack.contains(&source.id) => cycle = Some(source.id),
                Some(source) => return Ok(source),
                None => {
                    return Err(ThemeError::new(format!(
                        "Theme file {} not found{}",
                        path.display(),
                        inheritance_context(stack)
                    )));
                }
            }
        } else {
            for directory in [relative_to, self.directory].into_iter().flatten() {
                let path = directory.join(format!("{selection}.toml"));
                if let Some(source) = self.file(&path)? {
                    if stack.contains(&source.id) {
                        cycle = Some(source.id);
                    } else {
                        return Ok(source);
                    }
                }
            }
        }
        if !explicit {
            if let Some(content) = crate::bundled_themes::source(selection) {
                let id = SourceId::Bundled(selection.to_owned());
                if stack.contains(&id) {
                    cycle = Some(id);
                } else {
                    return Ok(Source {
                        id,
                        content: Cow::Borrowed(content),
                    });
                }
            }
        }
        if let Some(id) = cycle {
            let mut chain: Vec<String> = stack.iter().map(ToString::to_string).collect();
            chain.push(id.to_string());
            return Err(ThemeError::new(format!(
                "Theme inheritance cycle: {}",
                chain.join(" -> ")
            )));
        }
        Err(ThemeError::new(format!(
            "Unknown theme {selection:?}{}; choose a bundled theme or provide an existing .toml file",
            inheritance_context(stack)
        )))
    }

    fn inherit(&self, source: Source, stack: &mut Vec<SourceId>) -> Result<RawTheme, ThemeError> {
        let label = source.id.to_string();
        let mut styles: Table = toml::from_str(&source.content)
            .map_err(|error| ThemeError::invalid(&label, "TOML", error))?;
        let parent = styles.remove("inherits");
        let palette = match styles.remove("palette") {
            Some(Value::Table(palette)) => palette,
            Some(_) => return Err(ThemeError::invalid(&label, "palette", "expected a table")),
            None => Table::new(),
        };
        let relative_to = match &source.id {
            SourceId::File(path) => path.parent().map(Path::to_path_buf),
            SourceId::Bundled(_) => None,
        };
        stack.push(source.id);
        let mut raw = if let Some(parent) = parent {
            let parent = parent.as_str().ok_or_else(|| {
                ThemeError::invalid(&label, "inherits", "expected a parent theme name or path")
            })?;
            let source = self.resolve(parent, relative_to.as_deref(), stack)?;
            self.inherit(source, stack)?
        } else {
            RawTheme::default()
        };
        stack.pop();
        raw.styles.extend(styles);
        raw.palette.extend(palette);
        Ok(raw)
    }
}

fn inheritance_context(stack: &[SourceId]) -> String {
    stack
        .last()
        .map(|source| format!(" inherited by {source}"))
        .unwrap_or_default()
}

fn literal_color(value: &str) -> Result<Color, String> {
    let named = match value {
        "default" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" => Some(Color::Gray),
        "light-red" => Some(Color::LightRed),
        "light-green" => Some(Color::LightGreen),
        "light-yellow" => Some(Color::LightYellow),
        "light-blue" => Some(Color::LightBlue),
        "light-magenta" => Some(Color::LightMagenta),
        "light-cyan" => Some(Color::LightCyan),
        "light-gray" => Some(Color::DarkGray),
        "white" => Some(Color::White),
        _ => None,
    };
    if let Some(color) = named {
        return Ok(color);
    }
    if let Some(hex) = value.strip_prefix('#') {
        if hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            if hex.len() == 6 {
                let rgb = u32::from_str_radix(hex, 16).map_err(|error| error.to_string())?;
                return Ok(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
            }
            if hex.len() == 3 {
                let rgb = u16::from_str_radix(hex, 16).map_err(|error| error.to_string())?;
                return Ok(Color::Rgb(
                    ((rgb >> 8) as u8) * 17,
                    (((rgb >> 4) & 15) as u8) * 17,
                    ((rgb & 15) as u8) * 17,
                ));
            }
        }
    } else if let Ok(index) = value.parse::<u8>() {
        return Ok(Color::Indexed(index));
    }
    Err(format!("invalid colour or unknown palette entry {value:?}"))
}

fn parse_color(value: &Value, palette: &HashMap<String, Color>) -> Result<Color, String> {
    let value = value
        .as_str()
        .ok_or_else(|| format!("expected a colour string, got {value}"))?;
    palette
        .get(value)
        .copied()
        .map_or_else(|| literal_color(value), Ok)
}

fn parse_style(value: &Value, palette: &HashMap<String, Color>) -> Result<Style, String> {
    let Some(table) = value.as_table() else {
        return parse_color(value, palette).map(|color| Style::default().fg(color));
    };
    let mut style = Style::default();
    for (attribute, value) in table {
        match attribute.as_str() {
            "fg" => style = style.fg(parse_color(value, palette)?),
            "bg" => style = style.bg(parse_color(value, palette)?),
            "modifiers" => {
                let modifiers = value.as_array().ok_or("modifiers must be an array")?;
                for modifier in modifiers {
                    let modifier = match modifier.as_str() {
                        Some("bold") => Modifier::BOLD,
                        Some("dim") => Modifier::DIM,
                        Some("italic") => Modifier::ITALIC,
                        Some("underlined") => Modifier::UNDERLINED,
                        Some("slow_blink") => Modifier::SLOW_BLINK,
                        Some("rapid_blink") => Modifier::RAPID_BLINK,
                        Some("reversed") => Modifier::REVERSED,
                        Some("hidden") => Modifier::HIDDEN,
                        Some("crossed_out") => Modifier::CROSSED_OUT,
                        _ => return Err(format!("invalid modifier {modifier}")),
                    };
                    style = style.add_modifier(modifier);
                }
            }
            "underline" => {}
            _ => return Err(format!("invalid style attribute {attribute:?}")),
        }
    }
    if let Some(underline) = table.get("underline") {
        let underline = underline.as_table().ok_or("underline must be a table")?;
        for (attribute, value) in underline {
            match attribute.as_str() {
                "color" => style = style.underline_color(parse_color(value, palette)?),
                "style" => {
                    style = match value.as_str() {
                        Some("reset") => style.remove_modifier(Modifier::UNDERLINED),
                        Some("line" | "curl" | "dotted" | "dashed" | "double_line") => {
                            style.add_modifier(Modifier::UNDERLINED)
                        }
                        _ => return Err(format!("invalid underline style {value}")),
                    };
                }
                _ => return Err(format!("invalid underline attribute {attribute:?}")),
            }
        }
    }
    Ok(style)
}
