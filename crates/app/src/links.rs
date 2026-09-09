//! Link policy shared by desktop surfaces. File targets stay in the review.

use std::path::{Component, Path};

#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    File(String),
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
            Some(Target::File(relative.to_str()?.to_owned()))
        }
        _ => None,
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
        assert_eq!(
            target("file:///repo/a%20b.rs", root),
            Some(Target::File("a b.rs".into()))
        );
        assert_eq!(
            target("file://localhost/repo/src/a.rs", root),
            Some(Target::File("src/a.rs".into()))
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
}
