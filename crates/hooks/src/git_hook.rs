//! Install and manage git hooks that concats writes into `.git/hooks/`.
//!
//! Distinct from agent hooks (which live in agent-specific settings files),
//! git hooks run on ordinary git operations. Today `post-rewrite` keeps session
//! refs in sync after rebases/amends, and `post-commit` re-anchors session
//! turns onto materialized commits.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use concats_core::error::Result;

const MARKER: &str = "# concats: managed";

/// A git hook concats can manage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// `post-rewrite` — invokes `concats rewrite`.
    PostRewrite,
    /// `post-commit` — invokes `concats commit-link`.
    PostCommit,
}

impl Hook {
    fn file_name(self) -> &'static str {
        match self {
            Hook::PostRewrite => "post-rewrite",
            Hook::PostCommit => "post-commit",
        }
    }

    fn subcommand(self) -> &'static str {
        match self {
            Hook::PostRewrite => "rewrite",
            Hook::PostCommit => "commit-link",
        }
    }
}

/// Install status for a managed git hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    /// No hook file exists.
    Missing,
    /// A concats-managed hook is installed.
    Managed,
    /// A hook exists but is not managed by concats.
    Foreign,
}

/// Install a git hook that invokes the concats binary.
///
/// Writes a small shell script that invokes `<binary> <subcommand> "$@"`.
/// Existing concats-managed hooks are overwritten in place; foreign hooks are
/// left alone and the caller is expected to surface a warning.
///
/// # Errors
///
/// Returns an error if the hooks directory cannot be created, the hook file
/// cannot be written, or its permissions cannot be set.
pub fn install(gitdir: &Path, hook: Hook, binary: &Path) -> Result<HookStatus> {
    let hooks_dir = hooks_dir(gitdir);
    fs::create_dir_all(&hooks_dir)?;
    let path = hooks_dir.join(hook.file_name());

    match status_at(&path) {
        HookStatus::Foreign => return Ok(HookStatus::Foreign),
        HookStatus::Missing | HookStatus::Managed => {}
    }

    let script = render_script(hook, binary);
    write_executable(&path, &script)?;
    Ok(HookStatus::Managed)
}

/// Remove a concats-managed git hook. Foreign hooks are left untouched.
///
/// # Errors
///
/// Returns an error if the hook file cannot be removed.
pub fn uninstall(gitdir: &Path, hook: Hook) -> Result<HookStatus> {
    let path = hooks_dir(gitdir).join(hook.file_name());
    match status_at(&path) {
        HookStatus::Managed => {
            fs::remove_file(&path)?;
            Ok(HookStatus::Missing)
        }
        status @ (HookStatus::Foreign | HookStatus::Missing) => Ok(status),
    }
}

/// Report whether a concats-managed hook is present in the given git directory.
#[must_use]
pub fn status(gitdir: &Path, hook: Hook) -> HookStatus {
    status_at(&hooks_dir(gitdir).join(hook.file_name()))
}

fn hooks_dir(gitdir: &Path) -> PathBuf {
    gitdir.join("hooks")
}

fn status_at(path: &Path) -> HookStatus {
    let Ok(contents) = fs::read_to_string(path) else {
        return HookStatus::Missing;
    };
    if contents.contains(MARKER) {
        HookStatus::Managed
    } else {
        HookStatus::Foreign
    }
}

fn render_script(hook: Hook, binary: &Path) -> String {
    format!(
        "#!/bin/sh\n{MARKER}\nexec {} {} \"$@\"\n",
        shell_quote(binary),
        hook.subcommand(),
    )
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    if rendered
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    {
        rendered.into_owned()
    } else {
        let escaped = rendered.replace('\'', "'\\''");
        format!("'{escaped}'")
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn init_git_repo(dir: &Path) -> PathBuf {
        git2::Repository::init(dir).unwrap();
        dir.join(".git")
    }

    #[test]
    fn install_creates_executable_post_rewrite_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = init_git_repo(dir.path());
        let binary = PathBuf::from("/usr/local/bin/concats");

        let result = super::install(&gitdir, Hook::PostRewrite, &binary).unwrap();
        assert_eq!(result, HookStatus::Managed);

        let hook = gitdir.join("hooks").join("post-rewrite");
        let contents = fs::read_to_string(&hook).unwrap();
        assert!(contents.contains(MARKER));
        assert!(contents.contains("/usr/local/bin/concats rewrite"));
        let mode = fs::metadata(&hook).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn install_creates_executable_post_commit_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = init_git_repo(dir.path());
        let binary = PathBuf::from("/usr/local/bin/concats");

        let result = super::install(&gitdir, Hook::PostCommit, &binary).unwrap();
        assert_eq!(result, HookStatus::Managed);

        let hook = gitdir.join("hooks").join("post-commit");
        let contents = fs::read_to_string(&hook).unwrap();
        assert!(contents.contains(MARKER));
        assert!(contents.contains("/usr/local/bin/concats commit-link"));
    }

    #[test]
    fn install_overwrites_managed_hook_and_preserves_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = init_git_repo(dir.path());
        let binary = PathBuf::from("/concats");
        let hook = gitdir.join("hooks").join("post-rewrite");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();

        // Foreign hook is preserved.
        fs::write(&hook, "#!/bin/sh\necho foreign\n").unwrap();
        let result = super::install(&gitdir, Hook::PostRewrite, &binary).unwrap();
        assert_eq!(result, HookStatus::Foreign);
        assert_eq!(
            fs::read_to_string(&hook).unwrap(),
            "#!/bin/sh\necho foreign\n"
        );

        // Managed hook is overwritten.
        fs::write(&hook, format!("#!/bin/sh\n{MARKER}\nold\n")).unwrap();
        let result = super::install(&gitdir, Hook::PostRewrite, &binary).unwrap();
        assert_eq!(result, HookStatus::Managed);
        let contents = fs::read_to_string(&hook).unwrap();
        assert!(contents.contains("exec /concats rewrite"));
    }

    #[test]
    fn uninstall_removes_only_managed_hook() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = init_git_repo(dir.path());
        let binary = PathBuf::from("/concats");

        super::install(&gitdir, Hook::PostRewrite, &binary).unwrap();
        assert_eq!(status(&gitdir, Hook::PostRewrite), HookStatus::Managed);

        super::uninstall(&gitdir, Hook::PostRewrite).unwrap();
        assert_eq!(status(&gitdir, Hook::PostRewrite), HookStatus::Missing);

        // Foreign hook is not removed.
        let hook = gitdir.join("hooks").join("post-rewrite");
        fs::write(&hook, "#!/bin/sh\necho foreign\n").unwrap();
        let result = super::uninstall(&gitdir, Hook::PostRewrite).unwrap();
        assert_eq!(result, HookStatus::Foreign);
        assert!(hook.exists());
    }

    #[test]
    fn quotes_paths_with_spaces() {
        let quoted = shell_quote(Path::new("/tmp/with space/concats"));
        assert_eq!(quoted, "'/tmp/with space/concats'");
    }
}
