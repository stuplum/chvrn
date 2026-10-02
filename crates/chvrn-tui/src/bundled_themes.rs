pub(super) fn source(name: &str) -> Option<&'static str> {
    match name {
        "darcula" => Some(include_str!("../themes/darcula.toml")),
        "catppuccin_latte" => Some(include_str!("../themes/catppuccin_latte.toml")),
        "catppuccin_frappe" => Some(include_str!("../themes/catppuccin_frappe.toml")),
        "catppuccin_macchiato" => Some(include_str!("../themes/catppuccin_macchiato.toml")),
        "catppuccin_mocha" => Some(include_str!("../themes/catppuccin_mocha.toml")),
        "gruvbox" => Some(include_str!("../themes/gruvbox.toml")),
        "tokyonight" => Some(include_str!("../themes/tokyonight.toml")),
        "github_light" => Some(include_str!("../themes/github_light.toml")),
        "solarized_dark" => Some(include_str!("../themes/solarized_dark.toml")),
        "solarized_light" => Some(include_str!("../themes/solarized_light.toml")),
        _ => None,
    }
}
