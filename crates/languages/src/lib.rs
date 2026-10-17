//! What language a file is in, and the grammar that parses it.
//!
//! One table, [`LANGUAGES`], with two halves of very different cost, which is
//! why it is a crate rather than part of the engine above it:
//!
//! - **the names** — always compiled. A diff row is labelled with
//!   [`lang_for_ext`] whether or not anything can highlight the file.
//! - **the grammars** — eighteen C libraries behind the `grammars` feature.
//!   They are the reason this cannot build for wasm32, so a consumer that only
//!   wants the name takes `default-features = false` and pays for none of them.
//!
//! Adding a language is one row in [`LANGUAGES`] and one optional dependency in
//! the `grammars` feature, and touches nothing above.

/// A language: what it is called, the extensions that mean it, and, with
/// `grammars`, how to parse and colour it.
pub struct Language {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    #[cfg(feature = "grammars")]
    pub grammar: fn() -> tree_sitter::Language,
    /// Joined in order. A grammar that ships only a delta over another's query
    /// lists the base first: TypeScript over JavaScript, C++ over C.
    #[cfg(feature = "grammars")]
    pub highlights: &'static [&'static str],
}

/// Every language. [`lang_for_ext`] reads the names off it and the engine the
/// grammars, so the two cannot disagree about what a `.mjs` is.
pub static LANGUAGES: &[Language] = &[
    // NB: the grammar crates are maddeningly inconsistent — some export
    // HIGHLIGHT_QUERY, some HIGHLIGHTS_QUERY; php exports LANGUAGE_PHP.
    Language {
        name: "rust",
        extensions: &["rs"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
    },
    // NB: tree-sitter-typescript ships only the *delta* over JavaScript —
    // types, parameters, and the TS-only keywords. Everything a `.ts` file
    // shares with JavaScript (keywords, strings, calls, comments) is in the
    // JavaScript queries, so both go in. TSX takes the same pair over the JSX
    // grammar, and JSX's tags and attributes besides.
    // NOTE: tree-sitter.json lists JavaScript's part last, for later-wins; the
    // engine keeps the first capture, so it goes first here.
    Language {
        name: "typescript",
        extensions: &["ts", "mts"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        #[cfg(feature = "grammars")]
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
    },
    Language {
        name: "tsx",
        extensions: &["tsx"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        #[cfg(feature = "grammars")]
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
    },
    Language {
        name: "python",
        extensions: &["py", "pyi"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_python::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "go",
        extensions: &["go"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_go::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_go::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "c",
        extensions: &["c", "h"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_c::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_c::HIGHLIGHT_QUERY],
    },
    // NB: like TypeScript, tree-sitter-cpp's query is a delta: comments,
    // numbers and most keywords come from C's.
    Language {
        name: "cpp",
        extensions: &["cc", "cpp", "cxx", "hpp", "hh"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_cpp::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[
            tree_sitter_c::HIGHLIGHT_QUERY,
            tree_sitter_cpp::HIGHLIGHT_QUERY,
        ],
    },
    Language {
        name: "java",
        extensions: &["java"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_java::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "c_sharp",
        extensions: &["cs"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_c_sharp::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_c_sharp::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "ruby",
        extensions: &["rb"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_ruby::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_ruby::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "php",
        extensions: &["php"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_php::LANGUAGE_PHP.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_php::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "html",
        extensions: &["html", "htm"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_html::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_html::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "css",
        extensions: &["css", "scss"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_css::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "scala",
        extensions: &["scala", "sc"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_scala::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_scala::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "haskell",
        extensions: &["hs"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_haskell::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_haskell::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "json",
        extensions: &["json"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_json::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
    },
    Language {
        name: "bash",
        extensions: &["sh", "bash", "zsh"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_bash::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY],
    },
    Language {
        name: "markdown",
        extensions: &["md", "markdown"],
        #[cfg(feature = "grammars")]
        grammar: || tree_sitter_md::LANGUAGE.into(),
        #[cfg(feature = "grammars")]
        highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
    },
];

/// The language an extension means, if the table has one.
#[must_use]
pub fn language(ext: &str) -> Option<&'static Language> {
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&ext))
}

/// The language an extension means, or `"plain"`.
///
/// Answers with or without `grammars` — a file we cannot colour still has a
/// name, and the perf panel and the diff's file headers want it.
#[must_use]
pub fn lang_for_ext(ext: &str) -> &'static str {
    language(ext).map_or("plain", |language| language.name)
}

/// `language`'s grammar, and its highlights joined in order and compiled.
///
/// # Errors
///
/// When the query does not compile against the grammar, which the tests rule
/// out for every row.
#[cfg(feature = "grammars")]
pub fn compile(
    language: &Language,
) -> Result<(tree_sitter::Language, tree_sitter::Query), tree_sitter::QueryError> {
    let grammar = (language.grammar)();
    let query = tree_sitter::Query::new(&grammar, &language.highlights.concat())?;
    Ok((grammar, query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_names_its_language_and_anything_else_is_plain() {
        assert_eq!(lang_for_ext("rs"), "rust");
        assert_eq!(lang_for_ext("mjs"), "javascript");
        assert_eq!(lang_for_ext("scss"), "css");
        assert_eq!(lang_for_ext("wat"), "plain");
        assert_eq!(lang_for_ext(""), "plain");
    }

    /// Two languages claiming one extension is a coin toss over which grammar a
    /// file gets, decided by table order — so it is refused here instead.
    #[test]
    fn no_extension_is_claimed_twice() {
        let mut seen = std::collections::HashSet::new();
        for language in LANGUAGES {
            for ext in language.extensions {
                assert!(
                    seen.insert(*ext),
                    "{ext} is claimed twice, once by {}",
                    language.name
                );
            }
        }
    }

    #[test]
    fn every_language_is_reachable_from_an_extension() {
        for language in LANGUAGES {
            assert!(
                language
                    .extensions
                    .iter()
                    .any(|ext| lang_for_ext(ext) == language.name),
                "nothing opens as {}",
                language.name
            );
        }
    }

    #[cfg(feature = "grammars")]
    fn queries() -> Vec<tree_sitter::Query> {
        LANGUAGES
            .iter()
            .map(|language| {
                compile(language)
                    .unwrap_or_else(|error| panic!("{}: {error}", language.name))
                    .1
            })
            .collect()
    }

    /// A broken row fails here rather than the first time a file in it is
    /// drawn.
    #[cfg(feature = "grammars")]
    #[test]
    fn every_highlights_query_compiles() {
        assert_eq!(queries().len(), LANGUAGES.len());
    }

    /// A grammar that speaks another dialect of capture names shows up here
    /// rather than as text nobody can colour.
    #[cfg(feature = "grammars")]
    #[test]
    fn every_capture_a_query_reports_has_a_colour() {
        // Left plain on purpose: `_` names only feed predicates, `embedded`
        // spans an interpolation whose parts have captures of their own, `none`
        // and `spell` are markers, and there is no colour for a label or a
        // module.
        const PLAIN: &[&str] = &[
            "_",
            "embedded",
            "label",
            "module",
            "namespace",
            "none",
            "spell",
        ];
        let queries = queries();
        let uncoloured: std::collections::BTreeSet<_> = queries
            .iter()
            .flat_map(tree_sitter::Query::capture_names)
            .filter(|name| !PLAIN.iter().any(|plain| name.starts_with(plain)))
            .filter(|name| concats_syntax::capture_to_hl(name).is_none())
            .collect();
        assert!(uncoloured.is_empty(), "no colour for {uncoloured:?}");
    }
}
