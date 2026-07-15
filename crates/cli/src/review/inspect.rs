//! Looking at a range without changing anything: which recorded turns produced
//! it, and what loading it costs.

use std::{collections::HashMap, path::Path};

use concats_highlight::Highlighter;
use concats_review::sessions;

use super::{Outcome, RangeArgs, counted};

/// Which recorded turns link to the range, and how — what the Sessions tab is
/// built from, printable, so the linking can be inspected (and driven from a
/// hook) without opening a window.
pub(crate) fn turns(range: &RangeArgs) -> Outcome {
    let target = range.resolve()?;
    let (loaded, root) = super::load(&target)?;
    let Ok(repo) = gix::open(Path::new(&root)) else {
        eprintln!("error: cannot open {root}");
        return Err(std::process::ExitCode::from(super::BAD_INPUT));
    };

    let (Some(merge_base), Some(head)) = (loaded.merge_base, loaded.head) else {
        println!("a WORKTREE range has no commits — sessions link to commits only");
        return Ok(());
    };
    let diff = sessions::range_diff(&repo, &merge_base, &head);
    let turns = sessions::load_turns(&repo, &diff);
    if turns.is_empty() {
        println!("no agent sessions (refs/agent/sessions/*) in {root}");
        return Ok(());
    }

    // A turn can be linked by content and by the commit it was recorded at; the
    // mark shows the stronger of the two.
    let mut via: HashMap<usize, (bool, bool)> = HashMap::new();
    for link in &sessions::link_turns(&diff, &turns) {
        let entry = via.entry(link.turn).or_insert((false, false));
        entry.0 |= link.via_tree;
        entry.1 |= link.via_branch;
    }

    let mut session = "";
    for (i, turn) in turns.iter().enumerate() {
        if turn.session_id != session {
            session = &turn.session_id;
            println!("\n── session {session}");
        }
        let mark = match via.get(&i) {
            Some((true, _)) => "◆ linked (content)",
            Some((_, true)) => "◇ linked (recorded at commit)",
            _ => "·",
        };
        let entries = turn.message.entries().len();
        println!(
            "  {}  {}  [{entries} entr{}, {} file(s) touched]  {mark}",
            &turn.oid.to_string()[..10],
            turn.message.subject(),
            if entries == 1 { "y" } else { "ies" },
            turn.touched.len(),
        );
        if via.contains_key(&i) {
            for c in &turn.touched {
                println!("        {}", c.path());
            }
        }
    }
    println!(
        "\n{} of {} turn(s) linked to {}...{}",
        via.len(),
        turns.len(),
        target.base,
        target.head
    );
    Ok(())
}

/// Where the wall-clock goes on a cold load, per stage and per language.
pub(crate) fn bench(no_hl: bool, range: &RangeArgs) -> Outcome {
    let (mut loaded, _) = super::load(&range.resolve()?)?;
    let stats = loaded.stats.clone();
    let rows: usize = loaded.files.iter().map(|f| f.default_rows().len()).sum();

    println!(
        "{} files · +{}/-{} · {rows} rows · {} blobs · {:.2} MB · {} binary skipped",
        stats.files,
        stats.adds,
        stats.dels,
        loaded.blobs.len(),
        counted(stats.bytes) / 1_048_576.0,
        stats.skipped_binary
    );
    println!(
        "renames: {} exact + {} inexact",
        stats.renames_exact, stats.renames_inexact
    );
    println!(
        "LOAD: git {:.1}ms  rename {:.1}ms  diff {:.1}ms  lower {:.1}ms  → {:.1}ms",
        stats.git_ms, stats.rename_ms, stats.diff_ms, stats.lower_ms, stats.total_ms
    );
    if no_hl {
        return Ok(());
    }

    // The app highlights lazily; this forces every blob, i.e. the worst case.
    let mut hl = Highlighter::new();
    let started = std::time::Instant::now();
    for blob in &mut loaded.blobs {
        let one = std::time::Instant::now();
        let spans = hl.compute(&blob.ext, &blob.text);
        hl.record(&blob.ext, spans.len(), one.elapsed());
        blob.spans = Some(spans);
    }
    println!(
        "HIGHLIGHT ALL (lazy in the app): {:.1}ms over {} blobs, {} grammars",
        started.elapsed().as_secs_f64() * 1000.0,
        loaded.blobs.len(),
        hl.grammar_count()
    );

    println!(
        "\n{:<12} {:>6} {:>9} {:>10} {:>11}",
        "language", "blobs", "lines", "ms", "lines/ms"
    );
    for lang in hl.stats.ranked() {
        let rate = if lang.ms > 0.0 {
            counted(lang.lines) / lang.ms
        } else {
            0.0
        };
        println!(
            "{:<12} {:>6} {:>9} {:>10.1} {rate:>11.0}",
            lang.lang, lang.blobs, lang.lines, lang.ms
        );
    }
    Ok(())
}
