use std::path::Path;

/// An opaque commit identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Oid(git2::Oid);

impl Oid {
    pub fn short(&self) -> String {
        self.0.to_string()[..7].to_string()
    }
}

impl From<git2::Oid> for Oid {
    fn from(oid: git2::Oid) -> Self {
        Self(oid)
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Force-push a single ref to a remote.
///
/// Uses a force-push refspec (`+ref:ref`) which is safe for per-session refs
/// since only concats writes to them. Credentials are resolved via the SSH
/// agent, git credential helpers, or defaults.
pub fn push_ref(repo_path: &Path, remote_name: &str, ref_name: &str) -> Result<(), git2::Error> {
    let repo = git2::Repository::open(repo_path)?;
    let mut remote = repo.find_remote(remote_name)?;

    let refspec = format!("+{ref_name}:{ref_name}");

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        // Try SSH agent first.
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            if let Some(username) = username_from_url {
                return git2::Cred::ssh_key_from_agent(username);
            }
        }
        // Try git credential helper.
        if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            return git2::Cred::credential_helper(
                &repo.config()?,
                url,
                username_from_url,
            );
        }
        // Fall back to default credentials.
        git2::Cred::default()
    });

    let mut push_options = git2::PushOptions::new();
    push_options.remote_callbacks(callbacks);

    remote.push(&[&refspec], Some(&mut push_options))?;
    Ok(())
}
