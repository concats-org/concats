//! What a find matches: the query under the bar's toggles (match case, whole
//! word, regex) as one pattern, so the card views, the file browser and the
//! terminal agree on what a query finds.

use regex::Regex;

/// The bar's toggles. All off finds the query as literal text, in any case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

/// A whole word for the `regex` crate: bounded by Unicode word characters.
const WORD: &str = r"\b";

/// A whole word for the terminal: its search is alacritty's lazy DFA, which
/// refuses a Unicode `\b` when it builds, so it bounds by ASCII word bytes.
pub(crate) const ASCII_WORD: &str = r"(?-u:\b)";

/// The pattern `query` stands for, in the syntax the `regex` crate and the
/// terminal's search share, with `word` around a whole word. A regex query is
/// checked on its own first: wrapped, a stray `)` could pair up into a pattern
/// nobody typed.
pub(crate) fn pattern(query: &str, options: Options, word: &str) -> Result<String, regex::Error> {
    let body = if options.regex {
        Regex::new(query)?;
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let body = if options.word {
        format!("{word}(?:{body}){word}")
    } else {
        body
    };
    // NOTE: the case flag is always spelled out: the terminal's search turns
    // case-sensitive by itself whenever a pattern holds an uppercase letter.
    let case = if options.case { "-i" } else { "i" };
    Ok(format!("(?{case}:{body})"))
}

/// An open find: what was typed, the toggles, and the regex they make. Built
/// together, so the regex is always this query's.
#[derive(Clone, Default)]
pub(crate) struct Find {
    query: String,
    options: Options,
    /// `None` while nothing is typed; an error while the query is no regex.
    regex: Option<Result<Regex, regex::Error>>,
}

impl Find {
    pub(crate) fn new(query: String, options: Options) -> Self {
        let regex = (!query.is_empty())
            .then(|| pattern(&query, options, WORD).and_then(|pattern| Regex::new(&pattern)));
        Self {
            query,
            options,
            regex,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn options(&self) -> Options {
        self.options
    }

    /// Every match in `text`, as byte ranges in order. An empty match is not a
    /// find result, so `a*` does not hit between every pair of characters.
    pub(crate) fn hits(&self, text: &str) -> Vec<(usize, usize)> {
        let Some(Ok(regex)) = &self.regex else {
            return Vec::new();
        };
        regex
            .find_iter(text)
            .filter(|hit| !hit.is_empty())
            .map(|hit| (hit.start(), hit.end()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "Find finder find_all (find).";

    fn hits(query: &str, options: Options) -> Vec<(usize, usize)> {
        Find::new(query.to_owned(), options).hits(TEXT)
    }

    #[test]
    fn all_off_finds_literal_text_in_any_case() {
        let off = Options::default();
        assert_eq!(hits("find", off), [(0, 4), (5, 9), (12, 16), (22, 26)]);
        assert_eq!(hits("(", off), [(21, 22)], "a regex character is just text");
        assert_eq!(hits("", off), []);
        let overlapping = Find::new("aa".to_owned(), off);
        assert_eq!(overlapping.hits("aaa"), [(0, 2)], "hits do not overlap");
        let unicode = Find::new("ÉDITOR".to_owned(), off);
        assert_eq!(unicode.hits("src/éditor.rs"), [(4, 11)]);
    }

    #[test]
    fn match_case_keeps_only_the_typed_case() {
        let case = Options {
            case: true,
            ..Options::default()
        };
        assert_eq!(hits("find", case), [(5, 9), (12, 16), (22, 26)]);
        assert_eq!(hits("Find", case), [(0, 4)]);
    }

    #[test]
    fn whole_word_skips_matches_inside_longer_words() {
        let word = Options {
            word: true,
            ..Options::default()
        };
        assert_eq!(hits("find", word), [(0, 4), (22, 26)]);
        let german = Find::new("ber".to_owned(), word);
        assert_eq!(german.hits("über ber"), [(6, 9)], "ü is a word character");
    }

    #[test]
    fn regex_matches_a_pattern() {
        let regex = Options {
            regex: true,
            ..Options::default()
        };
        assert_eq!(
            hits(r"find\w*", regex),
            [(0, 4), (5, 11), (12, 20), (22, 26)]
        );
        assert_eq!(hits(r"find\w*", Options::default()), []);
        assert_eq!(hits("x*", regex), [], "empty matches are not hits");
        assert_eq!(
            hits("(", regex),
            [],
            "a regex that does not parse finds nothing"
        );
    }

    #[test]
    fn a_regex_is_checked_before_it_is_wrapped() {
        let regex = Options {
            regex: true,
            ..Options::default()
        };
        assert!(pattern("a)|(b", regex, WORD).is_err());
        assert_eq!(
            pattern(
                "a|b",
                Options {
                    word: true,
                    ..regex
                },
                ASCII_WORD
            )
            .unwrap(),
            r"(?i:(?-u:\b)(?:a|b)(?-u:\b))"
        );
    }
}
