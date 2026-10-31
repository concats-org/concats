//! Link policy shared by desktop surfaces. File targets stay in the review.

use std::path::{Component, Path};

#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    /// A path inside the checkout, and the 1-based line and column the link
    /// names, if any.
    File {
        path: String,
        at: Option<(u32, u32)>,
    },
    External(url::Url),
}

pub(crate) fn target(uri: &str, root: &Path) -> Option<Target> {
    let url = url::Url::parse(uri).ok()?;
    match url.scheme() {
        "http" | "https" | "mailto" => Some(Target::External(url)),
        "file" if url.host_str().is_none_or(|host| host == "localhost") => {
            let path = url.to_file_path().ok()?;
            let relative = checkout_path(path.strip_prefix(root).ok()?)?;
            let (path, suffix) = split_location(&relative);
            let fragment = url
                .fragment()
                .and_then(concats_review::guide::fragment_lines)
                .map(|(line, _)| (line, 1));
            Some(Target::File {
                path: path.to_owned(),
                at: fragment.or(suffix),
            })
        }
        _ => None,
    }
}

/// The file a run of plain terminal text names: a `file://` URL, or a path
/// inside `root`, either one with a `:line[:column]` or a `#L…`. Only a file
/// `is_file` finds counts, so `12:30`, or a word that merely looks like a
/// path, is not a link. Sentence punctuation after it is not part of it.
pub(crate) fn file_in_text(
    text: &str,
    root: &Path,
    is_file: impl Fn(&Path) -> bool,
) -> Option<Target> {
    let text = text.trim_end_matches(['.', ',', ':', ';']);
    let target = if text.starts_with("file://") {
        target(text, root)?
    } else {
        let (text, fragment) = text
            .split_once('#')
            .map_or((text, None), |(t, f)| (t, Some(f)));
        let (path, suffix) = split_location(text);
        let path = Path::new(path);
        let fragment = fragment
            .and_then(concats_review::guide::fragment_lines)
            .map(|(line, _)| (line, 1));
        Target::File {
            path: checkout_path(path.strip_prefix(root).unwrap_or(path))?,
            at: fragment.or(suffix),
        }
    };
    match &target {
        Target::File { path, .. } if is_file(&root.join(path)) => Some(target),
        _ => None,
    }
}

/// `relative` as a path in the checkout, or `None` when it is empty or steps
/// outside it (`..`, a root).
fn checkout_path(relative: &Path) -> Option<String> {
    let parts = relative
        .components()
        .filter(|part| *part != Component::CurDir)
        .map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `path:line` or `path:line:column`, the way compilers and `rg -n` print a
/// location, split into the path and where in it.
pub(crate) fn split_location(text: &str) -> (&str, Option<(u32, u32)>) {
    let number = |s: &str| s.parse::<u32>().ok().filter(|n| *n > 0);
    let Some((rest, last)) = text.rsplit_once(':') else {
        return (text, None);
    };
    let Some(last) = number(last) else {
        return (text, None);
    };
    match rest.rsplit_once(':') {
        Some((path, line)) if let Some(line) = number(line) => (path, Some((line, last))),
        _ => (rest, Some((last, 1))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_only_dispatch_web_and_mail_handlers() {
        let root = Path::new("/repo");
        for uri in [
            "-a Terminal",
            "ssh://host",
            "smb://host/share",
            "vnc://host",
            "javascript:alert(1)",
            "data:text/plain,test",
        ] {
            assert!(target(uri, root).is_none(), "{uri}");
        }
        for uri in [
            "http://example.com",
            "https://example.com/a?b=c",
            "mailto:someone@example.com",
        ] {
            assert!(
                matches!(target(uri, root), Some(Target::External(_))),
                "{uri}"
            );
        }
    }

    #[test]
    fn local_file_links_name_only_files_inside_the_repository() {
        let root = Path::new("/repo");
        let file = |path: &str, at| {
            Some(Target::File {
                path: path.into(),
                at,
            })
        };
        assert_eq!(target("file:///repo/a%20b.rs", root), file("a b.rs", None));
        assert_eq!(
            target("file://localhost/repo/src/a.rs", root),
            file("src/a.rs", None)
        );
        for uri in [
            "file://remote/repo/a",
            "file:///elsewhere/a",
            "file:///repo-other/a",
            "file:///repo/../outside",
            "file:///repo",
            "file:///repo/%FF",
        ] {
            assert!(target(uri, root).is_none(), "{uri}");
        }
    }

    #[test]
    fn file_links_carry_the_line_they_name() {
        let root = Path::new("/repo");
        let at = |uri| match target(uri, root) {
            Some(Target::File { path, at }) => Some((path, at)),
            _ => None,
        };
        let a = |line, column| Some(("a.rs".to_string(), Some((line, column))));
        assert_eq!(at("file:///repo/a.rs#L12"), a(12, 1));
        assert_eq!(at("file:///repo/a.rs#L12-L20"), a(12, 1));
        assert_eq!(at("file:///repo/a.rs#L12-20"), a(12, 1));
        assert_eq!(at("file:///repo/a.rs:12"), a(12, 1));
        assert_eq!(at("file:///repo/a.rs:12:3"), a(12, 3));
        assert_eq!(at("file:///elsewhere/a.rs:12"), None);
    }

    #[test]
    fn a_location_suffix_splits_off_its_path() {
        assert_eq!(split_location("src/a.rs:12:5"), ("src/a.rs", Some((12, 5))));
        assert_eq!(split_location("src/a.rs:12"), ("src/a.rs", Some((12, 1))));
        assert_eq!(split_location("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_location("src/a.rs:"), ("src/a.rs:", None));
        assert_eq!(split_location("src/a.rs:0"), ("src/a.rs:0", None));
    }

    #[test]
    fn plain_text_names_a_file_only_when_it_exists_in_the_checkout() {
        let root = Path::new("/repo");
        let is_file = |path: &Path| path == Path::new("/repo/src/a.rs");
        let cases = [
            ("src/a.rs:12:5", Some((12, 5))),
            ("./src/a.rs:12", Some((12, 1))),
            ("src/a.rs#L12", Some((12, 1))),
            ("src/a.rs:12:", Some((12, 1))),
            ("file:///repo/src/a.rs", None),
            ("/repo/src/a.rs", None),
            ("src/a.rs", None),
            // The end of a sentence.
            ("src/a.rs.", None),
        ];
        for (text, at) in cases {
            assert_eq!(
                file_in_text(text, root, is_file),
                Some(Target::File {
                    path: "src/a.rs".into(),
                    at
                }),
                "{text}"
            );
        }
        for text in [
            "src/b.rs",
            "../repo/src/a.rs",
            "/elsewhere/src/a.rs",
            "12:30",
            "",
        ] {
            assert_eq!(file_in_text(text, root, is_file), None, "{text}");
        }
    }
}
