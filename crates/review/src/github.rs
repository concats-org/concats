//! A pull request's review comments, as the GitHub REST API serves them.
//!
//! ```sh
//! gh api repos/{owner}/{repo}/pulls/{n}/comments --paginate \
//!   | concats comments import -
//! ```
//!
//! The pipe is the whole integration: no HTTP client, no token, no `gh`
//! subprocess. Whatever puts the JSON on stdin is the caller's business. This
//! module is the protocol boundary and nothing more. It maps GitHub's payload
//! onto [`interchange::Entry`], and from there import — resolution against the
//! loaded diff, threading, dedupe — runs the same code path the markdown
//! profiles use.
//!
//! GitHub's line model is already ours. `side: "RIGHT"` is the new side,
//! `"LEFT"` the old, and `start_line`…`line` is the 1-based display range the
//! manifest's `#Lstart-end` links carry. `in_reply_to_id` is a thread root,
//! like [`crate::store::Comment::parent`].
//!
//! What cannot be mapped is reported, never dropped on the quiet — see
//! [`Document::warnings`]. On a live pull request that is usually an outdated
//! thread: GitHub nulls its `line` once the lines it was written against left
//! the diff. That is a property of the source, not something the caller can
//! fix.

use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    Error,
    interchange::{Document, Entry, Meta, Side},
};

/// One item of `GET /repos/{owner}/{repo}/pulls/{n}/comments`. Only the fields
/// that survive the trip; GitHub sends about forty more.
#[derive(Deserialize)]
struct ReviewComment {
    id: u64,
    in_reply_to_id: Option<u64>,
    /// Absent on the issue-comments endpoint, which is the usual wrong turn.
    path: Option<String>,
    body: String,
    user: Option<User>,
    created_at: Option<String>,
    /// The range's last line, 1-based. Null once the thread is outdated.
    line: Option<u32>,
    start_line: Option<u32>,
    side: Option<String>,
    start_side: Option<String>,
    /// `"line"` or `"file"`; absent on older payloads, which are all line
    /// comments.
    subject_type: Option<String>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

/// Parse a payload into the same document the markdown profiles produce.
///
/// Accepts a stream of top-level values: `gh api --paginate` concatenates one
/// array per page and `--slurp` nests them. Both are what you actually type,
/// and neither is a single flat array.
pub fn parse(json: &str) -> Result<Document, Error> {
    let mut doc = Document {
        meta: Meta::default(),
        entries: Vec::new(),
        warnings: Vec::new(),
    };
    let mut unanchored = 0usize;

    for value in serde_json::Deserializer::from_str(json).into_iter::<serde_json::Value>() {
        let value = value?;
        for item in flatten(value) {
            match serde_json::from_value::<ReviewComment>(item) {
                Ok(c) => match entry(&c) {
                    Ok(e) => doc.entries.push(e),
                    Err(Skip::NotLineAnchored) => unanchored += 1,
                    Err(Skip::Outdated) => doc.warnings.push(format!(
                        "comment {} is outdated — GitHub no longer places it in the diff",
                        c.id
                    )),
                },
                Err(e) => doc.warnings.push(format!("unrecognized comment: {e}")),
            }
        }
    }

    if unanchored > 0 {
        doc.warnings.push(format!(
            "{unanchored} comment(s) are not line-anchored — skipped. Pull request \
             conversation lives on a different endpoint than review comments; the \
             one to import is `repos/{{owner}}/{{repo}}/pulls/{{n}}/comments`"
        ));
    }
    Ok(doc)
}

/// A top-level value as a flat sequence of comment objects. An array yields its
/// elements, recursing so `--slurp`'s array of pages works; an object is
/// itself.
fn flatten(value: serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => items.into_iter().flat_map(flatten).collect(),
        other => vec![other],
    }
}

enum Skip {
    /// A pull request comment with no line to anchor to.
    NotLineAnchored,
    /// A line comment GitHub can no longer place in the diff.
    Outdated,
}

/// The provenance an imported pull-request comment is stored under. Import
/// matches on this one string, so writer and reader cannot drift apart.
pub fn provenance(id: u64) -> String {
    format!("github:{id}")
}

fn entry(c: &ReviewComment) -> Result<Entry, Skip> {
    let (Some(path), Some(end)) = (c.path.clone(), c.line) else {
        // `subject_type: "file"` and issue comments both land here; only the
        // second is a wrong endpoint, and only a null `line` on a real line
        // comment is worth naming.
        return Err(match (&c.path, c.subject_type.as_deref()) {
            (Some(_), Some("file")) | (None, _) => Skip::NotLineAnchored,
            _ => Skip::Outdated,
        });
    };
    let side = match c.side.as_deref() {
        Some("LEFT") => Side::Old,
        _ => Side::New,
    };
    // A range whose ends sit on different sides is GitHub's LEFT→RIGHT
    // selection. Its extent on the far side is not in the payload, so anchor
    // on the line GitHub places the comment at rather than invent one.
    let crosses_sides = c.start_side.is_some() && c.start_side != c.side;
    let start = if crosses_sides {
        end
    } else {
        c.start_line.unwrap_or(end)
    };
    Ok(Entry {
        path,
        side,
        start: start.min(end),
        end: start.max(end),
        body: c.body.clone(),
        author: c.user.as_ref().map(|u| u.login.clone()),
        id: Some(c.id),
        reply_to: c.in_reply_to_id,
        external: Some(provenance(c.id)),
        created_at: c.created_at.as_deref().and_then(unix_seconds),
        line: 0,
    })
}

fn unix_seconds(stamp: &str) -> Option<u64> {
    OffsetDateTime::parse(stamp, &Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp().max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &str = r#"[
      {"id": 2181234567, "in_reply_to_id": null, "path": "src/lib.rs",
       "body": "this leaks on the error path", "user": {"login": "octocat"},
       "created_at": "2026-07-30T09:15:00Z", "line": 68, "start_line": 63,
       "side": "RIGHT", "start_side": "RIGHT", "subject_type": "line"},
      {"id": 2181234599, "in_reply_to_id": 2181234567, "path": "src/lib.rs",
       "body": "fixed — the early return drops it now", "user": {"login": "claude"},
       "created_at": "2026-07-30T11:02:03Z", "line": 68, "start_line": null,
       "side": "RIGHT", "start_side": null, "subject_type": "line"}
    ]"#;

    #[test]
    fn maps_a_pull_requests_review_comments() {
        let doc = parse(PAYLOAD).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.entries.len(), 2);

        let root = &doc.entries[0];
        assert_eq!(root.path, "src/lib.rs");
        assert_eq!((root.side, root.start, root.end), (Side::New, 63, 68));
        assert_eq!(root.author.as_deref(), Some("octocat"));
        assert_eq!(root.id, Some(2181234567));
        assert_eq!(root.reply_to, None);
        assert_eq!(root.external.as_deref(), Some("github:2181234567"));
        assert_eq!(root.created_at, Some(1_785_402_900));

        let reply = &doc.entries[1];
        assert_eq!(reply.reply_to, Some(2181234567));
        // A reply with no start_line is the single line GitHub places it on.
        assert_eq!((reply.start, reply.end), (68, 68));
    }

    #[test]
    fn a_left_side_comment_is_an_old_side_anchor() {
        let doc = parse(
            r#"[{"id": 1, "path": "a.rs", "body": "why was this dropped?",
                "line": 12, "side": "LEFT", "subject_type": "line"}]"#,
        )
        .unwrap();
        assert_eq!(doc.entries[0].side, Side::Old);
        assert_eq!((doc.entries[0].start, doc.entries[0].end), (12, 12));
    }

    #[test]
    fn a_range_crossing_sides_anchors_where_github_places_it() {
        // start_side LEFT, side RIGHT: the old side's extent is not in the
        // payload, so the anchor is the placed line alone.
        let doc = parse(
            r#"[{"id": 1, "path": "a.rs", "body": "b", "line": 40,
                "start_line": 12, "side": "RIGHT", "start_side": "LEFT"}]"#,
        )
        .unwrap();
        assert_eq!(
            (
                doc.entries[0].side,
                doc.entries[0].start,
                doc.entries[0].end
            ),
            (Side::New, 40, 40)
        );
    }

    #[test]
    fn reports_what_it_cannot_anchor() {
        // An outdated thread: GitHub nulls `line` once the diff moves past it.
        let doc = parse(
            r#"[{"id": 7, "path": "a.rs", "body": "stale", "line": null,
                "original_line": 3, "side": "RIGHT", "subject_type": "line"}]"#,
        )
        .unwrap();
        assert!(doc.entries.is_empty());
        assert!(doc.warnings[0].contains("outdated"), "{:?}", doc.warnings);

        // The issue-comments endpoint: no path, so nothing to anchor to.
        let doc = parse(r#"[{"id": 8, "body": "LGTM", "user": {"login": "octocat"}}]"#).unwrap();
        assert!(doc.entries.is_empty());
        assert!(
            doc.warnings[0].contains("pulls/{n}/comments"),
            "{:?}",
            doc.warnings
        );

        // A file-level comment has a path but no line.
        let doc = parse(
            r#"[{"id": 9, "path": "a.rs", "body": "rename this file",
                "subject_type": "file"}]"#,
        )
        .unwrap();
        assert!(doc.entries.is_empty());
        assert_eq!(doc.warnings.len(), 1);
    }

    #[test]
    fn reads_the_pages_gh_paginate_concatenates() {
        let two_pages = format!("{PAYLOAD}\n{PAYLOAD}");
        assert_eq!(parse(&two_pages).unwrap().entries.len(), 4);
        // And --slurp's array of pages.
        let slurped = format!("[{PAYLOAD},{PAYLOAD}]");
        assert_eq!(parse(&slurped).unwrap().entries.len(), 4);
    }
}
