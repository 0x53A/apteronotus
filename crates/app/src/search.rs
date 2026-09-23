//! Literal document search. Ranges use editor character coordinates, never
//! UTF-8 byte offsets; changing the source invalidates the complete result set.
use std::ops::Range;

#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub focus: bool,
    /// The source caret when Find opened. Typing a longer query must not
    /// move its starting point to the match selected by a shorter prefix.
    pub anchor: usize,
    pub query: String,
    source: String,
    searched: String,
    matches: Vec<Match>,
    selected: Option<usize>,
}

struct Match {
    characters: Range<usize>,
    bytes: Range<usize>,
}

impl Search {
    pub fn refresh(&mut self, source: &str, cursor: usize) -> bool {
        if self.source == source && self.searched == self.query {
            return false;
        }
        self.source.clear();
        self.source.push_str(source);
        self.searched.clone_from(&self.query);
        self.matches.clear();
        if !self.query.is_empty() {
            let length = self.query.chars().count();
            let mut byte = 0;
            let mut character = 0;
            for (start, matched) in source.match_indices(&self.query) {
                character += source[byte..start].chars().count();
                self.matches.push(Match {
                    characters: character..character + length,
                    bytes: start..start + matched.len(),
                });
                byte = start + matched.len();
                character += length;
            }
        }
        self.select_from(cursor);
        true
    }

    pub fn select_from(&mut self, cursor: usize) {
        self.selected = if self.matches.is_empty() {
            None
        } else {
            Some(
                self.matches
                    .iter()
                    .position(|matched| matched.characters.start >= cursor)
                    .unwrap_or(0),
            )
        };
    }

    pub fn current(&self) -> Option<Range<usize>> {
        self.selected
            .map(|index| self.matches[index].characters.clone())
    }

    pub fn highlight(&self) -> Option<(&str, Range<usize>)> {
        self.selected
            .filter(|_| self.open)
            .map(|index| (self.source.as_str(), self.matches[index].bytes.clone()))
    }

    pub fn advance(&mut self, backwards: bool) -> Option<Range<usize>> {
        let current = self.selected?;
        let count = self.matches.len();
        self.selected = Some(if backwards {
            (current + count - 1) % count
        } else {
            (current + 1) % count
        });
        self.current()
    }

    pub fn label(&self) -> String {
        match self.selected {
            Some(index) => format!("{} / {}", index + 1, self.matches.len()),
            None if self.query.is_empty() => "Exact text · case sensitive".into(),
            None => "No matches".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_coordinates_wrap_in_both_directions() {
        let mut search = Search {
            query: "水🌊".into(),
            ..Default::default()
        };
        assert!(search.refresh("é 水🌊\r\n水🌊", 0));
        assert_eq!(search.current(), Some(2..4));
        assert_eq!(search.advance(false), Some(6..8));
        assert_eq!(search.advance(false), Some(2..4));
        assert_eq!(search.advance(true), Some(6..8));
        assert!(!search.refresh("é 水🌊\r\n水🌊", 0));
        assert_eq!(search.label(), "2 / 2");
    }

    #[test]
    fn edits_recompute_coordinates_and_empty_queries_never_match() {
        let mut search = Search {
            query: "aa".into(),
            ..Default::default()
        };
        search.refresh("aaaa aa", 3);
        assert_eq!(search.current(), Some(5..7));
        search.refresh("水 aa", 0);
        assert_eq!(search.current(), Some(2..4));
        search.query.clear();
        search.refresh("水 aa", 0);
        assert_eq!(search.current(), None);
        assert_eq!(search.advance(true), None);
    }

    #[test]
    fn matching_is_literal_case_sensitive_and_nonoverlapping() {
        let mut search = Search {
            query: "[A]?".into(),
            ..Default::default()
        };
        search.refresh("[a]? [A]?", 99);
        assert_eq!(search.current(), Some(5..9));
        search.query = "aa".into();
        search.refresh("aaa", 0);
        assert_eq!(search.label(), "1 / 1");
        search.query = "missing".into();
        search.refresh("aaa", 0);
        assert_eq!(search.label(), "No matches");
    }
}
