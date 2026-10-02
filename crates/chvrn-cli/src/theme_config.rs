use crate::Result;
use chvrn_tui::Theme;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ThemeSelection {
    pub theme: Arc<Theme>,
    pub argument: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    theme: Option<String>,
}

pub fn load_theme(selection: Option<&str>) -> Result<ThemeSelection> {
    let directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|directory| directory.join("chvrn"));
    resolve_theme(selection, directory.as_deref())
}

fn resolve_theme(selection: Option<&str>, directory: Option<&Path>) -> Result<ThemeSelection> {
    let saved = if selection.is_none() {
        directory
            .map(|directory| -> Result<Config> {
                let path = directory.join("config.toml");
                match std::fs::read_to_string(&path) {
                    Ok(source) => toml::from_str::<Config>(&source)
                        .map_err(|error| format!("{}: {error}", path.display()).into()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Ok(Config::default())
                    }
                    Err(error) => Err(format!("{}: {error}", path.display()).into()),
                }
            })
            .transpose()?
            .unwrap_or_default()
            .theme
    } else {
        None
    };
    let selected = selection.or(saved.as_deref()).unwrap_or("chvrn");
    let path = Path::new(selected);
    let saved_path = if selection.is_none()
        && (path
            .extension()
            .is_some_and(|extension| extension == "toml")
            || path.components().count() > 1)
        && path.is_relative()
    {
        directory.map(|directory| directory.join(path))
    } else {
        None
    };
    let selected = saved_path
        .as_ref()
        .map(|path| path.to_str().ok_or("theme path is not valid UTF-8"))
        .transpose()?
        .unwrap_or(selected);
    let themes = directory.map(|directory| directory.join("themes"));
    let theme = Arc::new(Theme::load(selected, themes.as_deref())?);
    let argument = match theme.source_path() {
        Some(path) => path
            .to_str()
            .ok_or("theme path is not valid UTF-8")?
            .to_owned(),
        None => selected.to_owned(),
    };
    Ok(ThemeSelection { theme, argument })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use std::fs;

    #[test]
    fn explicit_selection_bypasses_an_invalid_saved_default() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("config.toml"), "theme = [").unwrap();
        let selection = resolve_theme(Some("darcula"), Some(directory.path())).unwrap();
        assert_eq!(selection.theme.name(), "darcula");
        assert_eq!(selection.argument, "darcula");
    }

    #[test]
    fn saved_relative_files_resolve_beside_the_configuration() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("config.toml"),
            "theme = './custom.toml'",
        )
        .unwrap();
        let path = directory.path().join("custom.toml");
        fs::write(
            &path,
            "'ui.background' = { bg = '#fafafa' }\n'ui.text' = '#123456'\n",
        )
        .unwrap();
        let selection = resolve_theme(None, Some(directory.path())).unwrap();
        assert_eq!(
            selection.theme.style("ui.text").fg,
            Some(Color::Rgb(18, 52, 86))
        );
        assert_eq!(Path::new(&selection.argument), path.canonicalize().unwrap());
    }

    #[test]
    fn named_user_themes_keep_their_source_when_a_companion_changes_directory() {
        let directory = tempfile::tempdir().unwrap();
        let themes = directory.path().join("themes");
        fs::create_dir(&themes).unwrap();
        let path = themes.join("personal.toml");
        fs::write(&path, "'ui.text' = '#102030'\n").unwrap();
        fs::write(directory.path().join("config.toml"), "theme = 'personal'").unwrap();
        let selection = resolve_theme(None, Some(directory.path())).unwrap();
        assert_eq!(Path::new(&selection.argument), path.canonicalize().unwrap());
        let reopened = chvrn_tui::Theme::load(&selection.argument, None).unwrap();
        assert_eq!(reopened.style("ui.text").fg, Some(Color::Rgb(16, 32, 48)));
    }

    #[test]
    fn invalid_saved_configuration_is_not_silently_ignored() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("config.toml"), "theme = 42").unwrap();
        let error = resolve_theme(None, Some(directory.path()))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("config.toml"));
    }
}
