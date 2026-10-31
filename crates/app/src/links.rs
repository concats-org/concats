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
            let relative = path.strip_prefix(root).ok()?;
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return None;
            }
            let (path, suffix) = split_location(relative.to_str()?);
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
}
