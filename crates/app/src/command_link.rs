//! The `concats` command the app ships beside itself, put on the PATH as a
//! link into the bundle, so the command and the app it talks to can't drift
//! apart.

use std::path::{Path, PathBuf};

/// Where the command goes: on the PATH macOS gives every shell.
const LINK: &str = "/usr/local/bin/concats";

/// What sits where the link goes.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Nothing,
    Link(PathBuf),
    File,
}

/// What installing does about what it found.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Link,
    Done,
    Refuse,
}

/// A link to anything is ours to point at the bundle; a real file is someone
/// else's command, and stays.
fn step(found: &Found, bundled: &Path) -> Step {
    match found {
        Found::Link(to) if to == bundled => Step::Done,
        Found::Nothing | Found::Link(_) => Step::Link,
        Found::File => Step::Refuse,
    }
}

/// Link the bundled command onto the PATH, asking for an administrator's
/// password when the directory is not the user's to write. Returns what to
/// tell them.
pub(crate) fn install() -> String {
    let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|app| Some(app.parent()?.join("concats")))
        .filter(|path| path.is_file())
    else {
        return "this build has no concats command beside it; package the app".into();
    };
    let link = Path::new(LINK);
    let found = match std::fs::symlink_metadata(link) {
        Err(_) => Found::Nothing,
        Ok(meta) if meta.file_type().is_symlink() => {
            Found::Link(std::fs::read_link(link).unwrap_or_default())
        }
        Ok(_) => Found::File,
    };
    match step(&found, &bundled) {
        Step::Done => format!("{LINK} already runs this app's concats"),
        Step::Refuse => format!("{LINK} is another program; remove it to install"),
        Step::Link => match relink(&bundled, link) {
            Ok(()) => format!("installed {LINK}"),
            Err(error) => format!("cannot install {LINK}: {error}"),
        },
    }
}

/// Point `link` at `bundled`: directly when the user may, otherwise through
/// an administrator prompt.
fn relink(bundled: &Path, link: &Path) -> Result<(), String> {
    let direct = std::fs::remove_file(link)
        .or_else(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(error),
        })
        .and_then(|()| std::os::unix::fs::symlink(bundled, link));
    match direct {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            as_admin(bundled, link)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// `ln -sfn` run as an administrator, macOS asking for the password.
fn as_admin(bundled: &Path, link: &Path) -> Result<(), String> {
    let quote = |path: &Path| {
        shlex::try_quote(&path.to_string_lossy())
            .map(std::borrow::Cow::into_owned)
            .map_err(|error| error.to_string())
    };
    let dir = quote(link.parent().unwrap_or(Path::new("/")))?;
    let shell = format!(
        "mkdir -p {dir} && ln -sfn {} {}",
        quote(bundled)?,
        quote(link)?
    );
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        shell.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let status = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| error.to_string())?;
    if status.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&status.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installing_links_once_and_leaves_other_programs_alone() {
        let bundled = Path::new("/Applications/Concats.app/Contents/MacOS/concats");
        assert_eq!(step(&Found::Nothing, bundled), Step::Link);
        assert_eq!(step(&Found::Link(bundled.into()), bundled), Step::Done);
        assert_eq!(
            step(&Found::Link("/old/Concats.app/concats".into()), bundled),
            Step::Link
        );
        assert_eq!(step(&Found::File, bundled), Step::Refuse);
    }
}
