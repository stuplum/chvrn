use crate::{Options, OutputFormat, Result, Whitespace, terminal, theme_config};
use chvrn_core::unified::UnifiedPatch;
use chvrn_tui::PagerSession;
use std::io::{self, IsTerminal, Read};

pub fn run(options: &Options) -> Result<u8> {
    for (flag, present) in [
        ("--jev", options.jev),
        ("--herdr", options.herdr.is_some()),
        ("--agent", options.agent.is_some()),
        ("--lsp", options.lsp.is_some()),
        ("--lsp-arg", !options.lsp_args.is_empty()),
        (
            "--whitespace",
            !matches!(options.whitespace, Whitespace::Exact),
        ),
    ] {
        if present {
            return Err(format!(
                "pager does not support {flag}; it displays only the supplied diff"
            )
            .into());
        }
    }
    if options.format == OutputFormat::Json {
        return Err("pager does not support --format json; use auto or text".into());
    }
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(
            "pipe a unified diff into chvrn pager, for example: git diff | chvrn pager".into(),
        );
    }
    let mut input = stdin.lock();
    let stdout = io::stdout();
    if options.non_interactive || options.format == OutputFormat::Text || !stdout.is_terminal() {
        io::copy(&mut input, &mut stdout.lock())?;
        return Ok(0);
    }
    let mut source = String::new();
    input.read_to_string(&mut source)?;
    drop(input);
    let patch = UnifiedPatch::parse(source)?;
    if patch.files.is_empty() {
        return Ok(0);
    }
    let theme = theme_config::load_theme(options.theme.as_deref())?;
    let mut session = PagerSession::new(patch, theme.theme);
    terminal::run_pager(&mut session)
}
