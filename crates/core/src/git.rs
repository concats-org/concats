use time::{OffsetDateTime, UtcOffset};

use crate::error::{Error, Result};

/// Resolve `HEAD` to a commit object ID.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, `HEAD` cannot be
/// resolved, or `HEAD` does not point to a commit.
pub fn current_head_oid(repo: &gix::Repository) -> Result<gix::ObjectId> {
    let id = repo.head_id().map_err(Error::git)?;
    Ok(id.detach())
}

pub(crate) fn commit_time(time: gix::date::Time) -> Result<OffsetDateTime> {
    let timestamp = OffsetDateTime::from_unix_timestamp(time.seconds)
        .map_err(|error| Error::session(format!("invalid git commit timestamp: {error}")))?;
    let offset = UtcOffset::from_whole_seconds(time.offset).unwrap_or(UtcOffset::UTC);
    Ok(timestamp.to_offset(offset))
}

pub(crate) fn resolve_ref<'repo>(
    repo: &'repo gix::Repository,
    ref_name: &str,
) -> Option<gix::Commit<'repo>> {
    let mut reference = repo.find_reference(ref_name).ok()?;
    let id = reference.peel_to_id().ok()?;
    repo.find_commit(id).ok()
}

/// Return whether `ancestor` is reachable from `tip` (inclusive).
pub(crate) fn reachable_from(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    ancestor: gix::ObjectId,
) -> Result<bool> {
    use gix::repository::merge_base::Error as MergeBaseError;
    match repo.merge_base(tip, ancestor) {
        Ok(base) => Ok(base.detach() == ancestor),
        Err(MergeBaseError::NotFound { .. }) => Ok(false),
        Err(error) => Err(Error::git(error)),
    }
}

pub(crate) fn signature(
    repo: &gix::Repository,
    minimum_time_seconds: Option<i64>,
) -> gix::actor::Signature {
    let (name, email) = repo
        .committer()
        .and_then(std::result::Result::ok)
        .map_or_else(
            || ("concats".into(), "concats@turn".into()),
            |sig| (sig.name.to_owned(), sig.email.to_owned()),
        );
    let mut time = repo
        .committer()
        .and_then(std::result::Result::ok)
        .and_then(|sig| sig.time().ok())
        .unwrap_or_else(gix::date::Time::now_local_or_utc);
    if let Some(minimum_time_seconds) = minimum_time_seconds
        && time.seconds <= minimum_time_seconds
    {
        time.seconds = minimum_time_seconds + 1;
    }

    gix::actor::Signature { name, email, time }
}

/// Write a commit object and force-point `ref_name` at it.
pub(crate) fn commit(
    repo: &gix::Repository,
    ref_name: &str,
    message: &str,
    parts: CommitParts,
) -> Result<gix::ObjectId> {
    let signature = signature(repo, parts.minimum_time_seconds);
    let commit = gix::objs::Commit {
        tree: parts.tree,
        parents: parts.parents.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    let oid = repo.write_object(&commit).map_err(Error::git)?.detach();
    repo.reference(
        ref_name,
        oid,
        gix::refs::transaction::PreviousValue::Any,
        parts.log_message,
    )
    .map_err(Error::git)?;
    Ok(oid)
}

/// The non-message inputs of [`commit`] — tree, parents, and the write knobs.
pub(crate) struct CommitParts {
    pub tree: gix::ObjectId,
    pub parents: Vec<gix::ObjectId>,
    pub minimum_time_seconds: Option<i64>,
    pub log_message: &'static str,
}
