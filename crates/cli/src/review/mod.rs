//! The review commands: everything that works on a diff rather than on a
//! session.
//!
//! Only in the `review` build, the one that ships inside the app bundle. It
//! links the review domain, sqlite and a CRDT, and no UI toolkit — that is what
//! the crate split is for: these commands run without a window.
//!
//! ## Ranges
//!
//! Every command works on `base...head` and takes it the same way:
//! `--repo`/`--base`/`--head`, falling back to what the app shows for the repo.
//! A window reads a pipe in the root of the checkout it shows
//! ([`concats_state::PIPE`]) and answers with its range as the document has
//! it at the moment it is asked. So an agent anywhere in that checkout reviews
//! the diff on screen with no flags at all, across range switches, and a
//! linked worktree never answers for another. Without an app, the review is
//! the branch so far: from where HEAD left the default branch to the working
//! tree, staged or not. `--repo` defaults to the cwd, like git.
//!
//! ## Exit codes
//!
//! This is what makes the commands usable from a hook or CI, and why this
//! module answers in [`ExitCode`] rather than `miette::Result`:
//!
//! - `0` clean;
//! - `1` findings — broken links, coverage below the gate, nothing to act on;
//! - `2` bad arguments, or no diff to load.

use std::{path::Path, process::ExitCode};

use clap::{Args, Subcommand, ValueEnum};
use concats_diff::load::{self, Loaded};
use concats_state::{Answer, Method, Request, Target};

mod comments;
mod guide;
mod inspect;

/// Exit codes as named values, so a `2` in a command body says why.
pub(crate) const OK: u8 = 0;
pub(crate) const FINDINGS: u8 = 1;
pub(crate) const BAD_INPUT: u8 = 2;

/// A command's outcome: `Ok` is exit 0, `Err` carries the code to exit with.
/// `?` on a helper that could not do its job hands that code straight on.
pub(crate) type Outcome = Result<(), ExitCode>;

#[derive(Subcommand)]
pub enum ReviewCommands {
    /// Every reviewable hunk of the diff, each with a ready-made link.
    ///
    /// The links are the point: an agent copies one, it never computes a line
    /// range — which is the thing LLMs are worst at.
    Manifest {
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Check a review guide against the diff: broken links, and coverage.
    ///
    /// This is how an agent checks its own work — it is told exactly what it
    /// broke and what it forgot, with ready-made links for the hunks it missed.
    Lint {
        /// The guide to check.
        guide: String,
        #[command(flatten)]
        gate: GuideArgs,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Lint a guide, then store it for a review app open on this range.
    ///
    /// Refuses to store a failing guide, so what lands is guaranteed to render.
    /// Local and ephemeral: nothing is written to the worktree or to git.
    Submit {
        /// The guide to store.
        guide: String,
        /// Who wrote it.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        #[command(flatten)]
        gate: GuideArgs,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Load the diff and report where the time went.
    Bench {
        /// Skip highlighting — the part that dominates a cold load.
        #[arg(long)]
        no_hl: bool,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// The recorded agent turns linked to this range, and how each was linked.
    Turns {
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Review comments on a diff.
    ///
    /// With no subcommand: the stored comments, threads whole.
    Comments {
        #[command(subcommand)]
        action: Option<CommentsAction>,
        /// Repository (default: the current directory).
        #[arg(long, value_name = "PATH")]
        repo: Option<String>,
        /// Delete one comment, by id.
        #[arg(long, value_name = "ID")]
        delete: Option<u64>,
    },

    /// Print an agent spec for reviewing a diff.
    ///
    /// Embedded, so one binary is all an agent needs.
    Skill {
        /// Which spec to print.
        #[arg(value_enum, default_value_t = Skill::ReviewGuide)]
        which: Skill,
    },
}

#[derive(Subcommand)]
pub enum CommentsAction {
    /// Leave a comment on a range of lines.
    ///
    /// Lines are 1-based new-side numbers, the same the manifest's links carry;
    /// `<path>@<rev>:…` addresses deleted lines on the diff's old side, the way
    /// `concats comments` prints them. The anchor is checked against the
    /// loaded diff, so a stored comment always renders.
    Add {
        /// Where: `<path>[@<rev>]:<start>[-<end>]`.
        #[arg(value_name = "PATH:LINES")]
        anchor: String,
        /// The comment body.
        #[arg(short = 'm', long = "body", value_name = "TEXT")]
        body: String,
        /// Who wrote it (default: `git config user.name`).
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Answer a comment.
    ///
    /// With no anchor a reply takes its thread root's, so it needs no range and
    /// loads no diff. Give one and the reply is anchored there. A comment is
    /// anchored to content, so fixing the line it is about strands the thread;
    /// a thread renders under the newest of its comments the diff can place, so
    /// saying where the code lives now brings the whole conversation along.
    ///
    /// With no range given and the app open on the checkout, the app stores the
    /// reply — so it lands even from a sandbox that cannot write `.git`.
    Reply {
        /// The comment to answer (`3` or `#3`).
        #[arg(value_name = "ID")]
        id: String,
        /// Optionally, where that code lives now: `<path>[@<rev>]:<start>[-<end>]`.
        #[arg(value_name = "PATH:LINES")]
        anchor: Option<String>,
        /// The reply body.
        #[arg(short = 'm', long = "body", value_name = "TEXT")]
        body: String,
        /// Who wrote it (default: `git config user.name`).
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// The range's threads as one JSON document, each comment in GitHub's
    /// review-comment shape.
    ///
    /// You can paste it, diff it and import it back — importing an export is a
    /// no-op. With no range given and the app open on the checkout, it has the
    /// threads the app's window shows.
    Export {
        /// Emit text to paste into an agent session instead; it does not
        /// import.
        #[arg(long)]
        prompt: bool,
        /// Every thread in the store, including those made against other
        /// ranges and other worktrees.
        #[arg(long)]
        all: bool,
        /// Write here instead of to stdout.
        #[arg(short = 'o', long = "out", value_name = "FILE")]
        out: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Import comments from a document, or from a pull request.
    ///
    /// Takes what `export` writes, or a pull request's review comments as
    /// `gh api repos/{owner}/{repo}/pulls/{n}/comments --paginate` prints
    /// them. Re-importing is a no-op.
    Import {
        /// The document, or `-` for stdin.
        #[arg(value_name = "FILE|-")]
        input: String,
        /// Author for entries that name none.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        /// Report what would happen, and write nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        range: RangeArgs,
    },
}

/// The agent-facing specs. `review-guide` writes a guided tour;
/// `address-comments` is the other half of that loop.
#[derive(Clone, Copy, ValueEnum)]
pub enum Skill {
    ReviewGuide,
    AddressComments,
}

/// Which diff to work on.
#[derive(Args, Clone)]
pub struct RangeArgs {
    /// Repository to review (default: the current directory).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<String>,
    /// Base revision: any rev, or the `INDEX` / `WORKTREE` sentinels.
    #[arg(long, value_name = "REV")]
    pub base: Option<String>,
    /// Head revision: any rev, or the `INDEX` / `WORKTREE` sentinels.
    #[arg(long, value_name = "REV")]
    pub head: Option<String>,
}

/// The lint gate, shared by `lint` and `submit` — `submit` reruns the same
/// check under the same thresholds, so a guide that lints clean always stores.
#[derive(Args, Clone)]
pub struct GuideArgs {
    /// Fail below this coverage percentage.
    #[arg(long, value_name = "PCT", default_value_t = 0.0)]
    pub min_coverage: f64,
    /// Do not report references used more than once.
    #[arg(long)]
    pub allow_duplicates: bool,
    /// Machine-readable report, for a loop that parses rather than reads.
    #[arg(long)]
    pub json: bool,
}

impl RangeArgs {
    /// The range to review: the flags, then the app's window on the repo, then
    /// the branch so far — see the module docs.
    pub(crate) fn resolve(&self) -> Target {
        let repo = self.repo.clone().unwrap_or_else(|| ".".to_string());
        let shown = (self.base.is_none() || self.head.is_none())
            .then(|| shown_range(&repo))
            .flatten();
        let (shown_base, shown_head) = shown.map(|t| (t.base, t.head)).unzip();
        let head = self
            .head
            .clone()
            .or(shown_head)
            .unwrap_or_else(|| load::WORKTREE_REV.to_string());
        let base = self
            .base
            .clone()
            .or(shown_base)
            .unwrap_or_else(|| load::branch_point(Path::new(&repo)));
        Target { repo, base, head }
    }

    /// Neither end given: the command does what the app would, when one reads
    /// the checkout.
    pub(crate) const fn follows_app(&self) -> bool {
        self.base.is_none() && self.head.is_none()
    }
}

/// What the app shows for `repo`. `None` when no app reads its checkout.
// NOTE: kept out of `resolve` so its chain — flags, app, branch so far —
// reads one step per line.
// coland-ignore: @coland/single_caller_private_function
fn shown_range(repo: &str) -> Option<Target> {
    let answer = ask_app(repo, Method::Get, concats_state::RANGE, String::new())?;
    if answer.status != 200 {
        eprintln!("warning: concats-app: {}", answer.body.trim());
        return None;
    }
    serde_json::from_str(&answer.body)
        .inspect_err(|error| eprintln!("warning: concats-app: {error}"))
        .ok()
}

/// Ask the app reading the checkout `repo` is in. `None` when no app reads its
/// pipe, or when it cannot be reached — said on stderr, or the caller's
/// fallback would read as a missing app.
pub(crate) fn ask_app(repo: &str, method: Method, target: &str, body: String) -> Option<Answer> {
    let root = load::root(Path::new(repo))?;
    let request = Request {
        method,
        target: target.to_string(),
        body,
    };
    concats_state::ask(&root, &request)
        .inspect_err(|error| {
            // NOTE: A sandbox that does not allow the pipe fails here.
            let pipe = root.join(concats_state::PIPE);
            eprintln!(
                "warning: cannot reach concats-app on {}: {error}",
                pipe.display()
            );
        })
        .ok()
        .flatten()
}

/// A count as a float, for arithmetic that only ever lands in printed text —
/// percentages and megabytes.
///
/// The cast is made here and nowhere else, so this is where to justify it: the
/// counts are lines and bytes of a diff, and `f64` only starts losing precision
/// at 2^53 of them.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn counted(n: usize) -> f64 {
    n as f64
}

/// Load `target`'s diff, or say why not.
pub(crate) fn load(target: &Target) -> Result<Loaded, ExitCode> {
    load::load(Path::new(&target.repo), &target.base, &target.head).map_err(|error| {
        eprintln!("error: {error}");
        ExitCode::from(BAD_INPUT)
    })
}

/// Open the repository at `repo` for a command that needs the review store but
/// not a diff.
pub(crate) fn open_store(repo: Option<&str>) -> Result<concats_review::store::Store, ExitCode> {
    let repo = repo.unwrap_or(".");
    let Some(root) = load::discover(Path::new(&repo)) else {
        let Some(dir) = load::root(Path::new(&repo)) else {
            eprintln!("error: no directory at {repo}");
            return Err(ExitCode::from(BAD_INPUT));
        };
        return Ok(concats_review::store::Store::open(&load::concats_dir(&dir)));
    };
    let Ok(opened) = gix::open(&root) else {
        eprintln!("error: cannot open {}", root.display());
        return Err(ExitCode::from(BAD_INPUT));
    };
    Ok(concats_review::store::Store::open(opened.git_dir()))
}

#[must_use]
pub fn run(command: ReviewCommands) -> ExitCode {
    let code = match command {
        ReviewCommands::Manifest { range } => guide::manifest(&range),
        ReviewCommands::Lint { guide, gate, range } => guide::lint(&guide, &gate, &range),
        ReviewCommands::Submit {
            guide,
            author,
            gate,
            range,
        } => guide::submit(&guide, author, &gate, &range),
        ReviewCommands::Bench { no_hl, range } => inspect::bench(no_hl, &range),
        ReviewCommands::Turns { range } => inspect::turns(&range),
        ReviewCommands::Comments {
            action,
            repo,
            delete,
        } => comments::run(action, repo.as_deref(), delete),
        ReviewCommands::Skill { which } => {
            print!("{}", which.text());
            Ok(())
        }
    };
    match code {
        Ok(()) => ExitCode::from(OK),
        Err(code) => code,
    }
}

impl Skill {
    fn text(self) -> &'static str {
        match self {
            Self::ReviewGuide => include_str!("../../skills/review-guide/SKILL.md"),
            Self::AddressComments => include_str!("../../skills/address-comments/SKILL.md"),
        }
    }
}
