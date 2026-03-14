use std::{ffi::OsStr, path::Path};

use crate::error::{Error, Result};

/// An opaque commit identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oid(git2::Oid);

impl Oid {
    #[must_use]
    pub fn short(&self) -> String {
        self.0.to_string()[..7].to_string()
    }

    #[must_use]
    pub fn as_git(self) -> git2::Oid {
        self.0
    }
}

impl From<git2::Oid> for Oid {
    fn from(oid: git2::Oid) -> Self {
        Self(oid)
    }
}

impl std::str::FromStr for Oid {
    type Err = git2::Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        git2::Oid::from_str(value).map(Self)
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl serde::Serialize for Oid {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Oid {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Resolve `HEAD` to a commit object ID.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, `HEAD` cannot be
/// resolved, or `HEAD` does not point to a commit.
pub fn current_head_oid(repo_path: &Path) -> Result<Oid> {
    let repo = git2::Repository::open(repo_path)?;
    let head = repo.head()?;
    let oid = head
        .target()
        .ok_or_else(|| Error::session("HEAD does not point to a commit"))?;
    Ok(Oid::from(oid))
}

pub(crate) fn snapshot_workdir(repo: &git2::Repository) -> Result<git2::Oid> {
    let mut index = repo.index()?;
    index.clear()?;

    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::session("bare repository not supported"))?;
    let workdir = workdir.to_path_buf();

    let walker = ignore::WalkBuilder::new(&workdir)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(move |entry| {
            // NOTE: Don't ignore the root even if it has a .git entry.
            if entry.depth() == 0 {
                return true;
            }

            let file_type = entry
                .file_type()
                .expect("filesystem walk entries always have a file type");

            // NOTE: Ignore the .git entry itself.
            if entry
                .path()
                .file_name()
                .is_some_and(|name| name == OsStr::new(".git"))
            {
                return false;
            }

            if file_type.is_file() || file_type.is_symlink() {
                return true;
            }

            // NOTE: Ignore non-file, non-directory entries such as sockets,
            // pipes, and device nodes.
            if !file_type.is_dir() {
                return false;
            }

            // NOTE: Ignore nested git roots such as worktrees and sub-repos.
            match entry.path().join(".git").try_exists() {
                Ok(true) => false,
                Ok(false) => true,
                Err(error) => {
                    tracing::warn!(
                        path = ?entry.path(),
                        "failed to inspect directory for nested git root: {error}"
                    );
                    true
                }
            }
        })
        .build();

    for relative_path in walker.filter_map(|entry| {
        let entry = entry.ok()?;

        // NOTE: Ignore directories, which the walker yields alongside files.
        entry.file_type().filter(|file_type| !file_type.is_dir())?;

        entry
            .path()
            .strip_prefix(&workdir)
            .ok()
            .map(Path::to_path_buf)
    }) {
        if let Err(error) = index.add_path(&relative_path) {
            tracing::warn!("failed to add {relative_path:?} to checkpoint tree: {error}");
        }
    }

    Ok(index.write_tree()?)
}

/// Force-push a single ref to a remote.
///
/// Uses a force-push refspec (`+ref:ref`) which is safe for per-session refs
/// since only concats writes to them. Credentials are resolved via the SSH
/// agent, git credential helpers, or defaults.
pub fn push_ref(
    repo_path: &Path,
    remote_name: &str,
    ref_name: &str,
) -> std::result::Result<(), git2::Error> {
    let repo = git2::Repository::open(repo_path)?;
    let mut remote = repo.find_remote(remote_name)?;

    let refspec = format!("+{ref_name}:{ref_name}");

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        // Try SSH agent first.
        if allowed_types.contains(git2::CredentialType::SSH_KEY)
            && let Some(username) = username_from_url
        {
            return git2::Cred::ssh_key_from_agent(username);
        }
        // Try git credential helper.
        if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            return git2::Cred::credential_helper(&repo.config()?, url, username_from_url);
        }
        // Fall back to default credentials.
        git2::Cred::default()
    });

    let mut push_options = git2::PushOptions::new();
    push_options.remote_callbacks(callbacks);

    remote.push(&[&refspec], Some(&mut push_options))?;
    Ok(())
}
