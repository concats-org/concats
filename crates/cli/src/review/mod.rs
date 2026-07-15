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
//! `--repo`/`--base`/`--head`, falling back to the window this terminal belongs
//! to. The app exports `CONCATS_APP_WINDOW` (an identity, so it cannot go
//! stale) and republishes that window's range on every load. So an agent in the
//! app's built-in terminal reviews the diff on screen with no flags at all,
//! across range switches and without cross-talk between windows. The spawn-time
//! `CONCATS_APP_*` values are the fallback for when the app has exited. Outside
//! all of that the CLI does not guess: it exits 2 and names what to pass. Only
//! `--repo` has a default — the cwd, like git.
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
use concats_state::Target;

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
    /// Lines are 1-based new-side numbers, the same the manifest's links carry.
    /// The anchor is checked against the loaded diff, so a stored comment
    /// always renders.
    Add {
        /// Where: `<path>:<start>[-<end>]`.
        #[arg(value_name = "PATH:LINES")]
        anchor: String,
        /// The comment body.
        #[arg(short = 'm', long = "body", value_name = "TEXT")]
        body: String,
        /// Who wrote it.
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
    Reply {
        /// The comment to answer (`3` or `#3`).
        #[arg(value_name = "ID")]
        id: String,
        /// Optionally, where that code lives now: `<path>:<start>[-<end>]`.
        #[arg(value_name = "PATH:LINES")]
        anchor: Option<String>,
        /// The reply body.
        #[arg(short = 'm', long = "body", value_name = "TEXT")]
        body: String,
        /// Who wrote it.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// The whole comment store as one interchange document.
    ///
    /// You can paste it, diff it and import it back — importing an export is a
    /// no-op.
    Export {
        /// Emit the terse bot-style prompt format instead of the canonical one.
        #[arg(long)]
        prompt: bool,
        /// Write here instead of to stdout.
        #[arg(short = 'o', long = "out", value_name = "FILE")]
        out: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
    },

    /// Import comments from a document, or from a pull request.
    ///
    /// Takes either markdown profile, or a pull request's review comments as
    /// JSON — `gh api repos/{owner}/{repo}/pulls/{n}/comments --paginate` pipes
    /// straight in. Re-importing is a no-op.
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
    /// The range to review: the flags, then the live window, then the
    /// environment, then a loud error. The module docs say why the last step is
    /// an error and not a guess.
    pub(crate) fn resolve(&self) -> Result<Target, ExitCode> {
        let window = window_range();
        let repo = self
            .repo
            .clone()
            .or_else(|| window.as_ref().map(|w| w.repo.clone()))
            .or_else(|| env("CONCATS_APP_REPO"))
            .unwrap_or_else(|| ".".to_string());
        let (Some(base), Some(head)) = (
            self.base
                .clone()
                .or_else(|| window.as_ref().map(|w| w.base.clone()))
                .or_else(|| env("CONCATS_APP_BASE")),
            self.head
                .clone()
                .or_else(|| window.as_ref().map(|w| w.head.clone()))
                .or_else(|| env("CONCATS_APP_HEAD")),
        ) else {
            eprintln!(
                "error: no review range — pass --base <rev> --head <rev>, or run inside \
                 the concats-app terminal (its CONCATS_APP_* environment names the open diff).\n\
                 Common ranges:\n  \
                 --base HEAD  --head WORKTREE   everything uncommitted\n  \
                 --base INDEX --head WORKTREE   unstaged changes only\n  \
                 --base main  --head HEAD       a branch against its merge base"
            );
            return Err(ExitCode::from(BAD_INPUT));
        };
        Ok(Target { repo, base, head })
    }
}

/// The repo alone — for the commands that never touch a range (listing or
/// deleting stored comments works on the repo, whatever diff is open).
pub(crate) fn repo_arg(repo: Option<&str>) -> String {
    repo.map(str::to_string)
        .or_else(|| window_range().map(|w| w.repo))
        .or_else(|| env("CONCATS_APP_REPO"))
        .unwrap_or_else(|| ".".to_string())
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

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// What the window this terminal belongs to has open.
fn window_range() -> Option<Target> {
    let window = env("CONCATS_APP_WINDOW")?;
    concats_state::window_range(&concats_state::open_app_db()?, &window)
}

/// Load `target`'s diff, or say why not. Returns the repository root alongside
/// it, which is what every link in the output is relative to.
pub(crate) fn load(target: &Target) -> Result<(Loaded, String), ExitCode> {
    let Some(root) = load::discover(Path::new(&target.repo)) else {
        eprintln!("error: no git repository at {}", target.repo);
        return Err(ExitCode::from(BAD_INPUT));
    };
    match load::load(Path::new(&target.repo), &target.base, &target.head) {
        Ok(loaded) => Ok((loaded, root.display().to_string())),
        Err(error) => {
            eprintln!("error: {error}");
            Err(ExitCode::from(BAD_INPUT))
        }
    }
}

/// Open the repository at `repo` for a command that needs the review store but
/// not a diff.
pub(crate) fn open_store(repo: Option<&str>) -> Result<concats_review::store::Store, ExitCode> {
    let repo = repo_arg(repo);
    let Some(root) = load::discover(Path::new(&repo)) else {
        eprintln!("error: no git repository at {repo}");
        return Err(ExitCode::from(BAD_INPUT));
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
