use chvrn_tui::Theme;
use ratatui::style::{Color, Modifier, Style};
use std::fs;
use tempfile::tempdir;

#[test]
fn inherited_styles_use_the_final_child_palette() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("base.toml"),
        r##"
"ui.text" = { fg = "ink", bg = "paper", modifiers = ["italic"] }
"keyword" = { fg = "ink", modifiers = ["bold"] }
[palette]
ink = "#112233"
paper = "#eeeeee"
"##,
    )
    .unwrap();
    fs::write(
        directory.path().join("middle.toml"),
        "inherits = 'base'\n[palette]\npaper = '#ffffff'\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("child.toml"),
        "inherits = 'middle'\nkeyword = { bg = 'ink' }\n[palette]\nink = '#abcdef'\n",
    )
    .unwrap();

    let theme = Theme::load("child", Some(directory.path())).unwrap();
    assert_eq!(
        theme.style("ui.text.focus"),
        Style::default()
            .fg(Color::Rgb(171, 205, 239))
            .bg(Color::Rgb(255, 255, 255))
            .add_modifier(Modifier::ITALIC)
    );
    assert_eq!(
        theme.style("keyword.control"),
        Style::default().bg(Color::Rgb(171, 205, 239))
    );
}

#[test]
fn specific_scope_replaces_parent_and_empty_scope_blocks_fallback() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("scopes.toml"),
        r##"
"ui" = { bg = "#010203" }
"ui.text" = "#aabbcc"
"ui.text.focus" = {}
"##,
    )
    .unwrap();
    let theme = Theme::load("scopes", Some(directory.path())).unwrap();
    assert_eq!(
        theme.style("ui.text.inactive"),
        Style::default().fg(Color::Rgb(170, 187, 204))
    );
    assert_eq!(theme.style("ui.text.focus.extra"), Style::default());
    assert_eq!(theme.style("unknown.scope"), Style::default());
}

#[test]
fn explicit_file_resolves_sibling_parent_before_config_directory() {
    let directory = tempdir().unwrap();
    let other_directory = tempdir().unwrap();
    fs::write(directory.path().join("parent.toml"), "keyword = '#abc'\n").unwrap();
    fs::write(
        other_directory.path().join("parent.toml"),
        "keyword = '#000'\n",
    )
    .unwrap();
    let path = directory.path().join("chvrn.toml");
    fs::write(&path, "inherits = 'parent'\n").unwrap();

    let theme = Theme::load(path.to_str().unwrap(), Some(other_directory.path())).unwrap();
    assert_eq!(
        theme.style("keyword"),
        Style::default().fg(Color::Rgb(170, 187, 204))
    );
    assert_eq!(
        theme.source_path(),
        Some(path.canonicalize().unwrap().as_path())
    );
    assert_ne!(theme.name(), "chvrn");
}

#[test]
fn user_override_can_inherit_its_bundled_namesake() {
    let directory = tempdir().unwrap();
    let bundled = Theme::load("gruvbox", None).unwrap();
    let path = directory.path().join("gruvbox.toml");
    fs::write(&path, "inherits = 'gruvbox'\nkeyword = '#010203'\n").unwrap();
    let custom = Theme::load("gruvbox", Some(directory.path())).unwrap();

    assert_eq!(
        custom.style("keyword"),
        Style::default().fg(Color::Rgb(1, 2, 3))
    );
    assert_eq!(
        custom.style("ui.background"),
        bundled.style("ui.background")
    );
    assert_eq!(
        custom.source_path(),
        Some(path.canonicalize().unwrap().as_path())
    );
    assert_eq!(bundled.source_path(), None);
}

#[test]
fn reserved_chvrn_ignores_user_override_file() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("chvrn.toml"), "invalid TOML [").unwrap();
    let theme = Theme::load("chvrn", Some(directory.path())).unwrap();
    assert_eq!(theme.name(), Theme::default().name());
    assert_eq!(theme.source_path(), None);
}

#[test]
fn reserved_legacy_palette_cannot_silently_become_an_empty_parent() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("child.toml"),
        "inherits = 'chvrn'\nkeyword = '#abcdef'\n",
    )
    .unwrap();
    let error = Theme::load("child", Some(directory.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("child.toml"), "{error}");
    assert!(error.contains("inherits"), "{error}");
    assert!(error.contains("chvrn"), "{error}");
    assert!(error.contains("legacy"), "{error}");
    assert_eq!(Theme::load("chvrn", None).unwrap().name(), "chvrn");
}

#[test]
fn helix_colours_modifiers_and_underlines_survive_loading() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("styles.toml"),
        r##"
"keyword" = { fg = "light-red", bg = "default", modifiers = ["bold", "italic", "dim", "slow_blink", "rapid_blink", "reversed", "hidden", "crossed_out"] }
"diagnostic.error" = { underline = { color = "accent", style = "curl" } }
"diagnostic.warning" = { modifiers = ["underlined"], underline = { style = "reset" } }
"ui.virtual.unused" = { fg = "255", underline = { style = "double_line" } }
rainbow = ["#123", { fg = "accent", modifiers = ["bold"] }]
[palette]
accent = "#123456"
"##,
    )
    .unwrap();
    let theme = Theme::load("styles", Some(directory.path())).unwrap();
    assert_eq!(
        theme.style("keyword"),
        Style::default()
            .fg(Color::LightRed)
            .bg(Color::Reset)
            .add_modifier(
                Modifier::BOLD
                    | Modifier::ITALIC
                    | Modifier::DIM
                    | Modifier::SLOW_BLINK
                    | Modifier::RAPID_BLINK
                    | Modifier::REVERSED
                    | Modifier::HIDDEN
                    | Modifier::CROSSED_OUT
            )
    );
    assert_eq!(
        theme.style("diagnostic.error"),
        Style::default()
            .underline_color(Color::Rgb(18, 52, 86))
            .add_modifier(Modifier::UNDERLINED)
    );
    assert!(
        theme
            .style("diagnostic.warning")
            .sub_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(
        theme.style("ui.virtual.unused"),
        Style::default()
            .fg(Color::Indexed(255))
            .add_modifier(Modifier::UNDERLINED)
    );
    assert_eq!(
        theme.style("rainbow.0"),
        Style::default().fg(Color::Rgb(17, 34, 51))
    );
    assert_eq!(
        theme.style("rainbow.1"),
        Style::default()
            .fg(Color::Rgb(18, 52, 86))
            .add_modifier(Modifier::BOLD)
    );
}

#[test]
fn malformed_values_identify_the_file_and_failed_scope() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("broken.toml");
    for (source, context) in [
        ("keyword = '#12gg45'", "keyword"),
        ("keyword = '#éabc'", "keyword"),
        ("keyword = 'missing-palette-entry'", "keyword"),
        ("keyword = '256'", "keyword"),
        ("keyword = { modifiers = ['invented'] }", "keyword"),
        (
            "keyword = { underline = { style = 'invented' } }",
            "keyword",
        ),
        ("keyword = { typo = 'red' }", "keyword"),
        ("palette = { invalid = '#xyz' }", "palette"),
        ("inherits = 42", "inherits"),
    ] {
        fs::write(&path, source).unwrap();
        let error = Theme::load("broken", Some(directory.path()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("broken.toml"), "{error}");
        assert!(error.contains(context), "{error}");
    }
}

#[test]
fn invalid_toml_does_not_fall_back_to_bundled_theme() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("gruvbox.toml"), "keyword = [").unwrap();
    let error = Theme::load("gruvbox", Some(directory.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("gruvbox.toml"), "{error}");
    assert!(error.contains("TOML"), "{error}");
}

#[test]
fn missing_parent_names_the_child_and_parent() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("child.toml"),
        "inherits = 'absent-parent'\n",
    )
    .unwrap();
    let error = Theme::load("child", Some(directory.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("child.toml"), "{error}");
    assert!(error.contains("absent-parent"), "{error}");
}

#[test]
fn cyclic_inheritance_identifies_every_participant() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("first.toml"), "inherits = 'second'\n").unwrap();
    fs::write(
        directory.path().join("second.toml"),
        "inherits = './first.toml'\n",
    )
    .unwrap();
    let error = Theme::load("first", Some(directory.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("cycle"), "{error}");
    assert!(error.contains("first.toml"), "{error}");
    assert!(error.contains("second.toml"), "{error}");
}

#[test]
fn unknown_name_and_missing_explicit_path_have_actionable_errors() {
    let directory = tempdir().unwrap();
    let error = Theme::load("not-a-bundled-theme", Some(directory.path()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not-a-bundled-theme"), "{error}");
    let path = directory.path().join("missing.toml");
    let error = Theme::load(path.to_str().unwrap(), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()), "{error}");
}
