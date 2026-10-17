//! The syntax vocabulary: what a highlighter says and what a theme colours.
//!
//! This is the contract between themes and highlighting, and it is a crate
//! because both sides must be able to hold it without holding each other. A
//! theme loads and tests with no grammar and no renderer; an engine compiles
//! and tests with no palette. Zed's JSON and Helix's TOML both key their
//! colours on tree-sitter capture names, so a shared vocabulary is what lets a
//! theme be written without knowing which engine renders it.
//!
//! Small on purpose. Anything that needs a dependency belongs a layer up.

/// One styled run inside a line: `[start, end)` byte columns + a theme key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub hl: Option<Hl>,
}

/// The theme vocabulary — tree-sitter/Zed capture names, not makepad's 14 kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hl {
    Attribute,
    Comment,
    Constant,
    Function,
    Keyword,
    Number,
    Operator,
    Property,
    Punctuation,
    String,
    Type,
    Variable,
    Parameter,
}

/// A file's spans, by 0-based line.
pub type LineSpans = Vec<Vec<Span>>;

/// Capture-name prefix to colour, in precedence order: the first prefix a name
/// starts with wins, so `variable.parameter` sits ahead of `variable`.
///
/// The classic tree-sitter names come first, then the dialects the bundled
/// grammars also speak: Scala's and Haskell's nvim names, C's `delimiter`, and
/// the bare `escape` of Rust, Python, Go, Ruby and JSON. Last, so a theme that
/// colours both `constant` and its alias `boolean` takes `constant`.
const PREFIXES: &[(&str, Hl)] = &[
    ("attribute", Hl::Attribute),
    ("comment", Hl::Comment),
    ("constructor", Hl::Type),
    ("tag", Hl::Type),
    ("constant", Hl::Constant),
    ("function", Hl::Function),
    ("keyword", Hl::Keyword),
    ("number", Hl::Number),
    ("operator", Hl::Operator),
    ("property", Hl::Property),
    ("punctuation", Hl::Punctuation),
    ("string", Hl::String),
    // Markdown: headings read as the function colour, inline code and links as
    // strings, reference labels as properties.
    ("text.title", Hl::Function),
    ("text.literal", Hl::String),
    ("text.uri", Hl::String),
    ("text.reference", Hl::Property),
    ("type", Hl::Type),
    ("variable.parameter", Hl::Parameter),
    ("variable", Hl::Variable),
    ("method", Hl::Function),
    ("conditional", Hl::Keyword),
    ("repeat", Hl::Keyword),
    ("include", Hl::Keyword),
    ("exception", Hl::Keyword),
    ("storageclass", Hl::Keyword),
    ("boolean", Hl::Constant),
    ("float", Hl::Number),
    ("character", Hl::String),
    ("escape", Hl::String),
    ("parameter", Hl::Parameter),
    ("delimiter", Hl::Punctuation),
];

/// Map a tree-sitter / Zed capture name onto the [`Hl`] vocabulary.
///
/// Prefix matching, so a grammar that reports `keyword.control.return` lands on
/// the same colour as one that reports `keyword`. The vocabulary is coarser
/// than any one grammar's capture set on purpose.
#[must_use]
pub fn capture_to_hl(name: &str) -> Option<Hl> {
    capture_row(name).map(|(_, hl)| hl)
}

/// The vocabulary row a capture name lands on, with its colour. A theme breaks
/// ties between two names of one colour by row, so a classic name wins over
/// its dialect alias.
#[must_use]
pub fn capture_row(name: &str) -> Option<(usize, Hl)> {
    PREFIXES
        .iter()
        .position(|(prefix, _)| name.starts_with(prefix))
        .map(|row| (row, PREFIXES[row].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grammars_finer_capture_lands_on_the_coarser_colour() {
        assert_eq!(capture_to_hl("keyword.control.return"), Some(Hl::Keyword));
        assert_eq!(capture_to_hl("string.special.path"), Some(Hl::String));
        // …except where the finer name is its own colour, which must win over
        // the prefix it shares.
        assert_eq!(capture_to_hl("variable.parameter"), Some(Hl::Parameter));
        assert_eq!(capture_to_hl("variable.builtin"), Some(Hl::Variable));
        assert_eq!(capture_to_hl("nonesuch"), None);
    }
}
