use std::{path::Path, sync::Arc};

use chvrn_core::{
    TextSnapshot,
    diff::intraline_spans,
    structural::{HighlightSpan, Language, highlight},
    unified::{PatchFile, PatchHunk, PatchLineKind, UnifiedPatch},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{Frame, buffer::Buffer, layout::Rect, style::Style};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Pane, Theme,
    render::{RenderPalette, render_line},
    session::{ChangeKind, ViewLine},
    text,
};

const HELP: &[&str] = &[
    "CHVRN PAGER: read-only patch review",
    "Ctrl-N / Ctrl-P  Next / previous file",
    "] / [            Next / previous supplied hunk",
    "Up / Down        Scroll one row",
    "PageDown / Ctrl-F  Scroll one page down",
    "PageUp / Ctrl-B    Scroll one page up",
    "Ctrl-D / Ctrl-U    Scroll half a page",
    "Left / Right     Scroll horizontally",
    "Home / End       First / last rows of this file",
    "Tab              Switch focused side on narrow screens",
    "Mouse wheel      Scroll; click a side to focus it",
    "?                Open / close this help",
    "q / Esc          Close help, then quit",
    "- / +            Removed / added source lines",
    "Omitted context is not reconstructed from the patch.",
];

pub struct PagerSession {
    patch: UnifiedPatch,
    palette: RenderPalette,
    files: Vec<Option<CachedFile>>,
    file: usize,
    hunk: usize,
    top: usize,
    horizontal: usize,
    focused: usize,
    viewport: Rect,
    help: bool,
    help_top: usize,
    help_lines: Vec<ViewLine>,
}

struct CachedFile {
    titles: [ViewLine; 2],
    rows: Vec<Row>,
    hunks: Vec<CachedHunk>,
    gutter: u16,
    width: usize,
}

struct CachedHunk {
    row: usize,
    syntax: [Vec<HighlightSpan>; 2],
}

enum Row {
    Notice(ViewLine),
    Source {
        sides: [Option<ViewLine>; 2],
        hunk: usize,
        change: Option<ChangeKind>,
    },
}

impl PagerSession {
    pub fn new(patch: UnifiedPatch, theme: Arc<Theme>) -> Self {
        let palette = RenderPalette::compile(&theme);
        let palette = if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            palette.without_color()
        } else {
            palette
        };
        let files = (0..patch.files.len()).map(|_| None).collect();
        let mut session = Self {
            patch,
            palette,
            files,
            file: 0,
            hunk: 0,
            top: 0,
            horizontal: 0,
            focused: 1,
            viewport: Rect::default(),
            help: false,
            help_top: 0,
            help_lines: HELP.iter().map(|line| label(line.to_string())).collect(),
        };
        session.prepare_file();
        session
    }

    fn prepare_file(&mut self) {
        if let Some(cache) = self.files.get_mut(self.file) {
            if cache.is_none() {
                *cache = Some(CachedFile::new(&self.patch, &self.patch.files[self.file]));
            }
        }
    }

    fn current(&self) -> Option<&CachedFile> {
        self.files.get(self.file)?.as_ref()
    }

    pub fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    return false;
                }
                let control = key.modifiers.contains(KeyModifiers::CONTROL);
                if !control && matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    if self.help {
                        self.help = false;
                        return false;
                    }
                    return true;
                }
                if !control && key.code == KeyCode::Char('?') {
                    self.help = !self.help;
                    self.help_top = 0;
                    return false;
                }
                let page = usize::from(self.viewport.height).max(1);
                let half = (page / 2).max(1);
                if self.help {
                    match (key.code, control) {
                        (KeyCode::Down, false) => self.scroll_help(1, true),
                        (KeyCode::Up, false) => self.scroll_help(1, false),
                        (KeyCode::PageDown, false) | (KeyCode::Char('f'), true) => {
                            self.scroll_help(page, true)
                        }
                        (KeyCode::PageUp, false) | (KeyCode::Char('b'), true) => {
                            self.scroll_help(page, false)
                        }
                        _ => {}
                    }
                    return false;
                }
                match (key.code, control) {
                    (KeyCode::Char('n'), true) => self.move_file(true),
                    (KeyCode::Char('p'), true) => self.move_file(false),
                    (KeyCode::Char(']'), false) => self.move_hunk(true),
                    (KeyCode::Char('['), false) => self.move_hunk(false),
                    (KeyCode::Down, false) => self.scroll(1, true),
                    (KeyCode::Up, false) => self.scroll(1, false),
                    (KeyCode::PageDown, false) | (KeyCode::Char('f'), true) => {
                        self.scroll(page, true)
                    }
                    (KeyCode::PageUp, false) | (KeyCode::Char('b'), true) => {
                        self.scroll(page, false)
                    }
                    (KeyCode::Char('d'), true) => self.scroll(half, true),
                    (KeyCode::Char('u'), true) => self.scroll(half, false),
                    (KeyCode::Home, false) => {
                        self.top = 0;
                        self.hunk = 0;
                    }
                    (KeyCode::End, false) => {
                        self.top = self
                            .current()
                            .map_or(0, |file| file.rows.len().saturating_sub(page));
                        self.update_hunk();
                    }
                    (KeyCode::Left, false) => self.horizontal = self.horizontal.saturating_sub(1),
                    (KeyCode::Right, false) => {
                        let max = self
                            .current()
                            .map_or(0, |file| file.width.saturating_sub(1));
                        self.horizontal = self.horizontal.saturating_add(1).min(max);
                    }
                    (KeyCode::Tab | KeyCode::BackTab, false) => self.focused = 1 - self.focused,
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                if self.help {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => self.scroll_help(3, true),
                        MouseEventKind::ScrollUp => self.scroll_help(3, false),
                        _ => {}
                    }
                } else {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => self.scroll(3, true),
                        MouseEventKind::ScrollUp => self.scroll(3, false),
                        MouseEventKind::ScrollLeft => {
                            self.horizontal = self.horizontal.saturating_sub(3)
                        }
                        MouseEventKind::ScrollRight => {
                            let max = self
                                .current()
                                .map_or(0, |file| file.width.saturating_sub(1));
                            self.horizontal = self.horizontal.saturating_add(3).min(max);
                        }
                        MouseEventKind::Down(MouseButton::Left) if self.viewport.width >= 80 => {
                            self.focused = usize::from(
                                mouse.column >= self.viewport.x + self.viewport.width / 2,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Event::Resize(width, height) => {
                self.viewport.width = width;
                self.viewport.height = height.saturating_sub(3);
            }
            _ => {}
        }
        false
    }

    fn scroll_help(&mut self, amount: usize, down: bool) {
        self.help_top = if down {
            self.help_top
                .saturating_add(amount)
                .min(self.help_lines.len().saturating_sub(1))
        } else {
            self.help_top.saturating_sub(amount)
        };
    }

    fn move_file(&mut self, next: bool) {
        let target = if next {
            self.file
                .saturating_add(1)
                .min(self.files.len().saturating_sub(1))
        } else {
            self.file.saturating_sub(1)
        };
        if target != self.file {
            self.file = target;
            self.hunk = 0;
            self.top = 0;
            self.horizontal = 0;
            self.prepare_file();
        }
    }

    fn move_hunk(&mut self, next: bool) {
        let Some(file) = self.current() else {
            return;
        };
        if file.hunks.is_empty() {
            return;
        }
        let target = if next {
            self.hunk.saturating_add(1).min(file.hunks.len() - 1)
        } else {
            self.hunk.saturating_sub(1)
        };
        self.top = file.hunks[target].row;
        self.hunk = target;
    }

    fn update_hunk(&mut self) {
        self.hunk = self.current().map_or(0, |file| {
            file.hunks
                .partition_point(|hunk| hunk.row <= self.top)
                .saturating_sub(1)
        });
    }

    fn scroll(&mut self, amount: usize, down: bool) {
        let max = self.current().map_or(0, |file| {
            file.rows
                .len()
                .saturating_sub(usize::from(self.viewport.height).max(1))
        });
        self.top = if down {
            self.top.saturating_add(amount).min(max.max(self.top))
        } else {
            self.top.saturating_sub(amount)
        };
        self.update_hunk();
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let buffer = frame.buffer_mut();
        buffer.set_style(area, self.palette.surface);
        if area.is_empty() {
            return;
        }
        if self.help {
            self.viewport = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
            buffer.set_style(area, self.palette.help);
            for (offset, line) in self
                .help_lines
                .iter()
                .skip(self.help_top)
                .take(usize::from(self.viewport.height))
                .enumerate()
            {
                paint_label(
                    buffer,
                    self.viewport,
                    area.y + offset as u16,
                    line,
                    0,
                    &self.palette,
                    self.palette.help,
                );
            }
            buffer.set_stringn(
                area.x,
                area.bottom() - 1,
                "q / Esc close help · arrows scroll",
                usize::from(area.width),
                self.palette.heading,
            );
            return;
        }
        self.viewport = Rect::new(
            area.x,
            area.y.saturating_add(2).min(area.bottom()),
            area.width,
            area.height.saturating_sub(3),
        );
        let count = self.current().map_or(0, |file| file.hunks.len());
        let title = format!(
            "CHVRN PAGER  file {}/{}  hunk {}/{}  read-only",
            usize::from(!self.files.is_empty()) + self.file,
            self.files.len(),
            if count == 0 { 0 } else { self.hunk + 1 },
            count
        );
        buffer.set_style(
            Rect::new(area.x, area.y, area.width, 1),
            self.palette.heading,
        );
        buffer.set_stringn(
            area.x,
            area.y,
            title,
            usize::from(area.width),
            self.palette.accent,
        );
        if area.height < 2 {
            return;
        }
        let Some(file) = self.current() else {
            buffer.set_stringn(
                area.x,
                area.y + 1,
                "No changes",
                usize::from(area.width),
                self.palette.quiet,
            );
            return;
        };
        let wide = area.width >= 80;
        let split = area.width / 2;
        let panes = if wide {
            [
                Rect::new(
                    area.x,
                    area.y + 1,
                    split.saturating_sub(1),
                    area.height.saturating_sub(2),
                ),
                Rect::new(
                    area.x + split,
                    area.y + 1,
                    area.width - split,
                    area.height.saturating_sub(2),
                ),
            ]
        } else {
            [Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(2),
            ); 2]
        };
        for side in 0..2 {
            if !wide && side != self.focused {
                continue;
            }
            let style = if self.focused == side {
                self.palette.accent
            } else {
                self.palette.heading
            };
            buffer.set_style(
                Rect::new(panes[side].x, area.y + 1, panes[side].width, 1),
                style,
            );
            paint_label(
                buffer,
                panes[side],
                area.y + 1,
                &file.titles[side],
                self.horizontal,
                &self.palette,
                style,
            );
        }
        for (offset, row) in file
            .rows
            .iter()
            .skip(self.top)
            .take(usize::from(self.viewport.height))
            .enumerate()
        {
            let y = self.viewport.y + offset as u16;
            match row {
                Row::Notice(line) => paint_label(
                    buffer,
                    self.viewport,
                    y,
                    line,
                    self.horizontal,
                    &self.palette,
                    self.palette.quiet,
                ),
                Row::Source {
                    sides,
                    hunk,
                    change,
                } => {
                    for side in 0..2 {
                        if !wide && side != self.focused {
                            continue;
                        }
                        let pane = panes[side];
                        let Some(line) = &sides[side] else {
                            continue;
                        };
                        let gutter = file.gutter.min(pane.width.saturating_sub(1));
                        let marker = match (change, side) {
                            (Some(ChangeKind::Modified | ChangeKind::Removed), 0) => '-',
                            (Some(ChangeKind::Modified | ChangeKind::Added), 1) => '+',
                            _ => ' ',
                        };
                        crate::render::render_line_number(
                            buffer,
                            pane.x,
                            y,
                            gutter.saturating_sub(1),
                            line.number,
                            self.palette.line_number,
                        );
                        if gutter > 0 {
                            buffer[(pane.x + gutter.saturating_sub(2), y)]
                                .set_char(marker)
                                .set_style(self.palette.line_number);
                        }
                        let content =
                            Rect::new(pane.x + gutter, y, pane.width.saturating_sub(gutter), 1);
                        let paint = change.map_or(self.palette.neutral, |kind| {
                            self.palette.region(
                                kind,
                                if side == 0 { Pane::Left } else { Pane::Right },
                                false,
                            )
                        });
                        buffer.set_style(content, self.palette.surface.patch(paint.normal));
                        render_line(
                            buffer,
                            content,
                            y,
                            line,
                            &file.hunks[*hunk].syntax[side],
                            self.horizontal,
                            &self.palette,
                            paint,
                        );
                    }
                }
            }
            if wide && matches!(row, Row::Source { .. }) {
                buffer.set_string(area.x + split - 1, y, "│", self.palette.quiet);
            }
        }
        if area.height >= 3 {
            let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
            buffer.set_style(footer, self.palette.heading);
            buffer.set_stringn(
                footer.x,
                footer.y,
                "Ctrl-N/P files  [/] hunks  arrows scroll  Tab side  ? help  q quit",
                usize::from(footer.width),
                self.palette.quiet,
            );
        }
    }
}

impl CachedFile {
    fn new(patch: &UnifiedPatch, file: &PatchFile) -> Self {
        let titles = [
            label(format!(
                "OLD  {}",
                file.old_path.as_deref().unwrap_or("/dev/null")
            )),
            label(format!(
                "NEW  {}",
                file.new_path.as_deref().unwrap_or("/dev/null")
            )),
        ];
        let mut rows: Vec<Row> = file
            .metadata
            .iter()
            .map(|line| Row::Notice(label(line.clone())))
            .collect();
        if file.binary
            && !file
                .metadata
                .iter()
                .any(|line| line.to_ascii_lowercase().contains("binary"))
        {
            rows.push(Row::Notice(label(
                "Binary file change (contents not shown)".into(),
            )));
        }
        let languages = [file.old_path.as_deref(), file.new_path.as_deref()]
            .map(|path| path.and_then(|path| Language::for_path(Path::new(path))));
        let mut hunks = Vec::with_capacity(file.hunks.len());
        let mut previous = [0usize; 2];
        let mut largest = 1usize;
        for (index, hunk) in file.hunks.iter().enumerate() {
            let starts = [
                hunk.old_start
                    .saturating_sub(usize::from(hunk.old_count > 0)),
                hunk.new_start
                    .saturating_sub(usize::from(hunk.new_count > 0)),
            ];
            let gaps = [
                starts[0].saturating_sub(previous[0]),
                starts[1].saturating_sub(previous[1]),
            ];
            if gaps != [0, 0] {
                rows.push(Row::Notice(label(format!(
                    "... omitted context: {} old / {} new lines ...",
                    gaps[0], gaps[1]
                ))));
            }
            let header = format!(
                "@@ -{},{} +{},{} @@{}{}",
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                if hunk.heading.is_empty() { "" } else { " " },
                hunk.heading
            );
            let row = rows.len();
            rows.push(Row::Notice(label(header)));
            let syntax = append_hunk(patch, hunk, index, languages, &mut rows);
            hunks.push(CachedHunk { row, syntax });
            previous = [
                starts[0].saturating_add(hunk.old_count),
                starts[1].saturating_add(hunk.new_count),
            ];
            for line in &hunk.lines {
                largest = largest
                    .max(line.old_number.unwrap_or(0))
                    .max(line.new_number.unwrap_or(0));
            }
        }
        if rows.is_empty() {
            rows.push(Row::Notice(label("Metadata-only file change".into())));
        }
        let width = rows
            .iter()
            .map(|row| match row {
                Row::Notice(line) => display_width(&line.text),
                Row::Source { sides, .. } => sides
                    .iter()
                    .flatten()
                    .map(|line| display_width(&line.text))
                    .max()
                    .unwrap_or(0),
            })
            .chain(titles.iter().map(|line| display_width(&line.text)))
            .max()
            .unwrap_or(0);
        Self {
            titles,
            rows,
            hunks,
            gutter: (largest.ilog10() + 3) as u16,
            width,
        }
    }
}

fn append_hunk(
    patch: &UnifiedPatch,
    hunk: &PatchHunk,
    index: usize,
    languages: [Option<Language>; 2],
    rows: &mut Vec<Row>,
) -> [Vec<HighlightSpan>; 2] {
    let mut sources = [String::new(), String::new()];
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for line in &hunk.lines {
        let value = patch.text(&line.text);
        match line.kind {
            PatchLineKind::Context => {
                flush_changes(&mut removed, &mut added, index, rows);
                let left = source_line(line.old_number, value, &mut sources[0]);
                let right = source_line(line.new_number, value, &mut sources[1]);
                rows.push(Row::Source {
                    sides: [left, right],
                    hunk: index,
                    change: None,
                });
                if line.no_newline {
                    rows.push(Row::Notice(label(
                        "\\ No newline at end of file (old and new)".into(),
                    )));
                }
            }
            PatchLineKind::Removed => {
                if !added.is_empty() {
                    flush_changes(&mut removed, &mut added, index, rows);
                }
                removed.push((
                    source_line(line.old_number, value, &mut sources[0]),
                    line.no_newline,
                ));
            }
            PatchLineKind::Added => added.push((
                source_line(line.new_number, value, &mut sources[1]),
                line.no_newline,
            )),
        }
    }
    flush_changes(&mut removed, &mut added, index, rows);
    std::array::from_fn(|side| {
        if languages[side].is_none() {
            return Vec::new();
        }
        TextSnapshot::from_bytes(sources[side].as_bytes())
            .ok()
            .and_then(|source| highlight(languages[side], &source).ok())
            .unwrap_or_default()
    })
}

fn source_line(number: Option<usize>, value: &str, source: &mut String) -> Option<ViewLine> {
    let number = number?;
    let start = source.len();
    source.push_str(value);
    source.push('\n');
    Some(ViewLine::new(number, value.to_owned(), start))
}

fn flush_changes(
    removed: &mut Vec<(Option<ViewLine>, bool)>,
    added: &mut Vec<(Option<ViewLine>, bool)>,
    hunk: usize,
    rows: &mut Vec<Row>,
) {
    let count = removed.len().max(added.len());
    let mut left = removed.drain(..);
    let mut right = added.drain(..);
    for _ in 0..count {
        let (mut old, old_no_newline) = left.next().unwrap_or((None, false));
        let (mut new, new_no_newline) = right.next().unwrap_or((None, false));
        let change = match (&old, &new) {
            (Some(_), Some(_)) => ChangeKind::Modified,
            (Some(_), None) => ChangeKind::Removed,
            _ => ChangeKind::Added,
        };
        if let (Some(old), Some(new)) = (&mut old, &mut new) {
            for span in intraline_spans(&old.text, &new.text) {
                old.changed.push(span.left);
                new.changed.push(span.right);
            }
            old.changed.sort_unstable_by_key(|span| span.start);
            new.changed.sort_unstable_by_key(|span| span.start);
        }
        rows.push(Row::Source {
            sides: [old, new],
            hunk,
            change: Some(change),
        });
        if old_no_newline || new_no_newline {
            let side = match (old_no_newline, new_no_newline) {
                (true, true) => "old and new",
                (true, false) => "old",
                _ => "new",
            };
            rows.push(Row::Notice(label(format!(
                "\\ No newline at end of file ({side})"
            ))));
        }
    }
}

fn label(value: String) -> ViewLine {
    let safe = if value.chars().any(char::is_control) {
        let mut safe = String::with_capacity(value.len());
        for character in value.chars() {
            if character.is_control() {
                safe.extend(character.escape_default());
            } else {
                safe.push(character);
            }
        }
        safe
    } else {
        value
    };
    ViewLine::new(0, safe, 0)
}

fn display_width(value: &str) -> usize {
    value.graphemes(true).fold(0, |width, grapheme| {
        width + text::display_cell_width(grapheme, width)
    })
}

fn paint_label(
    buffer: &mut Buffer,
    area: Rect,
    y: u16,
    line: &ViewLine,
    offset: usize,
    palette: &RenderPalette,
    style: Style,
) {
    let paint = crate::render::RegionPaint {
        normal: style,
        inline: style,
    };
    render_line(buffer, area, y, line, &[], offset, palette, paint);
}
