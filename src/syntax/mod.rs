//! Syntax colors for code shown outside an editor: the highlighter, its languages and the
//! [`CodeColors`] the markdown renderer uses.

mod highlight;
mod languages;

use gpui::App;

/// Compiles the grammars for `languages` (names or file extensions) on a background thread,
/// so opening the first file of each doesn't wait for its queries.
pub fn prewarm(languages: Vec<String>) {
    std::thread::spawn(move || highlight::prewarm(languages.iter().map(String::as_str)));
}

/// A code block's captures, worked out once and colored from the theme when drawn.
pub struct CodeColors {
    names: Vec<String>,
    runs: Vec<(std::ops::Range<usize>, u16)>,
}

impl CodeColors {
    /// Parses `code` in `language` (a name or file extension); `None` without a grammar.
    pub fn new(code: &str, language: &str) -> Option<Self> {
        let highlights = highlight::highlight(code, language)?;
        let mut at = 0;
        let runs = highlights
            .runs(0..code.len())
            .filter_map(|(len, id)| {
                let range = at..at + len;
                at += len;
                (id > 0).then_some((range, id))
            })
            .collect();
        Some(Self { names: highlights.names, runs })
    }

    /// Syntax colors for the code, for code shown outside an editor, such as Markdown code
    /// blocks.
    pub fn styles(&self, cx: &App) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
        self.styles_in(0..usize::MAX, &self.palette(cx))
    }

    /// The theme's style for each capture name, for [`Self::styles_in`].
    pub fn palette(&self, cx: &App) -> Vec<Option<gpui::HighlightStyle>> {
        let palette = cx.global::<crate::theme::Palette>();
        self.names.iter().map(|name| palette.syntax(name)).collect()
    }

    /// Syntax colors for `range` of the code, such as one of its lines, measured from the
    /// range's start; `palette` comes from [`Self::palette`].
    pub fn styles_in(
        &self,
        range: std::ops::Range<usize>,
        palette: &[Option<gpui::HighlightStyle>],
    ) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
        let first = self.runs.partition_point(|(run, _)| run.end <= range.start);
        self.runs[first..]
            .iter()
            .take_while(|(run, _)| run.start < range.end)
            .filter_map(|(run, id)| {
                let style = (*palette.get(*id as usize - 1)?)?;
                let (start, end) = (run.start.max(range.start), run.end.min(range.end));
                Some((start - range.start..end - range.start, style))
            })
            .collect()
    }
}

#[cfg(test)]
impl CodeColors {
    /// How many capture names the code uses, so tests can build a palette without a theme.
    pub fn palette_len(&self) -> usize {
        self.names.len()
    }
}

/// Whether text in `language` (a name or file extension) gets colors, for tests outside.
#[cfg(test)]
pub fn highlights(text: &str, language: &str) -> bool {
    highlight::highlight(text, language).is_some_and(|h| !h.names.is_empty())
}
