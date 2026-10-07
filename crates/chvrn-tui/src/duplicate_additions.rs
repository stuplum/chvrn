use std::collections::{HashMap, HashSet};

use chvrn_core::{
    TextSnapshot,
    diff::{Diff, WhitespacePolicy},
};

use crate::{Pane, ReviewSession, session::Mode, text};

pub(crate) struct AdditionLink {
    pub(crate) text: String,
    pub(crate) ours: usize,
    pub(crate) theirs: usize,
    pub(crate) result: Vec<usize>,
}

#[derive(Default)]
pub(crate) struct DuplicateAdditions {
    pub(crate) links: Vec<AdditionLink>,
    pub(crate) review: Option<(usize, usize)>,
}

impl DuplicateAdditions {
    pub(crate) fn new(base: &TextSnapshot, ours: &TextSnapshot, theirs: &TextSnapshot) -> Self {
        let existing: HashSet<_> = line_contents(base.text()).collect();
        let additions = |source: &TextSnapshot| {
            let lines: Vec<_> = line_contents(source.text()).collect();
            let mut added = HashMap::new();
            for hunk in Diff::between(base, source, WhitespacePolicy::Exact).hunks() {
                for line in hunk.right_lines.clone() {
                    let content = lines[line];
                    if !content.trim().is_empty() && !existing.contains(content) {
                        added
                            .entry(content.to_owned())
                            .or_insert_with(Vec::new)
                            .push((line, hunk.left_lines.start));
                    }
                }
            }
            added
        };
        let ours = additions(ours);
        let theirs = additions(theirs);
        let mut links = Vec::new();
        for (text, locations) in ours {
            let Some(other) = theirs.get(&text) else {
                continue;
            };
            if let ([(ours, first)], [(theirs, second)]) = (locations.as_slice(), other.as_slice())
            {
                if first != second {
                    links.push(AdditionLink {
                        text,
                        ours: *ours,
                        theirs: *theirs,
                        result: Vec::new(),
                    });
                }
            }
        }
        links.sort_unstable_by_key(|link| link.ours);
        Self {
            links,
            review: None,
        }
    }

    pub(crate) fn refresh(&mut self, result: &str) {
        self.review = None;
        let mut indices: HashMap<&str, &mut Vec<usize>> = self
            .links
            .iter_mut()
            .map(|link| {
                link.result.clear();
                (link.text.as_str(), &mut link.result)
            })
            .collect();
        for (line, content) in line_contents(result).enumerate() {
            if let Some(positions) = indices.get_mut(content) {
                positions.push(line);
            }
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.links
            .iter()
            .filter(|link| link.result.len() > 1)
            .count()
    }
}

fn line_contents(source: &str) -> impl Iterator<Item = &str> {
    text::lines(source)
        .into_iter()
        .map(|span| &source[span.start_byte..span.content_end_byte])
}

impl ReviewSession {
    pub(crate) fn refresh_duplicate_additions(&mut self) {
        if let Mode::ThreeWay { result, .. } = &self.mode {
            self.duplicate_additions.refresh(result.snapshot.text());
        }
    }

    pub(crate) fn linked_addition(&self, pane: Pane, line: usize) -> Option<usize> {
        self.duplicate_additions
            .links
            .iter()
            .position(|link| match pane {
                Pane::Ours => link.ours == line,
                Pane::Theirs => link.theirs == line,
                Pane::Result => link.result.contains(&line),
                _ => false,
            })
    }

    pub(crate) fn active_addition(&self) -> Option<usize> {
        if self.local_pending {
            return None;
        }
        self.duplicate_additions
            .review
            .map(|(link, _)| link)
            .or_else(|| {
                let (line, _) = self.position_for_row(self.focus, self.aligned_row);
                self.linked_addition(self.focus, line)
            })
    }

    pub(crate) fn review_duplicate_addition(&mut self) {
        if self.local_pending {
            return;
        }
        let links = &self.duplicate_additions.links;
        let selected = self
            .active_addition()
            .filter(|&index| links[index].result.len() > 1)
            .or_else(|| links.iter().position(|link| link.result.len() > 1));
        if let Some(index) = selected {
            let line = links[index].result[0];
            self.go_to(Pane::Result, line, 0);
            self.duplicate_additions.review = Some((index, 0));
        }
    }
}
