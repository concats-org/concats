//! The guide loop: hand an agent ready-made links, check what it wrote, store
//! it for the app to pick up.
//!
//! `lint` is why this exists. An agent can check its own work: write a review
//! guide, run `lint`, and get told what it broke and what it forgot, with
//! ready-made links for the hunks it missed. The fix is a copy, not more line
//! arithmetic.

use std::{
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use concats_diff::load::Loaded;
use concats_review::{guide, store};

use super::{BAD_INPUT, FINDINGS, GuideArgs, Outcome, RangeArgs, counted};

pub(crate) fn manifest(range: &RangeArgs) -> Outcome {
    let (loaded, root) = super::load(&range.resolve()?)?;
    print!("{}", guide::manifest(&loaded, &root));
    Ok(())
}

pub(crate) fn lint(path: &str, gate: &GuideArgs, range: &RangeArgs) -> Outcome {
    let (failed, _, _) = check(path, gate, range)?;
    if failed {
        return Err(ExitCode::from(FINDINGS));
    }
    Ok(())
}

/// Validate and store a guide locally — the agent-facing hand-off. Guides go in
/// the repo's review store, keyed by the resolved (merge base, head) of the
/// range they were linted against; a review app open on that range picks the
/// newest one up within a second.
pub(crate) fn submit(
    path: &str,
    author: Option<String>,
    gate: &GuideArgs,
    range: &RangeArgs,
) -> Outcome {
    let (failed, loaded, markdown) = check(path, gate, range)?;
    if failed {
        eprintln!("\nnot submitted — fix the failures above and rerun");
        return Err(ExitCode::from(FINDINGS));
    }
    // WORKTREE endpoints key as the zero oid — the app looks worktree guides up
    // under the same convention.
    let (base, head) = store::guide_key(loaded.merge_base, loaded.head);
    let rec = store::Guide {
        base,
        head,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        author,
        markdown,
    };
    match store::save_guide(&loaded.git_dir, &rec) {
        Ok(()) => {
            let msg = "submitted — stored in this repo's review store\n\
                       a review app open on this range picks it up within ~1s";
            // Keep stdout parseable when the shared lint run printed JSON.
            if gate.json {
                eprintln!("{msg}");
            } else {
                println!("{msg}");
            }
            Ok(())
        }
        Err(error) => {
            eprintln!("error: {error}");
            Err(ExitCode::from(BAD_INPUT))
        }
    }
}

/// The whole lint run — reading, loading, reporting, printing — returning what
/// `submit` needs to reuse it verbatim: the verdict, the loaded diff (for the
/// resolved oids), and the guide text.
fn check(
    path: &str,
    gate: &GuideArgs,
    range: &RangeArgs,
) -> Result<(bool, Loaded, String), ExitCode> {
    let target = range.resolve()?;
    let Ok(markdown) = std::fs::read_to_string(path) else {
        eprintln!("error: cannot read {path}");
        return Err(ExitCode::from(BAD_INPUT));
    };
    let (loaded, root) = super::load(&target)?;

    let report = guide::lint(&markdown, &loaded, &root);
    let pct = report.coverage_pct();
    let coverage_fail = pct + 1e-9 < gate.min_coverage;
    let failed = !report.broken.is_empty() || coverage_fail;

    if gate.json {
        print_json(path, &report, pct, failed);
        return Ok((failed, loaded, markdown));
    }
    print_report(path, &report, &target, gate, (pct, coverage_fail, failed));
    Ok((failed, loaded, markdown))
}

fn print_report(
    path: &str,
    report: &guide::Report,
    target: &concats_state::Target,
    gate: &GuideArgs,
    (pct, coverage_fail, failed): (f64, bool, bool),
) {
    println!("{path}   [{} → {}]\n", target.base, target.head);
    print_problems(path, report, gate);

    if report.no_refs {
        println!("⚠ this review references no part of the diff at all — prose about nothing\n");
    }

    let mark = if coverage_fail { "✗" } else { "·" };
    println!(
        "{mark} coverage {pct:.0}%   {} of {} changed lines   ({} of {} hunks)",
        report.lines_covered, report.lines_total, report.hunks_placed, report.hunks_total
    );
    if coverage_fail {
        println!(
            "      below the --min-coverage {:.0}% threshold",
            gate.min_coverage
        );
    }

    print_new_file_warning(report);
    print_uncovered(report);
    println!("{}", if failed { "FAIL" } else { "OK" });
}

/// What the guide got wrong: links that resolve to nothing, and hunks it
/// pointed at more than once.
fn print_problems(path: &str, report: &guide::Report, gate: &GuideArgs) {
    if !report.broken.is_empty() {
        println!("✗ {} broken link(s)\n", report.broken.len());
        for p in &report.broken {
            println!("  {path}:{}", p.line);
            println!("      {}", p.locator);
            println!("      → {}\n", p.message);
        }
    }
    if !gate.allow_duplicates && !report.duplicates.is_empty() {
        println!("⚠ {} duplicate reference(s)\n", report.duplicates.len());
        for p in &report.duplicates {
            println!("  {path}:{}  {}", p.line, p.message);
        }
        println!();
    }
}

/// The payload: ready-made links for everything the guide never mentioned.
///
/// Printed at column 0 — indenting these would make a pasted link a markdown
/// code block in any normal viewer.
fn print_uncovered(report: &guide::Report) {
    if report.uncovered.is_empty() {
        return;
    }
    println!("\n  Not referenced — paste a link line where it belongs.\n");
    for f in &report.uncovered {
        println!(
            "  ── {}  ({} of {} hunks unreferenced)\n",
            f.path,
            f.links.len(),
            f.total_hunks
        );
        for l in &f.links {
            println!("{l}\n");
        }
    }
}

/// A whole new file is one hunk worth hundreds of lines, so linking new files
/// is very cheap coverage. We can't forbid it — it's legitimate — so we make it
/// visible. A review that clears the gate mostly on new-file bulk has not
/// actually looked at the changes to existing code.
fn print_new_file_warning(report: &guide::Report) {
    let share = report.new_file_share();
    if report.new_file_hunks_placed == 0 || share < 50.0 {
        return;
    }
    println!(
        "\n⚠ {share:.0}% of your covered lines ({} of {}) come from {} whole-new-file link(s).",
        report.lines_covered_new_files, report.lines_covered, report.new_file_hunks_placed
    );
    let old_covered = report.lines_covered - report.lines_covered_new_files;
    let old_total = report
        .lines_total
        .saturating_sub(report.lines_covered_new_files);
    let old_pct = if old_total == 0 {
        100.0
    } else {
        counted(old_covered) / counted(old_total) * 100.0
    };
    println!(
        "      New files are one hunk each, so they are cheap coverage. Coverage of the\n\
         \x20     changes to EXISTING code is what this number was meant to measure — and\n\
         \x20     yours is {old_pct:.0}%. Read the modified files."
    );
}

/// Machine-readable, for an agent loop that parses rather than reads.
fn print_json(path: &str, report: &guide::Report, pct: f64, failed: bool) {
    let value = serde_json::json!({
        "guide": path,
        "ok": !failed,
        "coverage_pct": (pct * 10.0).round() / 10.0,
        "lines_covered": report.lines_covered,
        "lines_total": report.lines_total,
        "hunks_placed": report.hunks_placed,
        "hunks_total": report.hunks_total,
        "broken": report.broken.iter().map(|p| serde_json::json!({
            "line": p.line,
            "locator": p.locator,
            "message": p.message,
        })).collect::<Vec<_>>(),
        "uncovered": report.uncovered.iter().map(|f| serde_json::json!({
            "path": f.path,
            "links": f.links,
        })).collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}
