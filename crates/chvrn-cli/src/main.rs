mod files;
mod herdr_ui;
mod jev_ui;
mod language;
mod repository;
mod socket_ui;
mod standalone;
mod terminal;
mod theme_config;
mod watch;

use chvrn_core::diff::WhitespacePolicy;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

#[derive(Parser)]
#[command(
    name = "chvrn",
    version,
    about = "Editable, snapshot-safe terminal diff and merge",
    after_help = "Bundled Helix themes are available under MPL-2.0. Theme source and licence:\nhttps://github.com/helix-editor/helix/tree/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    options: Options,
}

#[derive(Args)]
struct Options {
    #[arg(long, global = true, value_enum, default_value = "auto")]
    format: OutputFormat,
    #[arg(long, global = true)]
    non_interactive: bool,
    #[arg(
        long,
        global = true,
        help = "Enable on-demand Jev suggestions via TypeSafe (interactive only)"
    )]
    jev: bool,
    #[arg(
        long,
        global = true,
        help = "Select a bundled theme name or a Helix-compatible TOML file"
    )]
    theme: Option<String>,
    #[arg(skip)]
    loaded_theme: std::sync::Arc<chvrn_tui::Theme>,
    #[arg(long, global = true, value_enum)]
    herdr: Option<HerdrMode>,
    #[arg(long, global = true)]
    agent: Option<String>,
    #[arg(long, global = true, value_enum, default_value = "exact")]
    whitespace: Whitespace,
    #[arg(long, global = true)]
    lsp: Option<PathBuf>,
    #[arg(long = "lsp-arg", global = true, allow_hyphen_values = true)]
    lsp_args: Vec<OsString>,
}

impl Options {
    fn interactive(&self) -> bool {
        !self.non_interactive
            && self.format == OutputFormat::Auto
            && io::stdin().is_terminal()
            && io::stdout().is_terminal()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Auto,
    Text,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum HerdrMode {
    Companion,
    Gate,
    Auto,
}

#[derive(Clone, Copy, ValueEnum)]
enum Whitespace {
    Exact,
    IgnoreEdge,
    IgnoreAll,
    IgnoreBlankLines,
}

impl From<Whitespace> for WhitespacePolicy {
    fn from(value: Whitespace) -> Self {
        match value {
            Whitespace::Exact => Self::Exact,
            Whitespace::IgnoreEdge => Self::IgnoreEdge,
            Whitespace::IgnoreAll => Self::IgnoreAll,
            Whitespace::IgnoreBlankLines => Self::IgnoreBlankLines,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    Diff {
        left: PathBuf,
        right: PathBuf,
    },
    Merge(MergeArgs),
    Review(ReviewArgs),
    Difftool {
        left: Option<PathBuf>,
        right: Option<PathBuf>,
    },
    Mergetool {
        #[arg(long)]
        base: Option<PathBuf>,
        #[arg(long)]
        ours: Option<PathBuf>,
        #[arg(long)]
        theirs: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Args)]
struct MergeArgs {
    #[arg(long)]
    base: PathBuf,
    #[arg(long)]
    ours: PathBuf,
    #[arg(long)]
    theirs: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Args, Default)]
struct ReviewArgs {
    #[arg(
        long,
        help = "Compare directly with a revision or index; default: merge base of HEAD and CHVRN_BASE_BRANCH (main when unset)"
    )]
    base: Option<String>,
    #[arg(long)]
    patch: Option<PathBuf>,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(
        long,
        help = "Set patch export destination (interactive revision review; press P)"
    )]
    export_patch: Option<PathBuf>,
    #[arg(long, help = "Write a report after completed interactive review")]
    report: Option<PathBuf>,
    #[arg(long)]
    open_companion: bool,
    paths: Vec<PathBuf>,
}

fn environment_path(path: Option<PathBuf>, name: &str) -> Result<PathBuf> {
    path.or_else(|| std::env::var_os(name).map(PathBuf::from))
        .ok_or_else(|| format!("supply a path or set Git's {name} environment variable").into())
}

fn execute(cli: Cli) -> Result<u8> {
    let mut options = cli.options;
    let theme = theme_config::load_theme(options.theme.as_deref())?;
    options.loaded_theme = theme.theme;
    options.theme = Some(theme.argument);
    let options = &options;
    if options.jev && !options.interactive() {
        return Err("--jev requires an interactive session; output unchanged".into());
    }
    if options.herdr.is_some() && std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return Err("herdr integration requires HERDR_ENV=1 in the current session".into());
    }
    match cli.command {
        Some(Command::Diff { left, right }) => standalone::diff(left, right, options),
        Some(Command::Merge(args)) => standalone::merge(args, options),
        Some(Command::Difftool { left, right }) => standalone::diff(
            environment_path(left, "LOCAL")?,
            environment_path(right, "REMOTE")?,
            options,
        ),
        Some(Command::Mergetool {
            base,
            ours,
            theirs,
            output,
        }) => standalone::merge(
            MergeArgs {
                base: environment_path(base, "BASE")?,
                ours: environment_path(ours, "LOCAL")?,
                theirs: environment_path(theirs, "REMOTE")?,
                output: environment_path(output, "MERGED")?,
            },
            options,
        ),
        Some(Command::Review(args)) => repository::review(args, options),
        None => repository::review(ReviewArgs::default(), options),
    }
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("chvrn: {error}");
            ExitCode::from(2)
        }
    }
}
