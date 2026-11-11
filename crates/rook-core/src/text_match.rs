//! Unicode literal matching with original UTF-8 coordinates and query-sized state.
use caseless::Caseless;
use std::{collections::VecDeque, ops::Range};

pub(crate) fn fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    text.chars().flat_map(char::to_lowercase).default_case_fold().collect()
}

pub(crate) struct Literal {
    needle: Vec<char>,
    fallback: Vec<usize>,
}

impl Literal {
    pub(crate) fn new(query: &str) -> Self {
        let needle: Vec<char> = fold(query).chars().collect();
        let mut fallback = vec![0; needle.len()];
        let mut matched = 0;
        for index in 1..needle.len() {
            while matched > 0 && needle[matched] != needle[index] {
                matched = fallback[matched - 1];
            }
            if needle[matched] == needle[index] {
                matched += 1;
            }
            fallback[index] = matched;
        }
        Self { needle, fallback }
    }

    pub(crate) fn overlap(&self) -> usize {
        self.needle.len().saturating_mul(4)
    }

    pub(crate) fn find(&self, data: &[u8]) -> Option<Range<usize>> {
        if self.needle.is_empty() {
            return None;
        }
        let mut matched = 0;
        let mut positions = VecDeque::with_capacity(self.needle.len());
        let mut base = 0;
        while base < data.len() {
            let (valid, skip) = match std::str::from_utf8(&data[base..]) {
                Ok(text) => (text, 0),
                Err(error) => (
                    std::str::from_utf8(&data[base..base + error.valid_up_to()])
                        .expect("valid_up_to identifies a valid UTF-8 prefix"),
                    error.error_len().unwrap_or(data.len() - base - error.valid_up_to()),
                ),
            };
            for (offset, character) in valid.char_indices() {
                if character.is_ascii() {
                    if let Some(found) = self.advance(
                        character.to_ascii_lowercase(),
                        base + offset,
                        base + offset + 1,
                        &mut matched,
                        &mut positions,
                    ) {
                        return Some(found);
                    }
                    continue;
                }
                for folded in character.to_lowercase().default_case_fold() {
                    if let Some(found) = self.advance(
                        folded,
                        base + offset,
                        base + offset + character.len_utf8(),
                        &mut matched,
                        &mut positions,
                    ) {
                        return Some(found);
                    }
                }
            }
            base += valid.len() + skip;
            // A partial code point at a scan boundary is not searchable text.
            // Do not join a match through invalid bytes or replacement glyphs.
            matched = 0;
            positions.clear();
        }
        None
    }

    fn advance(
        &self,
        folded: char,
        start: usize,
        end: usize,
        matched: &mut usize,
        positions: &mut VecDeque<usize>,
    ) -> Option<Range<usize>> {
        while *matched > 0 && self.needle[*matched] != folded {
            *matched = self.fallback[*matched - 1];
        }
        if positions.len() == self.needle.len() {
            positions.pop_front();
        }
        positions.push_back(start);
        if self.needle[*matched] == folded {
            *matched += 1;
        }
        (*matched == self.needle.len())
            .then(|| *positions.front().expect("a complete match has source positions")..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanded_folds_preserve_source_offsets_and_invalid_bytes_break_matches() {
        for (query, text, found) in [
            ("STRASSE", "🦀 Straße!", "Straße"),
            ("οσ", "ος", "ος"),
            ("ss", "ß", "ß"),
            ("s", "ß", "ß"),
            ("İ", "i\u{307}", "i\u{307}"),
            ("K!", "k!", "k!"),
            ("aaaaab", "aaaaaaaaab", "aaaaab"),
        ] {
            let range = Literal::new(query).find(text.as_bytes()).unwrap();
            assert_eq!(&text[range], found, "{query} in {text}");
        }
        assert!(Literal::new("ab").find(b"a\xffb").is_none());
        assert_eq!(Literal::new("ss").find(b"\xff\xc3\x9f"), Some(1..3));
        assert!(Literal::new("").find(b"anything").is_none());
        assert!(Literal::new("no").find(b"\xf0\x9f").is_none());
        assert!(Literal::new("İ").overlap() >= "i\u{307}".len());
    }
}
