//! Looking at a range without changing anything: which recorded turns produced
//! it, and what loading it costs.

use std::collections::HashMap;

use concats_highlight::Highlighter;
use concats_languages::lang_for_ext;
use concats_review::sessions;

use super::{Outcome, RangeArgs, counted};

/// Which recorded turns link to the range, and how — what the Sessions tab is
/// built from, printable, so the linking can be inspected (and driven from a
/// hook) without opening a window.
pub(crate) fn turns(range: &RangeArgs) -> Outcome {
    let target = range.resolve()?;
    let loaded = super::load(&target)?;
    let Ok(repo) = gix::open(&loaded.root) else {
        eprintln!("error: cannot open {}", loaded.root.display());
        return Err(std::process::ExitCode::from(super::BAD_INPUT));
    };

    let diff = sessions::review_diff(&repo, &loaded);
    let turns = sessions::load_turns(&repo, &diff);
    if turns.is_empty() {
        println!(
            "no agent sessions (refs/agent/sessions/*) in {}",
            loaded.root.display()
        );
        return Ok(());
    }

    let via = sessions::matching_turns(&diff, &turns);

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
    let mut loaded = super::load(&range.resolve()?)?;
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
    // Per language: blobs, lines, and the milliseconds spent colouring them.
    let mut per_lang: HashMap<&str, (usize, usize, f64)> = HashMap::new();
    let started = std::time::Instant::now();
    for blob in &mut loaded.blobs {
        let one = std::time::Instant::now();
        let spans = hl.compute(&blob.ext, &blob.text);
        let ms = one.elapsed().as_secs_f64() * 1000.0;
        let (blobs, lines, total) = per_lang.entry(lang_for_ext(&blob.ext)).or_default();
        *blobs += 1;
        *lines += spans.len();
        *total += ms;
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
    // Slowest first.
    let mut ranked: Vec<_> = per_lang.into_iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.2.total_cmp(&a.2));
    for (lang, (blobs, lines, ms)) in ranked {
        let rate = if ms > 0.0 { counted(lines) / ms } else { 0.0 };
        println!("{lang:<12} {blobs:>6} {lines:>9} {ms:>10.1} {rate:>11.0}");
    }
    Ok(())
}
