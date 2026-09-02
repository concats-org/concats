//! What language a file is in, and the grammar that parses it.
//!
//! Two halves with very different costs, which is why they are one crate rather
//! than part of the engine above them:
//!
//! - **the name** — [`lang_for_ext`], a table, always compiled. A diff row is
//!   labelled with it whether or not anything can highlight the file.
//! - **the grammars** — eighteen C libraries behind [`registry`], one Cargo
//!   feature each. They are the reason this cannot build for wasm32, so a
//!   consumer that only wants the name takes `default-features = false` and
//!   pays for none of them.
//!
//! Adding a language is one entry in [`EXTENSIONS`], one `insert` in
//! [`registry`], and one feature — and touches nothing above.

/// Every language, and the extensions that mean it. One table: [`lang_for_ext`]
/// reads the names off it and [`registry`] takes each grammar's extensions from
/// it, so the two cannot disagree about what a `.mjs` is.
pub const EXTENSIONS: &[(&str, &[&str])] = &[
    ("rust", &["rs"]),
    ("javascript", &["js", "jsx", "mjs", "cjs"]),
    ("typescript", &["ts", "mts"]),
    ("tsx", &["tsx"]),
    ("python", &["py", "pyi"]),
    ("go", &["go"]),
    ("c", &["c", "h"]),
    ("cpp", &["cc", "cpp", "cxx", "hpp", "hh"]),
    ("java", &["java"]),
    ("c_sharp", &["cs"]),
    ("ruby", &["rb"]),
    ("php", &["php"]),
    ("html", &["html", "htm"]),
    ("css", &["css", "scss"]),
    ("scala", &["scala", "sc"]),
    ("haskell", &["hs"]),
    ("json", &["json"]),
    ("bash", &["sh", "bash", "zsh"]),
    ("markdown", &["md", "markdown"]),
];

/// The language an extension means, or `"plain"`.
///
/// Answers for every language in the table, including ones no grammar feature
/// built — a file we cannot colour still has a name, and the perf panel and the
/// diff's file headers want it.
#[must_use]
pub fn lang_for_ext(ext: &str) -> &'static str {
    EXTENSIONS
        .iter()
        .find(|(_, exts)| exts.contains(&ext))
        .map_or("plain", |(name, _)| *name)
}

#[cfg(feature = "grammars")]
mod grammars {
    use std::collections::HashMap;

    use concats_syntax::CAPTURES;
    use tree_sitter_highlight::HighlightConfiguration;

    /// Every built grammar, by file extension, configured to report
    /// [`CAPTURES`].
    ///
    /// One entry per extension rather than per language, because that is how a
    /// caller asks: it has a filename, not a language.
    // One `add` per language, so the list reads as the table it is; clippy's
    // line count is measuring the table rather than any logic.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn registry() -> HashMap<&'static str, HighlightConfiguration> {
        let mut langs = HashMap::new();

        // NB: the grammar crates are maddeningly inconsistent — some export
        // HIGHLIGHT_QUERY, some HIGHLIGHTS_QUERY; php exports LANGUAGE_PHP; and
        // the query consts are #[cfg]-gated on the .scm file actually shipping,
        // so an absent injections.scm means the const does not exist at all.
        let mut add =
            |name: &'static str, lang: tree_sitter::Language, hl: &str, inj: &str, loc: &str| {
                // The extensions come off the shared table rather than being
                // repeated here, so a grammar is reachable by exactly the
                // extensions `lang_for_ext` names it for.
                let exts = super::EXTENSIONS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map_or(&[][..], |(_, exts)| *exts);
                for ext in exts {
                    if let Ok(mut cfg) =
                        HighlightConfiguration::new(lang.clone(), name, hl, inj, loc)
                    {
                        cfg.configure(CAPTURES);
                        langs.insert(*ext, cfg);
                    }
                }
            };

        #[cfg(feature = "rust")]
        add(
            "rust",
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        );
        #[cfg(feature = "javascript")]
        add(
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        );
        // NB: tree-sitter-typescript ships only the *delta* over JavaScript —
        // types, parameters, and the TS-only keywords. Everything a `.ts` file
        // shares with JavaScript (keywords, strings, calls, comments) is in the
        // JavaScript queries, so the two concatenate or a `.ts` file comes out
        // all but uncoloured. TSX is the same queries over the JSX grammar.
        #[cfg(feature = "typescript")]
        {
            let hl = [
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ]
            .concat();
            let loc = [
                tree_sitter_javascript::LOCALS_QUERY,
                tree_sitter_typescript::LOCALS_QUERY,
            ]
            .concat();
            add(
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &hl,
                tree_sitter_javascript::INJECTIONS_QUERY,
                &loc,
            );
            add(
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &hl,
                tree_sitter_javascript::INJECTIONS_QUERY,
                &loc,
            );
        }
        #[cfg(feature = "python")]
        add(
            "python",
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "go")]
        add(
            "go",
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "c")]
        add(
            "c",
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        );
        #[cfg(feature = "cpp")]
        add(
            "cpp",
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::HIGHLIGHT_QUERY,
            "",
            "",
        );
        #[cfg(feature = "java")]
        add(
            "java",
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "c-sharp")]
        add(
            "c_sharp",
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "ruby")]
        add(
            "ruby",
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY,
        );
        #[cfg(feature = "php")]
        add(
            "php",
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_php::HIGHLIGHTS_QUERY,
            tree_sitter_php::INJECTIONS_QUERY,
            "",
        );
        #[cfg(feature = "html")]
        add(
            "html",
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        );
        #[cfg(feature = "css")]
        add(
            "css",
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "scala")]
        add(
            "scala",
            tree_sitter_scala::LANGUAGE.into(),
            tree_sitter_scala::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_scala::LOCALS_QUERY,
        );
        #[cfg(feature = "haskell")]
        add(
            "haskell",
            tree_sitter_haskell::LANGUAGE.into(),
            tree_sitter_haskell::HIGHLIGHTS_QUERY,
            tree_sitter_haskell::INJECTIONS_QUERY,
            tree_sitter_haskell::LOCALS_QUERY,
        );
        #[cfg(feature = "json")]
        add(
            "json",
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        );
        #[cfg(feature = "bash")]
        add(
            "bash",
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        );
        #[cfg(feature = "markdown")]
        add(
            "markdown",
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            "",
        );

        langs
    }
}

#[cfg(feature = "grammars")]
pub use grammars::registry;

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
        for (name, exts) in EXTENSIONS {
            for ext in *exts {
                assert!(seen.insert(*ext), "{ext} is claimed twice, once by {name}");
            }
        }
    }

    #[cfg(feature = "grammars")]
    #[test]
    fn every_built_grammar_is_reachable_from_an_extension() {
        let langs = registry();
        assert!(!langs.is_empty(), "the default build has grammars");
        for ext in langs.keys() {
            assert_ne!(
                lang_for_ext(ext),
                "plain",
                "{ext} has a grammar but no name"
            );
        }
    }
}
