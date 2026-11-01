//! Visual snapshot tests for the editor's surfaces.
//!
//! Each test drives the real app through its `CONCATS_APP_*` hooks and
//! compares the frame to a committed golden pixel for pixel. A caret that
//! fails to land, a line that moves by one pixel or a colour that shifts fails
//! the test. That is what we want: before these, such states could only be
//! checked by looking at a picture.
//!
//! ```text
//! cargo test -p concats-app --features dev-hooks --test snapshots -- --ignored          # run them
//! SNAPSHOT_BLESS=1 cargo test -p concats-app --features dev-hooks --test snapshots -- --ignored
//! ```
//!
//! Ignored by default: every test spawns a GPU process and needs a window
//! server, so it passes on a desktop and fails in headless CI, and `cargo test`
//! must not depend on the machine.
//!
//! On a mismatch the golden, the capture and a diff (differing pixels in
//! magenta) are written next to each other under `target/snapshots/`; the
//! failure message names them.
//!
//! ## Interaction checks
//!
//! The click and typing scenarios assert the caret and dirty-buffer state as
//! well as pixels. Hooks wait for the before-frame to reach disk before sending
//! pointer events, so a capture cannot mistake the initial frame for the result.
//!
//! NOTE: Finish default cargo checks before running this suite. Both builds
//! write the same app executable; rebuilding without dev-hooks between native
//! launches would leave a test driving an app that cannot answer its hooks.
//!
//! ## A few thousand pixels off, in the text, everywhere
//!
//! Changing how much work a frame does can repack the font atlas: glyphs are
//! rasterised into it as they are first drawn, so a different order gives each
//! one a different sub-pixel phase. Every glyph in the window then differs by a
//! unit or two while the backgrounds match exactly. It looks alarming and reads
//! like a colour regression; it is neither.
//!
//! What tells the two apart is whether any difference is solid. A changed
//! colour covers an area, a re-rasterised glyph does not. If no pixel differing
//! by more than ~32 has all four neighbours differing by that much, it is the
//! atlas, and the goldens want re-blessing. If some do, it is a regression, and
//! blessing it would hide what these tests exist for.
//!
//! ## A fixture with no diff hangs
//!
//! `CONCATS_APP_EXIT_AFTER_SHOT` is served from the same tick sequence that
//! waits for a load to land, and that sequence never starts when the range has
//! no changed files. A fixture that forgot to dirty its worktree does not fail;
//! it sits there until [`TIMEOUT`]. The fixture below always edits a file after
//! committing it, and it has to keep doing so.
//!
//! ## Comparing against Figma
//!
//! These goldens are captured from the app, so strict equality is right: they
//! catch regressions. A golden exported from Figma can't be compared this way,
//! because Figma rasterises glyphs through another engine, and identical text
//! at an identical position still differs pixel for pixel. [`Tolerance`] is for
//! that comparison: a per-channel delta and a budget of differing pixels, which
//! catches real misalignment (padding, line height, colour) and ignores how
//! glyphs were rendered.

#![cfg(all(target_os = "macos", feature = "dev-hooks"))]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Mutex,
    time::{Duration, Instant},
};

use concats_fixture::git;

/// Logical window size. Fixed, because the layout responds to width — a capture
/// at another size is a different picture, not the same one scaled.
const CANVAS: &str = "1280x880";

/// How long to wait for a cold load, the hook ticks and the capture.
const TIMEOUT: Duration = Duration::from_secs(40);

/// One app at a time: they are real windows competing for the window server,
/// and cargo runs tests in parallel by default. Held for the whole capture.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// How far apart two images may be and still count as matching.
#[derive(Clone, Copy)]
struct Tolerance {
    /// Per-channel difference ignored entirely.
    channel: u8,
    /// How many pixels may differ by more than that.
    pixels: usize,
}

/// What a captured golden is held to: nothing may differ at all.
const EXACT: Tolerance = Tolerance {
    channel: 0,
    pixels: 0,
};

/// A golden with a focused text field in it: the field's cursor blinks, and
/// whether a frame catches it lit is timing. That cursor, about 120 pixels, is
/// all that may differ.
const BLINKING_CURSOR: Tolerance = Tolerance {
    channel: 0,
    pixels: 200,
};

/// A golden of a stream with its summary line: the line names the base
/// commit, whose id moves with the fixture's commit time, and how long the
/// load took. Those two runs of text, about 1,600 pixels, are all that may
/// differ.
const SUMMARY_LINE: Tolerance = Tolerance {
    channel: 0,
    pixels: 2_500,
};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/app sits two below the workspace root")
        .to_path_buf()
}

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn artifacts() -> PathBuf {
    workspace().join("target/snapshots")
}

/// A repo to capture: one commit, then a worktree edit, so every scenario has a
/// diff to show rather than a plain listing.
fn fixture(dir: &Path) {
    std::fs::create_dir_all(dir).expect("fixture dir");
    git(dir, &["init", "-q", "."]);
    std::fs::write(
        dir.join("editor.rs"),
        "use std::collections::HashMap;\n\
         \n\
         /// Resolve a name to its value, falling back to the environment.\n\
         pub fn resolve(names: &HashMap<String, String>, key: &str) -> Option<String> {\n\
         \x20   if let Some(found) = names.get(key) {\n\
         \x20       return Some(found.clone());\n\
         \x20   }\n\
         \x20   std::env::var(key).ok()\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let mut names = HashMap::new();\n\
         \x20   names.insert(\"greeting\".to_string(), \"hello\".to_string());\n\
         \x20   println!(\"{:?}\", resolve(&names, \"greeting\"));\n\
         }\n",
    )
    .expect("write editor.rs");
    std::fs::write(
        dir.join("README.md"),
        "# Project\n\n## Getting Started\n\nThis paragraph is deliberately one \
         very long unwrapped line, so that the soft wrap scenario has something \
         real to wrap: it keeps going well past any sensible column limit, and \
         then it keeps going some more, so that at least two continuation rows \
         are produced at the capture width.\n\nRun the thing, and it prints a \
         greeting.\n",
    )
    .expect("write README.md");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "initial"]);
    // The worktree change the review is of.
    let edited = std::fs::read_to_string(dir.join("editor.rs"))
        .expect("read back")
        .replace("\"hello\".to_string()", "\"hello, world\".to_string()");
    std::fs::write(dir.join("editor.rs"), edited).expect("dirty the worktree");
}

/// A child killed when it goes out of scope, so a failing assertion cannot leave
/// an app running and wedge every test after it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Drive the app once and return the frame it captured.
fn capture(name: &str, hooks: &[(&str, &str)], setup: impl FnOnce(&Path)) -> PathBuf {
    let _serial = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Two directories, because they have opposite lifetimes.
    //
    // The sandbox is everything the run writes that nobody wants afterwards:
    // the fixture repo and the app's own persisted state, which hangs off HOME.
    // A temp dir, wiped when this returns, so no scenario inherits anything
    // from the one before and none of it reaches the real config.
    //
    // The artifacts are the frames. They have to survive a failing test, which
    // names them so you can look at them.
    let sandbox = tempfile::tempdir().expect("a temp sandbox");
    let work = artifacts().join(name);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("scenario dir");
    // NOTE: a fixed path, not one in the sandbox. The window's outline takes
    // its colour from the checkout's path, so a fresh temp dir per run would
    // repaint it every time. Runs hold ONE_AT_A_TIME, so they never share it.
    let repo = Path::new("/tmp/concats-snapshots/repo");
    let _ = std::fs::remove_dir_all(repo);
    fixture(repo);
    setup(repo);
    let shot = work.join("actual.png");

    let mut app = Command::new(env!("CARGO_BIN_EXE_concats-app"));
    app.current_dir(repo)
        .args(["--repo", ".", "--base", "HEAD", "--head", "WORKTREE"])
        .env("CONCATS_APP_SIZE", CANVAS)
        // NOTE: paint is paced by a CADisplayLink, which only ticks while the
        // window is on an active display. A test run has no such window, so no
        // frame ever presents and `capture_next_frame_to_file` never completes
        // — the app exits having written nothing. Timer pacing still draws.
        .env("MAKEPAD_DISPLAY_LINK", "0")
        // Comment bylines are in local time; one zone, so a stamp reads the
        // same on every machine.
        .env("TZ", "UTC")
        .env("CONCATS_APP_SHOT", &shot)
        .env("CONCATS_APP_STATE", work.join("state.json"))
        // The app exits once it has written the frame, so waiting for it is an
        // exact signal. Waiting on the file is not: the app also captures when
        // the load lands, so the first frame that appears is the state before
        // any hook ran, and a click that works looks just like one that never
        // fired. Waiting for the file to stop changing doesn't help either;
        // these hooks advance per draw, and a sparse redraw can leave it
        // untouched for seconds between the two captures.
        .env("CONCATS_APP_EXIT_AFTER_SHOT", "1")
        // The same run also writes the pre-interaction frame, so a before/after
        // pair costs one launch rather than two — and the two frames come from
        // one session, with no cross-run difference to explain away.
        .env("CONCATS_APP_SHOT_BEFORE", work.join("before.png"))
        // A HOME inside the sandbox, so each scenario starts from a clean app.
        //
        // The dock layout, the recents and the theme are persisted under the
        // config dir, which hangs off HOME. Without this every scenario
        // inherits the one before it (a File tab left open by one changes what
        // the next one draws), and the suite quietly rewrites your real app
        // state as it runs. Both showed up as frames that matched when a test
        // ran alone and not when it ran with the others.
        .env("HOME", sandbox.path())
        .env("XDG_CONFIG_HOME", sandbox.path().join(".config"))
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(work.join("app.log")).expect("app log"));
    for (key, value) in hooks {
        app.env(key, value);
    }
    let mut running = Running(app.spawn().expect("the app binary is built"));

    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match running.0.try_wait().expect("wait on the app") {
            Some(status) => {
                assert!(
                    status.success(),
                    "{name}: app exited with {status}; see {}",
                    work.join("app.log").display()
                );
                assert!(
                    image::open(&shot).is_ok(),
                    "{name}: the app exited without a readable frame — see {}",
                    work.display()
                );
                let layout = repo.join(".git/concats-app-layout-5.ron");
                if layout.exists() {
                    std::fs::copy(layout, work.join("layout.ron")).expect("capture saved layout");
                }
                return shot;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    panic!(
        "{name}: the app did not finish within {TIMEOUT:?} — see {}",
        work.display()
    );
}

/// Compare a capture to its golden, writing the pair and a diff when they differ.
fn assert_matches(name: &str, actual: &Path, tolerance: Tolerance) {
    let golden = goldens().join(format!("{name}.png"));
    if std::env::var("SNAPSHOT_BLESS").is_ok() {
        std::fs::create_dir_all(goldens()).expect("goldens dir");
        // NOTE: re-encode instead of copying the capture. Makepad writes it
        // with deflate off, which is ~110x the bytes of the same pixels.
        image::open(actual)
            .expect("the capture decodes")
            .to_rgba8()
            .save(&golden)
            .expect("write the golden");
        eprintln!("blessed {name}");
        return;
    }
    let Ok(want) = image::open(&golden) else {
        panic!(
            "{name}: no golden at {} — record one with SNAPSHOT_BLESS=1",
            golden.display()
        );
    };
    let got = image::open(actual).expect("the capture decodes");
    let (want, got) = (want.to_rgba8(), got.to_rgba8());
    assert_eq!(
        want.dimensions(),
        got.dimensions(),
        "{name}: captured {:?}, golden is {:?}",
        got.dimensions(),
        want.dimensions()
    );

    let mut diff = got.clone();
    let mut differing = 0usize;
    for (x, y, want) in want.enumerate_pixels() {
        let got = got.get_pixel(x, y);
        let apart = (0..4)
            .map(|c| want.0[c].abs_diff(got.0[c]))
            .max()
            .unwrap_or(0);
        if apart > tolerance.channel {
            differing += 1;
            diff.put_pixel(x, y, image::Rgba([255, 0, 255, 255]));
        }
    }
    if differing > tolerance.pixels {
        let out = actual.with_file_name("diff.png");
        let _ = diff.save(&out);
        let _ = std::fs::copy(&golden, actual.with_file_name("golden.png"));
        panic!(
            "{name}: {differing} pixels differ (budget {}).\n  golden: {}\n  actual: {}\n  diff:   {}",
            tolerance.pixels,
            golden.display(),
            actual.display(),
            out.display()
        );
    }
}

/// The state the app wrote next to `shot`.
fn state(shot: &Path, file: &str) -> serde_json::Value {
    let bytes = std::fs::read(shot.with_file_name(file)).expect(file);
    serde_json::from_slice(&bytes).expect("state JSON")
}

fn snapshot(name: &str, hooks: &[(&str, &str)]) {
    let actual = capture(name, hooks, |_| {});
    assert_matches(name, &actual, EXACT);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn file_tab_clean() {
    snapshot("file-tab-clean", &[("CONCATS_APP_FILE", "editor.rs")]);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn diff_view() {
    snapshot("diff-view", &[]);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn wrapped_long_line() {
    snapshot("wrapped-long-line", &[("CONCATS_APP_FILE", "README.md")]);
}

/// How far apart two captures are, in pixels.
fn distance(a: &Path, b: &Path) -> usize {
    let (a, b) = (
        image::open(a).expect("decodes").to_rgba8(),
        image::open(b).expect("decodes").to_rgba8(),
    );
    assert_eq!(a.dimensions(), b.dimensions(), "captures differ in size");
    a.pixels().zip(b.pixels()).filter(|(a, b)| a != b).count()
}

/// An interaction has to change the frame, and change it into the right one.
/// Neither half is enough on its own:
///
/// - a golden alone can't tell a state that renders from one that never
///   happened. Bless a frame where the click missed and the failure becomes
///   the expectation; fixing the click then breaks the test.
/// - a difference alone says something moved, not that the right thing moved.
///
/// Together they also survive a bad recording: a golden taken from a run where
/// the interaction never fired still fails, because before and after are then
/// identical.
fn assert_interaction(name: &str, hooks: &[(&str, &str)], byte: usize, dirty_buffers: usize) {
    let after = capture(name, hooks, |_| {});
    // The same run wrote the frame as the load left it, before the hooks ran.
    let before = after.with_file_name("before.png");
    let data = state(&after, "state.json");
    assert_eq!(data["caret"], serde_json::json!([1, 2, byte]), "{name}");
    assert_eq!(data["dirty_buffers"], dirty_buffers, "{name}");
    assert!(
        distance(&before, &after) > 0,
        "{name}: the interaction changed nothing on screen — it never happened.\n  \
         before: {}\n  after:  {}",
        before.display(),
        after.display()
    );
    assert_matches(name, &after, EXACT);
}

/// The state that could not be verified before: a click has to place a caret,
/// and the caret has to be where it was last time.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_click_places_a_caret() {
    assert_interaction(
        "caret-placed",
        &[
            ("CONCATS_APP_FILE", "editor.rs"),
            ("CONCATS_APP_CLICK", "206,104"),
        ],
        18,
        0,
    );
}

/// …and typing at it has to reach the buffer, as the same text in the same place.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn typing_at_the_caret_reaches_the_buffer() {
    assert_interaction(
        "typed-edit",
        &[
            ("CONCATS_APP_FILE", "editor.rs"),
            ("CONCATS_APP_CLICK", "206,104"),
            ("CONCATS_APP_TYPE", " // edited"),
        ],
        28,
        1,
    );
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn typing_reaches_a_composer_recreated_after_a_comment_arrives() {
    let shot = capture(
        "composer-refresh",
        &[
            ("CONCATS_APP_COMPOSE", "editor.rs:12:12"),
            ("CONCATS_APP_TYPE", "before"),
            ("CONCATS_APP_COMPOSER_REFRESH", "1"),
            ("CONCATS_APP_TYPE_AFTER_REFRESH", "after"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    let before = state(&shot, "state-before-refresh.json");
    assert_eq!(before["draft"], "before");
    assert!(before["composer_input"].is_number());
    assert!(data["composer_input"].is_number());
    assert_ne!(before["composer_input"], data["composer_input"]);
    assert_eq!(data["draft"], "beforeafter");
    assert_eq!(data["dirty_buffers"], 0);
    assert_eq!(data["composer_open"], true);
    assert_eq!(data["focus_pending"], false);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn an_offscreen_composer_is_revealed_before_typing() {
    let shot = capture(
        "composer-offscreen",
        &[
            ("CONCATS_APP_COMPOSE", "editor.rs:200:200"),
            ("CONCATS_APP_TYPE", "offscreen draft"),
        ],
        |repo| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(repo.join("editor.rs"))
                .unwrap();
            for line in 0..220 {
                writeln!(file, "// fixture line {line}").unwrap();
            }
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["draft"], "offscreen draft");
    assert_eq!(data["dirty_buffers"], 0);
    assert_eq!(data["focus_pending"], false);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn search_stays_in_its_tab_without_a_code_caret() {
    for (name, hooks, expected) in [
        (
            "find-diff",
            vec![("CONCATS_APP_TAB", "files"), ("CONCATS_APP_FIND", "needle")],
            "0/2",
        ),
        (
            "find-file",
            vec![
                ("CONCATS_APP_FILE", "editor.rs,README.md"),
                ("CONCATS_APP_TAB", "file:editor.rs"),
                ("CONCATS_APP_FIND", "needle"),
            ],
            "0/1",
        ),
    ] {
        let shot = capture(name, &hooks, |repo| {
            std::fs::write(repo.join("editor.rs"), "// a needle in the editor\n")
                .expect("editor fixture");
            std::fs::write(repo.join("README.md"), "A needle in the README\n")
                .expect("readme fixture");
        });
        let data = state(&shot, "state.json");
        assert_eq!(data["find_query"], "needle", "{name}");
        assert_eq!(data["find_count"], expected, "{name}");
        assert_eq!(
            data["has_caret"], false,
            "search does not need a code caret"
        );
        assert_eq!(
            data["dirty_buffers"], 0,
            "the query must reach the find input"
        );
        // Find sits in the tab bar, above the rows and their pinned header.
        assert_matches(name, &shot, BLINKING_CURSOR);
    }
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn find_does_not_take_keys_from_the_composer() {
    let shot = capture(
        "find-composer",
        &[
            ("CONCATS_APP_COMPOSE", "editor.rs:12:12"),
            ("CONCATS_APP_TYPE", "before"),
            ("CONCATS_APP_FIND", "after"),
            ("CONCATS_APP_FIND_KEEP_FOCUS", "1"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["find_query"], "");
    assert_eq!(data["draft"], "beforeafter");
    assert_eq!(data["dirty_buffers"], 0);
    assert_eq!(data["composer_open"], true);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn file_browser_search_stays_in_its_own_pane() {
    let shot = capture(
        "file-browser-search",
        &[
            ("CONCATS_APP_CLICK", "1100,70"),
            ("CONCATS_APP_FIND", "deep/target"),
            ("CONCATS_APP_FIND_KEEP_FOCUS", "1"),
        ],
        |repo| {
            std::fs::create_dir(repo.join("deep")).expect("nested folder");
            std::fs::write(repo.join("deep/target.rs"), "target\n").expect("matching file");
            std::fs::write(repo.join("deep/other.rs"), "other\n").expect("unmatched file");
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["file_find_query"], "deep/target");
    assert_eq!(data["file_find_count"], "1 file");
    assert_eq!(data["find_query"], "");
    assert_eq!(data["dirty_buffers"], 0);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
#[expect(
    clippy::format_collect,
    reason = "Generated fixture lines make the seen range explicit."
)]
fn the_pinned_seen_control_marks_its_file() {
    let shot = capture(
        "pinned-seen",
        &[
            ("CONCATS_APP_SCROLL", "60"),
            ("CONCATS_APP_CLICK_SEEN", "1"),
        ],
        |repo| {
            let text = (1..=200)
                .map(|n| format!("// changed line {n}\n"))
                .collect::<String>();
            std::fs::write(repo.join("editor.rs"), text).expect("long file fixture");
        },
    );
    let data = state(&shot, "state.json");
    assert!(
        data["seen_lines"].as_u64().unwrap() >= 200,
        "the header marks the entire file"
    );
    assert_eq!(data["dirty_buffers"], 0);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn file_tab_titles_follow_typing_and_save_acknowledgements() {
    for (name, save) in [("title-dirty", ""), ("title-saved", "1")] {
        let shot = capture(
            name,
            &[
                ("CONCATS_APP_FILE", "editor.rs"),
                ("CONCATS_APP_CLICK", "206,104"),
                ("CONCATS_APP_TYPE", " // edited"),
                ("CONCATS_APP_SAVE", save),
            ],
            |_| {},
        );
        let data = state(&shot, "state.json");
        let before = state(&shot, "state-before-refresh.json");
        assert_eq!(
            before["dirty_buffers"], 1,
            "the native keystroke edited the file"
        );
        let dirty = save.is_empty();
        assert_eq!(data["dirty_buffers"], u64::from(dirty));
        let expected = if dirty {
            "• editor.rs · worktree"
        } else {
            "editor.rs · worktree"
        };
        assert!(
            data["tab_titles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|title| title == expected),
            "{data}"
        );
    }
}

#[test]
#[ignore = "needs a macOS window server"]
fn terminal_search_finds_literal_text_in_scrollback() {
    let shot = capture(
        "terminal-search",
        &[
            ("CONCATS_APP_TERM", "1"),
            ("CONCATS_APP_CLICK", "300,690"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf '[needle] in history\\n'\ni=0\nwhile [ $i -lt 80 ]; do printf 'filler line %s\\n' \"$i\"; i=$((i+1)); done\nread -r answer\n",
            ),
            ("CONCATS_APP_FIND", "[needle]"),
            ("CONCATS_APP_FIND_KEEP_FOCUS", "1"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["terminal_find_query"], "[needle]");
    assert_eq!(data["terminal_selection"], "[needle]");
    assert!(data["terminal_offset"].as_u64().unwrap() > 0);
    assert_eq!(data["find_query"], "");
    assert_eq!(data["file_find_query"], "");
    assert_eq!(data["dirty_buffers"], 0);
}

#[test]
#[ignore = "needs a macOS window server"]
fn find_opens_in_every_pane_at_once() {
    let shot = capture(
        "find-everywhere",
        &[
            ("CONCATS_APP_TERM", "1"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'a needle in history\\n'\nread -r answer\n",
            ),
            (
                "CONCATS_APP_STEPS",
                "click 400,200; find names; click 1100,70; find edit; click 300,690; find needle",
            ),
        ],
        |_| {},
    );
    // Each query reached the find of the pane it was typed in.
    let data = state(&shot, "state.json");
    assert_eq!(data["find_query"], "names");
    assert_eq!(data["file_find_query"], "edit");
    assert_eq!(data["terminal_find_query"], "needle");
}

#[test]
#[ignore = "needs a macOS window server"]
fn escape_closes_terminal_search() {
    let shot = capture(
        "terminal-search-escaped",
        &[
            ("CONCATS_APP_TERM", "1"),
            ("CONCATS_APP_CLICK", "300,690"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'a needle in history\\n'\nread -r answer\n",
            ),
            ("CONCATS_APP_FIND", "needle"),
            ("CONCATS_APP_FIND_KEEP_FOCUS", "1"),
            ("CONCATS_APP_KEY", "escape"),
        ],
        |_| {},
    );
    // The query reached the bar, and the frame shows the bar gone.
    assert_eq!(state(&shot, "state.json")["terminal_find_query"], "needle");
    assert_matches("terminal-search-escaped", &shot, BLINKING_CURSOR);
}

#[test]
#[ignore = "needs a macOS window server"]
fn terminal_draws_combining_marks_and_unicode() {
    let shot = capture(
        "terminal_unicode",
        &[
            ("CONCATS_APP_TERM", "1"),
            ("CONCATS_APP_CLICK", "300,690"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ASCII: hello\\nCombining: e\u{301} a\u{30a} n\u{303}\\nWide: 世界 👩‍💻\\nBraille: ⠁⠃⠇⠏\\nCursor: X\\r\\033[8C'\nread -r answer\n",
            ),
        ],
        |_| {},
    );
    let state = state(&shot, "state.json");
    assert!(
        state["terminal_height"].as_f64().unwrap() > 100.0,
        "terminal must be visible: {state}"
    );
    assert_eq!(
        state["terminal_cursor"], "X",
        "fixture cursor must cover a glyph"
    );
    assert_matches("terminal_unicode", &shot, EXACT);
}

#[test]
#[ignore = "needs a macOS window server"]
fn terminal_link_hover_shows_the_destination() {
    let shot = capture(
        "terminal_link_hover",
        &[
            ("CONCATS_APP_TERM", "1"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf '\\033]8;;https://example.com/review\\007Review changes\\033]8;;\\007\\n'\nread -r answer\n",
            ),
            ("CONCATS_APP_HOVER", "50,673"),
        ],
        |_| {},
    );
    assert_matches("terminal_link_hover", &shot, EXACT);
}

#[test]
#[ignore = "needs a macOS window server"]
fn loading_another_window_keeps_typing_in_the_clicked_window() {
    let shot = capture(
        "two_windows",
        &[
            ("CONCATS_APP_WINDOWS", "2"),
            ("CONCATS_APP_CLICK", "200,257"),
            ("CONCATS_APP_TYPE", "/* primary */"),
        ],
        |_| {},
    );
    let state = state(&shot, "state.json");
    assert_eq!(
        state["windows"],
        serde_json::json!([
            {"loaded": true, "dirty_buffers": 1},
            {"loaded": true, "dirty_buffers": 0},
        ])
    );
}

/// macOS asks the app whether a press drags the window, and every window
/// answers from its own header: one narrower than the first still drags by
/// its header, and its buttons still take their clicks.
#[test]
#[ignore = "needs a macOS window server"]
fn every_window_drags_by_its_own_header() {
    let shot = capture(
        "window_captions",
        &[
            ("CONCATS_APP_WINDOWS", "2"),
            ("CONCATS_APP_WINDOWS_SIZE", "900x700"),
        ],
        |_| {},
    );
    let caption = serde_json::json!({
        "header": true,
        "buttons": [false, false, false, false, false],
        "below": false,
    });
    assert_eq!(
        state(&shot, "state.json")["captions"],
        serde_json::json!([caption, caption])
    );
}

#[test]
#[ignore = "needs a macOS window server"]
fn restoring_the_sidebar_does_not_start_a_hidden_shell() {
    let baseline = capture("layout_baseline", &[], |_| {});
    let layout = std::fs::read(baseline.with_file_name("layout.ron")).expect("saved layout");
    let shot = capture(
        "restored_sidebar",
        &[(
            "CONCATS_APP_TERM_SCRIPT",
            "printf 'unexpected shell startup'\nread -r answer\n",
        )],
        |repo| {
            std::fs::write(repo.join(".git/concats-app-layout-5.ron"), layout)
                .expect("restore layout");
        },
    );
    let state = state(&shot, "state.json");
    assert_eq!(
        state["terminal_cursor"],
        serde_json::Value::Null,
        "a collapsed terminal must not start a shell"
    );
    assert_eq!(state["terminal_height"], 0.0);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_gutter_drag_spans_deleted_and_added_lines() {
    let shot = capture(
        "gutter-drag",
        &[
            ("CONCATS_APP_CLICK", "26,235"),
            ("CONCATS_APP_DRAG_TO", "26,257"),
            ("CONCATS_APP_TYPE", "Review this change"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(
        data["compose_ranges"],
        serde_json::json!([[12, 12], [12, 12]])
    );
    assert_eq!(data["composer_open"], true);
    assert_eq!(data["draft"], "Review this change");
    assert_eq!(data["dirty_buffers"], 0);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_file_tab_uses_the_light_palette() {
    snapshot(
        "light-file-tab",
        &[
            ("CONCATS_APP_FILE", "editor.rs"),
            ("CONCATS_APP_THEME", "Rosé Pine Dawn"),
        ],
    );
}

/// How many of the dock's tabs carry `title`.
fn tabs_titled(data: &serde_json::Value, title: &str) -> usize {
    data["tab_titles"]
        .as_array()
        .expect("tab_titles")
        .iter()
        .filter(|t| *t == title)
        .count()
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn open_file_on_a_card_opens_one_file_tab() {
    // Open file, back to File Diff, Open file again.
    let shot = capture(
        "open-file",
        &[(
            "CONCATS_APP_STEPS",
            "click 899,119; click 77,45; click 899,119",
        )],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "editor.rs · worktree"), 1, "{data}");
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
#[expect(
    clippy::format_collect,
    reason = "Generated fixture lines give the card room to scroll under its header."
)]
fn the_pinned_header_opens_its_file() {
    let shot = capture(
        "open-file-pinned",
        &[
            ("CONCATS_APP_SCROLL", "60"),
            ("CONCATS_APP_CLICK", "877,111"),
        ],
        |repo| {
            let text = (1..=200)
                .map(|n| format!("// changed line {n}\n"))
                .collect::<String>();
            std::fs::write(repo.join("editor.rs"), text).expect("long file fixture");
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "editor.rs · worktree"), 1, "{data}");
}

/// A file the head no longer has offers nothing to open.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_deleted_file_card_has_no_open_button() {
    let actual = capture("deleted-card", &[], |repo| {
        std::fs::remove_file(repo.join("README.md")).expect("delete README.md");
    });
    assert_matches("deleted-card", &actual, EXACT);
}

/// The Guide's cards are the same header: its Open file opens the file too.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn open_file_on_a_guide_card() {
    let shot = capture(
        "open-file-guide",
        &[
            ("CONCATS_APP_TAB", "guide"),
            ("CONCATS_APP_CLICK", "899,269"),
        ],
        |repo| {
            let head = git(repo, &["rev-parse", "HEAD"]);
            concats_review::store::save_guide(
                &repo.join(".git"),
                &concats_review::store::Guide {
                    base: gix::ObjectId::from_hex(head.trim().as_bytes()).expect("HEAD oid"),
                    head: gix::ObjectId::null(gix::hash::Kind::Sha1),
                    created_at: 1,
                    author: None,
                    markdown: "# Tour\n\nThe greeting changes.\n\n[editor.rs](editor.rs#L13)\n"
                        .into(),
                },
            )
            .expect("save the guide");
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "editor.rs · worktree"), 1, "{data}");
}

/// A file of `n` numbered lines, longer than the window.
#[expect(
    clippy::format_collect,
    reason = "Numbered fixture lines make the landed line readable in the capture."
)]
fn numbered(repo: &Path, n: usize) {
    let text: String = (1..=n).map(|i| format!("// line {i}\n")).collect();
    std::fs::write(repo.join("long.rs"), text).expect("long file fixture");
}

/// Opening at a line scrolls there and puts the caret at its start, also when
/// the tab is already open at the top.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_file_opens_at_a_line() {
    let shot = capture(
        "open-at-line",
        &[("CONCATS_APP_FILE", "long.rs,long.rs:120")],
        |repo| numbered(repo, 200),
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["caret"][1], 119, "{data}");
    assert_eq!(data["caret"][2], 0, "{data}");
    assert_matches("open-at-line", &shot, EXACT);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_line_past_the_end_opens_at_the_last_line() {
    let shot = capture(
        "open-past-end",
        &[("CONCATS_APP_FILE", "long.rs:999")],
        |repo| numbered(repo, 200),
    );
    assert_eq!(state(&shot, "state.json")["caret"][1], 199);
}

/// A path an agent printed opens on ⌘-click, at the line it names.
#[test]
#[ignore = "needs a macOS window server"]
fn a_path_in_terminal_output_opens_on_cmd_click() {
    let shot = capture(
        "terminal-path-click",
        &[
            ("CONCATS_APP_TERM", "1"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'editor.rs:12:5\\n'\nread -r answer\n",
            ),
            ("CONCATS_APP_STEPS", "cmd-click 50,673"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "editor.rs · worktree"), 1, "{data}");
    assert_eq!(data["caret"][1], 11, "{data}");
    assert_eq!(data["caret"][2], 4, "{data}");
}

/// ⌘N opens an empty Untitled tab with the keyboard in it; it is a file, not
/// a window.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn new_file_opens_an_untitled_tab_to_type_into() {
    let shot = capture(
        "untitled-typed",
        &[("CONCATS_APP_STEPS", "menu new_file; type fn main() {}")],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "• Untitled"), 1, "{data}");
    assert_eq!(data["caret"][1], 0, "{data}");
    assert_eq!(data["windows"].as_array().map(Vec::len), Some(1), "{data}");
    assert_matches("untitled-typed", &shot, EXACT);
}

#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn new_window_is_shift_cmd_n() {
    let shot = capture(
        "new-window",
        &[("CONCATS_APP_STEPS", "menu new_window")],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["windows"].as_array().map(Vec::len), Some(2), "{data}");
}

/// The first save of an Untitled tab writes where the Save dialog says, and
/// the tab becomes that file's.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn saving_an_untitled_tab_names_it() {
    let shot = capture(
        "untitled-saved",
        &[
            // A draw for the new tab to take the keyboard before the typing.
            (
                "CONCATS_APP_STEPS",
                "menu new_file; wait; wait; type fn main() {}",
            ),
            ("CONCATS_APP_SAVE", "1"),
            ("CONCATS_APP_SAVE_AS", "new.rs"),
        ],
        |_| {},
    );
    let repo = Path::new("/tmp/concats-snapshots/repo");
    assert_eq!(
        std::fs::read_to_string(repo.join("new.rs")).ok().as_deref(),
        Some("fn main() {}")
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "new.rs · worktree"), 1, "{data}");
    assert_eq!(tabs_titled(&data, "Untitled"), 0, "{data}");
    // NOTE: no golden: the save reloads the range, and whether the frame
    // catches the loading card is timing.
}

/// Saving an untouched Untitled tab writes an empty file, and only that one
/// of two Untitled tabs is named.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn an_untouched_untitled_tab_saves_empty() {
    let shot = capture(
        "untitled-empty",
        &[
            (
                "CONCATS_APP_STEPS",
                "menu new_file; menu new_file; wait; wait",
            ),
            ("CONCATS_APP_SAVE", "1"),
            ("CONCATS_APP_SAVE_AS", "empty.txt"),
        ],
        |_| {},
    );
    let repo = Path::new("/tmp/concats-snapshots/repo");
    assert_eq!(
        std::fs::read_to_string(repo.join("empty.txt"))
            .ok()
            .as_deref(),
        Some("")
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "empty.txt · worktree"), 1, "{data}");
    assert_eq!(tabs_titled(&data, "Untitled"), 1, "{data}");
}

/// Cancelling writes nothing, and so does a path outside the checkout; the tab
/// stays Untitled either way.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_refused_save_leaves_the_tab_untitled() {
    for (name, answer) in [
        ("untitled-cancel", "-"),
        ("untitled-outside", "../outside.rs"),
    ] {
        let shot = capture(
            name,
            &[
                (
                    "CONCATS_APP_STEPS",
                    "wait; wait; wait; menu new_file; wait; type x",
                ),
                ("CONCATS_APP_SAVE", "1"),
                ("CONCATS_APP_SAVE_AS", answer),
            ],
            |_| {},
        );
        let data = state(&shot, "state.json");
        assert_eq!(tabs_titled(&data, "• Untitled"), 1, "{name}: {data}");
        assert!(
            !Path::new("/tmp/concats-snapshots/outside.rs").exists(),
            "{name}"
        );
    }
}

/// Where the status bar's panel toggles and Files button sit.
const LEFT_TOGGLE: &str = "1220,866";
const RIGHT_TOGGLE: &str = "1262,866";
const BOTTOM_TOGGLE: &str = "1241,866";
const FILES_BUTTON: &str = "1106,866";

/// The size `state.json` reports for one panel.
fn panel(data: &serde_json::Value, side: &str) -> f64 {
    data["panels"][side].as_f64().expect("panel sizes")
}

/// The left toggle slides an empty left panel open and creates nothing.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_left_toggle_opens_an_empty_panel() {
    let shot = capture(
        "left-panel-open",
        &[("CONCATS_APP_CLICK", LEFT_TOGGLE)],
        |_| {},
    );
    let data = state(&shot, "state.json");
    let before = state(&shot, "state-before-refresh.json");
    assert_eq!(panel(&data, "left"), 260.0, "{data}");
    assert_eq!(data["tab_titles"], before["tab_titles"], "nothing opened");
    assert_matches("left-panel-open", &shot, EXACT);
}

/// The bottom toggle opens the panel on its terminal, whose shell starts as if
/// its tab were pressed.
#[test]
#[ignore = "needs a macOS window server"]
fn the_bottom_toggle_opens_onto_a_live_terminal() {
    let shot = capture(
        "bottom-panel-open",
        &[
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_CLICK", BOTTOM_TOGGLE),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "bottom"), 220.0, "{data}");
    assert_ne!(data["terminal_cursor"], serde_json::Value::Null, "{data}");
}

/// Files brings the browser back after its tab was closed and its panel shut.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn files_reopens_the_browser_and_its_panel() {
    // Close the browser's tab, shut the right panel, press Files.
    let steps = format!("click 1046,45; click {RIGHT_TOGGLE}; click {FILES_BUTTON}");
    let shot = capture("files-button", &[("CONCATS_APP_STEPS", &steps)], |_| {});
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "Files · WORKTREE"), 1, "{data}");
    assert_eq!(panel(&data, "right"), 260.0, "{data}");
}

/// A layout saved with the browser in the left panel comes back there, at its
/// size; one under the old `-4` name is not read at all.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_left_panel_restores_and_old_layouts_are_ignored() {
    let baseline = capture("left-layout-baseline", &[], |_| {});
    let saved = std::fs::read_to_string(baseline.with_file_name("layout.ron")).expect("layout");
    let moved = saved
        .replace(
            "right_tabs:Tabs(\n            tabs:[\n                browser_tab,\n            ],",
            "right_tabs:Tabs(\n            tabs:[\n            ],",
        )
        .replace(
            "left_tabs:Tabs(\n            tabs:[\n            ],",
            "left_tabs:Tabs(\n            tabs:[\n                browser_tab,\n            ],",
        )
        .replace("align:FromA(0)", "align:FromA(240)");
    assert_ne!(moved, saved, "the fixture layout moved the browser");
    let shot = capture("left-layout-restored", &[], |repo| {
        std::fs::write(repo.join(".git/concats-app-layout-5.ron"), &moved).expect("seed -5");
    });
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "left"), 240.0, "{data}");
    assert_matches("left-layout-restored", &shot, EXACT);

    let shot = capture("old-layout-ignored", &[], |repo| {
        std::fs::write(repo.join(".git/concats-app-layout-4.ron"), &moved).expect("seed -4");
    });
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "left"), 0.0, "{data}");
    assert_eq!(panel(&data, "right"), 260.0, "{data}");
}

/// Where the status bar's terminal button sits.
const TERMINAL_BUTTON: &str = "1137,866";

/// A HOME whose config places views as `placement` (TOML under
/// `[app.placement]`), for a test to point the app at.
fn home_with_placement(placement: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a temp HOME");
    let dir = home.path().join("Library/Application Support/concats");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!("[app.placement]\n{placement}\n"),
    )
    .expect("write config");
    home
}

/// The titles in one area's tab bar, as `state.json` lists them.
fn bar(data: &serde_json::Value, area: &str) -> Vec<String> {
    data["bars"][area]
        .as_array()
        .expect("bars")
        .iter()
        .filter_map(|title| title.as_str().map(str::to_owned))
        .collect()
}

/// `>_` with the bottom panel shut opens it on a running terminal.
#[test]
#[ignore = "needs a macOS window server"]
fn the_terminal_button_opens_its_panel() {
    let shot = capture(
        "terminal-button",
        &[
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_CLICK", TERMINAL_BUTTON),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "bottom"), 220.0, "{data}");
    assert_ne!(data["terminal_cursor"], serde_json::Value::Null, "{data}");
}

/// Placed right, the terminal is made in the right panel on a fresh layout,
/// and `>_` opens that panel while the bottom one stays shut.
#[test]
#[ignore = "needs a macOS window server"]
fn a_placed_terminal_opens_where_it_is_placed() {
    let home = home_with_placement("terminal = \"right\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let shot = capture(
        "terminal-placed-right",
        &[
            ("HOME", &home_path),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_CLICK", TERMINAL_BUTTON),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert!(
        bar(&data, "right").contains(&"Terminal".to_string()),
        "{data}"
    );
    assert!(bar(&data, "bottom").is_empty(), "{data}");
    assert_eq!(panel(&data, "bottom"), 0.0, "{data}");
    assert!(panel(&data, "right") > 0.0, "{data}");
}

/// A view dragged into a shut panel opens there on its button, and that panel
/// slides open.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_moved_view_opens_where_it_was_moved() {
    let baseline = capture("moved-view-baseline", &[], |_| {});
    let saved = std::fs::read_to_string(baseline.with_file_name("layout.ron")).expect("layout");
    let moved = saved
        .replace(
            "right_tabs:Tabs(\n            tabs:[\n                browser_tab,\n            ],",
            "right_tabs:Tabs(\n            tabs:[\n            ],",
        )
        .replace(
            "left_tabs:Tabs(\n            tabs:[\n            ],",
            "left_tabs:Tabs(\n            tabs:[\n                browser_tab,\n            ],",
        );
    assert_ne!(moved, saved, "the fixture layout moved the browser");
    let shot = capture(
        "moved-view-opened",
        &[("CONCATS_APP_CLICK", FILES_BUTTON)],
        |repo| {
            std::fs::write(repo.join(".git/concats-app-layout-5.ron"), &moved).expect("seed");
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(bar(&data, "left"), ["Files · WORKTREE"], "{data}");
    assert_eq!(panel(&data, "left"), 260.0, "{data}");
}

/// With a comment in the store, a fresh window opens the Comments tab in the
/// left panel and moves nothing else.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn comments_open_in_the_left_panel() {
    let shot = capture("comments-left", &[], |repo| {
        comment_on(repo, &["rev-parse", "HEAD:editor.rs"], "why hello?");
    });
    let data = state(&shot, "state.json");
    assert_eq!(bar(&data, "left"), ["Comments"], "{data}");
    assert_eq!(bar(&data, "center"), ["File Diff"], "{data}");
    assert_eq!(bar(&data, "right"), ["Files · WORKTREE"], "{data}");
}

/// A placement doesn't move a view the saved layout already holds; the
/// view's next opening follows it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn placement_applies_to_the_next_opening() {
    let baseline = capture("placement-baseline", &[], |_| {});
    let saved = std::fs::read_to_string(baseline.with_file_name("layout.ron")).expect("layout");
    let home = home_with_placement("browser = \"left\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let seed = |repo: &Path| {
        std::fs::write(repo.join(".git/concats-app-layout-5.ron"), &saved).expect("seed");
    };
    let kept = capture("placement-kept", &[("HOME", &home_path)], seed);
    let data = state(&kept, "state.json");
    assert_eq!(bar(&data, "right"), ["Files · WORKTREE"], "{data}");

    // Close the browser's tab, then press Files.
    let steps = format!("click 1046,45; click {FILES_BUTTON}");
    let reopened = capture(
        "placement-reopened",
        &[("HOME", &home_path), ("CONCATS_APP_STEPS", &steps)],
        seed,
    );
    let data = state(&reopened, "state.json");
    assert_eq!(bar(&data, "left"), ["Files · WORKTREE"], "{data}");
    assert!(panel(&data, "left") > 0.0, "{data}");
}

/// A few poll ticks for a panel's slide to finish before the next step.
const SETTLE: &str = "wait; wait; wait; wait; wait";

/// A tab held over each area highlights its drop in that area's hue; the
/// center keeps the accent. The left panel is open and empty. The bottom one
/// is aimed at its tab strip: its terminal takes drags itself, for files.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_dragged_tab_tints_its_drop_by_area() {
    for (name, steps) in [
        ("drag-center", "drag browser_tab 500,300".to_string()),
        ("drag-right", "drag files_tab 1150,300".to_string()),
        (
            "drag-bottom",
            format!("click {BOTTOM_TOGGLE}; {SETTLE}; drag files_tab 700,650"),
        ),
        (
            "drag-left-empty",
            format!("click {LEFT_TOGGLE}; {SETTLE}; drag files_tab 130,300"),
        ),
    ] {
        let shot = capture(
            name,
            &[
                // A fixed shell, so the bottom panel's terminal draws the
                // same every run and never someone's own prompt.
                (
                    "CONCATS_APP_TERM_SCRIPT",
                    "printf 'ready'\nread -r answer\n",
                ),
                ("CONCATS_APP_STEPS", &steps),
            ],
            |_| {},
        );
        assert_matches(name, &shot, EXACT);
    }
}

/// A light theme has the panel hues too.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_light_theme_tints_drops_too() {
    let shot = capture(
        "drag-right-light",
        &[
            ("CONCATS_APP_THEME", "Rosé Pine Dawn"),
            ("CONCATS_APP_STEPS", "drag files_tab 1150,300"),
        ],
        |_| {},
    );
    assert_matches("drag-right-light", &shot, EXACT);
}

/// Dropping still moves the tab, here into the right panel.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn dropping_a_tab_in_a_panel_moves_it_there() {
    let shot = capture(
        "drop-right",
        &[(
            "CONCATS_APP_STEPS",
            "drag files_tab 1150,300; drop files_tab 1150,300",
        )],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert!(
        bar(&data, "right").contains(&"File Diff".to_string()),
        "{data}"
    );
}

/// A HOME whose config.toml holds `config`, for a test to point the app at.
fn home_with_config(config: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a temp HOME");
    let dir = home.path().join("Library/Application Support/concats");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("write config");
    home
}

/// Three File tabs and the keys pressed after them: what the center shows.
fn center_after_keys(name: &str, keys: &str) -> serde_json::Value {
    let shot = capture(
        name,
        &[
            ("CONCATS_APP_FILE", "editor.rs,README.md,long.rs"),
            ("CONCATS_APP_STEPS", keys),
        ],
        |repo| numbered(repo, 20),
    );
    state(&shot, "state.json")
}

/// ⌘W closes the current tab, one per press.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn cmd_w_closes_one_tab_per_press() {
    let once = center_after_keys("close-once", "key cmd-w");
    assert_eq!(
        bar(&once, "center"),
        ["File Diff", "editor.rs · worktree", "README.md · worktree"],
        "{once}"
    );
    let thrice = center_after_keys("close-thrice", "key cmd-w; key cmd-w; key cmd-w");
    assert_eq!(bar(&thrice, "center"), ["File Diff"], "{thrice}");
}

/// ⌥⌘T keeps only the current tab (and the last stream tab, which always
/// stays); ⌘K ⌘W closes all it can.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn closing_others_and_all() {
    let others = center_after_keys("close-others", "key alt-cmd-t");
    assert_eq!(
        bar(&others, "center"),
        ["File Diff", "long.rs · worktree"],
        "{others}"
    );
    let all = center_after_keys("close-all", "key cmd-k cmd-w");
    assert_eq!(bar(&all, "center"), ["File Diff"], "{all}");
    assert_eq!(all["pending_chord"], false, "{all}");
}

/// ⌘K U closes the File tabs with nothing unsaved and keeps the one typed in.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn closing_saved_tabs_keeps_unsaved_work() {
    let data = center_after_keys("close-saved", "click 206,104; type x; key cmd-k u");
    assert_eq!(
        bar(&data, "center"),
        ["File Diff", "• long.rs · worktree"],
        "{data}"
    );
}

/// ⌥⌘→ and ⌥⌘← walk the current bar and wrap; ⌃2 picks its second tab.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn keys_walk_and_pick_tabs() {
    let next = center_after_keys("tab-next", "key alt-cmd-right");
    assert_eq!(next["current_tab"], "File Diff", "wraps: {next}");
    let back = center_after_keys("tab-previous", "key alt-cmd-left");
    assert_eq!(back["current_tab"], "README.md · worktree", "{back}");
    let second = center_after_keys("tab-second", "key ctrl-2");
    assert_eq!(second["current_tab"], "editor.rs · worktree", "{second}");
}

/// With the terminal focused, ⌘W closes the terminal's tab.
#[test]
#[ignore = "needs a macOS window server"]
fn cmd_w_closes_a_focused_terminal() {
    let shot = capture(
        "close-terminal",
        &[
            ("CONCATS_APP_TERM", "1"),
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_STEPS", "click 300,690; key cmd-w"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert!(bar(&data, "bottom").is_empty(), "{data}");
    assert_eq!(bar(&data, "center"), ["File Diff"], "{data}");
}

/// ⇧⌘N opens a window; ⇧⌘W closes the focused one.
#[test]
#[ignore = "needs a macOS window server"]
fn shift_cmd_w_closes_the_focused_window() {
    let opened = capture(
        "open-window",
        &[("CONCATS_APP_STEPS", "key cmd-shift-n; wait; wait")],
        |_| {},
    );
    let data = state(&opened, "state.json");
    assert_eq!(data["windows"].as_array().map(Vec::len), Some(2), "{data}");
    let shot = capture(
        "close-window",
        &[(
            "CONCATS_APP_STEPS",
            "key cmd-shift-n; wait; wait; key cmd-shift-w; wait",
        )],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["windows"].as_array().map(Vec::len), Some(1), "{data}");
}

/// Keys rebound in config.toml: ⌘W unbound does nothing, ⌘E closes.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn rebound_keys_follow_the_config() {
    let home = home_with_config("[app.keys]\n\"cmd-w\" = \"\"\n\"cmd-e\" = \"close_tab\"\n");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let shot = capture(
        "rebound-keys",
        &[
            ("HOME", &home_path),
            ("CONCATS_APP_FILE", "editor.rs,README.md"),
            ("CONCATS_APP_STEPS", "key cmd-w; key cmd-e"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(
        bar(&data, "center"),
        ["File Diff", "editor.rs · worktree"],
        "{data}"
    );
}

/// The panel keys only open and shut their panels.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn panel_keys_toggle_their_panels() {
    let steps = format!("key cmd-b; {SETTLE}; key cmd-j; {SETTLE}; key alt-cmd-b; {SETTLE}");
    let shot = capture(
        "panel-keys",
        &[
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_STEPS", &steps),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "left"), 260.0, "{data}");
    assert_eq!(panel(&data, "bottom"), 220.0, "{data}");
    assert_eq!(panel(&data, "right"), 0.0, "{data}");
    assert_eq!(bar(&data, "left"), Vec::<String>::new(), "{data}");
}

/// The terminal key opens the bottom panel on a running terminal; the new
/// terminal key adds another.
#[test]
#[ignore = "needs a macOS window server"]
fn terminal_keys_open_and_add_terminals() {
    let shot = capture(
        "terminal-keys",
        &[
            (
                "CONCATS_APP_TERM_SCRIPT",
                "printf 'ready'\nread -r answer\n",
            ),
            ("CONCATS_APP_STEPS", "key ctrl-`; wait; key ctrl-shift-`"),
        ],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(panel(&data, "bottom"), 220.0, "{data}");
    assert_ne!(data["terminal_cursor"], serde_json::Value::Null, "{data}");
    assert_eq!(bar(&data, "bottom"), ["Terminal", "Terminal 2"], "{data}");
}

/// ⇧⌘E puts the keyboard in the browser, so ⌘F opens its filter.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_browser_key_takes_the_keyboard() {
    let shot = capture(
        "browser-key",
        &[("CONCATS_APP_STEPS", "key cmd-shift-e; wait; find README")],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["file_find_query"], "README", "{data}");
}

/// ⌘O opens the repo picker on the keyboard: arrows walk its rows, Enter
/// opens one, Escape closes it untouched.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_repo_picker_works_from_the_keyboard() {
    // A second checkout with a change, so its review has a diff to land.
    let linked = |repo: &Path| {
        let _ = std::fs::remove_dir_all(repo.join("../linked"));
        git(repo, &["worktree", "add", "-q", "-b", "side", "../linked"]);
        std::fs::write(repo.join("../linked/README.md"), "changed\n").expect("dirty it");
    };
    let shot = capture(
        "repo-picker-keys",
        &[(
            "CONCATS_APP_STEPS",
            "key cmd-o; key down; key down; key enter; wait; wait; wait",
        )],
        linked,
    );
    let data = state(&shot, "state.json");
    assert!(
        data["range"]["repo"]
            .as_str()
            .is_some_and(|repo| repo.ends_with("linked")),
        "{data}"
    );
    let shot = capture(
        "repo-picker-escape",
        &[(
            "CONCATS_APP_STEPS",
            "key cmd-o; key escape; key down; key enter; wait; wait",
        )],
        linked,
    );
    let data = state(&shot, "state.json");
    assert!(
        data["range"]["repo"]
            .as_str()
            .is_some_and(|repo| repo.ends_with("repo")),
        "{data}"
    );
}

/// The base picker, reached by its command, walks its rows by arrow and loads
/// the highlighted one on Enter.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_base_picker_walks_by_arrow() {
    let shot = capture(
        "base-picker-keys",
        &[(
            "CONCATS_APP_STEPS",
            "menu pick_base; key down; key enter; wait; wait; wait",
        )],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert_ne!(data["range"]["base"], "HEAD", "{data}");
}

/// ⌘R loads the range again, like ↻.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn reload_key_loads_again() {
    let shot = capture(
        "reload-key",
        &[("CONCATS_APP_STEPS", "key cmd-r; wait; wait; wait; wait")],
        |_| {},
    );
    let data = state(&shot, "state.json");
    assert!(data["range"]["loads"].as_u64() >= Some(2), "{data}");
}

/// A long editor.rs changed in six places, far enough apart to be six hunks
/// and a card taller than the window, and README.md changed too.
fn six_hunks(repo: &Path) {
    let text = |changed: &[usize]| -> String {
        (1..=120)
            .map(|n| {
                if changed.contains(&n) {
                    format!("// edited {n}\n")
                } else {
                    format!("// line {n}\n")
                }
            })
            .collect()
    };
    std::fs::write(repo.join("editor.rs"), text(&[])).expect("base");
    git(repo, &["commit", "-qam", "long editor"]);
    std::fs::write(repo.join("editor.rs"), text(&[10, 30, 50, 70, 90, 110])).expect("six edits");
    std::fs::write(repo.join("README.md"), "# Changed\n").expect("second card");
}

/// ⌥⌘↓ lands where the tab bar's down arrow does.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_hunk_key_lands_where_the_arrow_does() {
    let key = capture(
        "hunk-key",
        &[("CONCATS_APP_STEPS", "key alt-cmd-down; key alt-cmd-down")],
        six_hunks,
    );
    let arrow = capture(
        "hunk-arrow",
        &[("CONCATS_APP_STEPS", "click 995,72; click 995,72")],
        six_hunks,
    );
    assert_eq!(
        distance(&key, &arrow),
        0,
        "the key and the arrow land apart"
    );
    let unmoved = capture("hunk-none", &[], six_hunks);
    assert!(distance(&key, &unmoved) > 0, "the key moved nothing");
}

/// ⇧⌥⌘↓ brings the next card's header up; on the pinned card, ⌘K P ticks it
/// seen and ⌘K J folds it, and ⌘K H turns Collapse seen on.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn card_keys_act_on_the_pinned_card() {
    let next = capture(
        "card-next",
        // The key walks from where the list stood in its last draw: let it
        // draw first.
        &[(
            "CONCATS_APP_STEPS",
            "wait; wait; key shift-alt-cmd-down; wait; wait",
        )],
        six_hunks,
    );
    let data = state(&next, "state.json");
    assert_eq!(data["cards"]["current"], "editor.rs", "{data}");
    // README.md sorts first, so its header is the one pinned at the start.
    let acted = capture(
        "card-keys",
        &[(
            "CONCATS_APP_STEPS",
            "wait; key cmd-k p; wait; key cmd-k j; wait; key cmd-k h",
        )],
        six_hunks,
    );
    let data = state(&acted, "state.json");
    assert!(data["seen_lines"].as_u64() > Some(0), "{data}");
    assert_eq!(
        data["cards"]["folded"],
        serde_json::json!(["README.md"]),
        "{data}"
    );
    assert_eq!(data["cards"]["collapse_seen"], true, "{data}");
}

/// ⌘K L comments on the selection: the composer opens and takes typing;
/// Escape cancels it like the Cancel button, Enter posts it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn comment_key_opens_the_composer_on_the_selection() {
    let select = "click 300,257; key shift-right; key shift-right; key shift-right; key cmd-k l";
    let typed = format!("{select}; wait; wait; type nice");
    let opened = capture("comment-key-open", &[("CONCATS_APP_STEPS", &typed)], |_| {});
    let data = state(&opened, "state.json");
    assert_eq!(data["composer_open"], true, "{data}");
    assert_eq!(data["draft"], "nice", "{data}");
    let escaped = capture(
        "comment-key-escape",
        &[("CONCATS_APP_STEPS", &format!("{typed}; key escape"))],
        |_| {},
    );
    let data = state(&escaped, "state.json");
    assert_eq!(data["composer_open"], false, "{data}");
    assert_eq!(data["comments"], 0, "{data}");
    let posted = capture(
        "comment-key-post",
        &[("CONCATS_APP_STEPS", &format!("{typed}; key enter; wait"))],
        |_| {},
    );
    let data = state(&posted, "state.json");
    assert_eq!(data["composer_open"], false, "{data}");
    assert_eq!(data["comments"], 1, "{data}");
}

/// ⌘G walks find's matches with the keyboard in the list, not in find.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn find_next_works_from_the_list() {
    let shot = capture(
        "find-next-key",
        &[(
            "CONCATS_APP_STEPS",
            "click 300,600; find edited; wait; click 300,600; key cmd-g; key cmd-g",
        )],
        six_hunks,
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["find_query"], "edited", "{data}");
    // The second ⌘G is on the second match, `edited 30`.
    assert_eq!(data["caret"][1], 29, "{data}");
    assert_eq!(data["caret"][2], 3, "{data}");
}

/// ⇧⌘P opens the palette with the keyboard in it; Escape closes it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_palette_opens_and_closes_by_key() {
    let open = capture(
        "palette-open",
        &[(
            "CONCATS_APP_STEPS",
            "wait; wait; wait; key cmd-shift-p; wait",
        )],
        |_| {},
    );
    let data = state(&open, "state.json");
    assert_eq!(data["picker_open"], true, "{data}");
    assert_matches("palette-open", &open, BLINKING_CURSOR);
    let closed = capture(
        "palette-closed",
        &[(
            "CONCATS_APP_STEPS",
            "wait; wait; wait; key cmd-shift-p; wait; key escape",
        )],
        |_| {},
    );
    let data = state(&closed, "state.json");
    assert_eq!(data["picker_open"], false, "{data}");
}

/// Typing in the palette finds a command and Enter runs it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn the_palette_runs_what_was_typed() {
    let data = center_after_keys(
        "palette-run",
        "wait; wait; wait; key cmd-shift-p; wait; type clo al; key enter; wait",
    );
    assert_eq!(bar(&data, "center"), ["File Diff"], "{data}");
    assert_eq!(data["picker_open"], false, "{data}");
}

/// `body` on the 13th line of the editor.rs blob `blob` names, stored long
/// before the app starts: stamped now, a launch within the same second would
/// count it as current.
fn comment_on(repo: &Path, blob: &[&str], body: &str) {
    let blob = git(repo, blob);
    let mut store = concats_review::store::Store::open(&repo.join(".git"));
    store
        .add_comment(concats_review::store::Comment {
            body: body.into(),
            created_at: 1,
            ..concats_review::store::comment(
                "editor.rs".into(),
                concats_review::store::Anchor {
                    blob: gix::ObjectId::from_hex(blob.trim().as_bytes()).expect("blob oid"),
                    start: 12,
                    end: 12,
                },
            )
        })
        .expect("store the comment");
}

/// A thread from before the window loaded its range draws its bar washed
/// out; one posted since keeps the comment blue.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn earlier_threads_are_washed_out() {
    // The stored thread pushes line 13 down to here.
    let select = "click 300,340; key shift-right; key shift-right; key cmd-k l";
    let shot = capture(
        "comments-earlier",
        &[(
            "CONCATS_APP_STEPS",
            &format!("wait; wait; {select}; wait; wait; type now; key enter; wait"),
        )],
        |repo| comment_on(repo, &["rev-parse", "HEAD:editor.rs"], "why hello?"),
    );
    assert_eq!(state(&shot, "state.json")["comments"], 2);
    assert_matches("comments-earlier", &shot, EXACT);
}

/// A Comments card's Open file puts the caret on its thread's first line, in
/// the checkout's file.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn open_file_on_a_thread_card() {
    let shot = capture(
        "open-file-thread",
        &[
            // The left panel holds Comments, shut until ⌘B; its card's Open
            // file is here then.
            (
                "CONCATS_APP_STEPS",
                "wait; wait; wait; key cmd-b; wait; wait; click 193,324; wait; wait",
            ),
        ],
        |repo| comment_on(repo, &["hash-object", "editor.rs"], "why hello?"),
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "editor.rs · worktree"), 1, "{data}");
    assert_eq!(data["caret"][1], 12, "{data}");
}

/// More than five lines at any width.
const LONG_COMMENT: &str = "Three things before this lands:\n\
    1. The fallback reads the environment on every miss, so a typo in a key silently \
    picks up whatever the shell has.\n\
    2. Make it explicit at the call site, say resolve_or_env, and keep resolve strict.\n\
    3. The tests can then stop setting variables to reach the happy path, which is what \
    made the last run flaky.\n\n\
    Happy to pair on it.";

/// A long comment stops at five lines in the Comments tab, with Show more;
/// Show more lets the rest out. In the center: a panel slides open, and the
/// list's scroll thumb ends wherever the slide's last frame left it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_long_comment_is_clamped_in_the_comments_tab() {
    let home = home_with_placement("comments = \"center\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let long = |repo: &Path| comment_on(repo, &["hash-object", "editor.rs"], LONG_COMMENT);
    let hooks = |steps: &'static str| [("HOME", home_path.as_str()), ("CONCATS_APP_STEPS", steps)];
    let shot = capture(
        "comments-clamped",
        &hooks("wait; wait; wait; key alt-cmd-1; wait; wait"),
        long,
    );
    assert_eq!(
        state(&shot, "state.json")["expanded_comments"],
        serde_json::json!([])
    );
    assert_matches("comments-clamped", &shot, EXACT);

    let shot = capture(
        "comments-expanded",
        &hooks("wait; wait; wait; key alt-cmd-1; wait; wait; click 77,442; wait; wait"),
        long,
    );
    assert_eq!(
        state(&shot, "state.json")["expanded_comments"],
        serde_json::json!([1])
    );
    assert_matches("comments-expanded", &shot, EXACT);

    // Show less puts it back. Not compared to the clamped golden: the button
    // keeps the highlight the click left on it, as Open file does.
    let shot = capture(
        "comments-collapsed",
        &hooks(
            "wait; wait; wait; key alt-cmd-1; wait; wait; click 77,442; wait; wait; \
             click 77,464; wait; wait",
        ),
        long,
    );
    assert_eq!(
        state(&shot, "state.json")["expanded_comments"],
        serde_json::json!([])
    );
}

/// Past two replies, the Comments tab folds the ones between the first and
/// the last into "N more replies", which shows them.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_long_thread_folds_its_middle_replies() {
    let home = home_with_placement("comments = \"center\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let thread = |repo: &Path| {
        comment_on(repo, &["hash-object", "editor.rs"], "why hello?");
        let mut store = concats_review::store::Store::open(&repo.join(".git"));
        // An hour apart, so each byline shows its own reply's stamp.
        for (hour, body) in [
            "The greeting is configurable now.",
            "Configurable from where?",
            "The environment, through resolve.",
            "Then the fallback wants a test.",
        ]
        .into_iter()
        .enumerate()
        {
            let at = 3_600 * (hour as u64 + 1);
            store
                .reply_comment(1, body.into(), None, at, None)
                .expect("store the reply");
        }
    };
    let hooks = |steps: &'static str| [("HOME", home_path.as_str()), ("CONCATS_APP_STEPS", steps)];
    let shot = capture(
        "replies-folded",
        &hooks("wait; wait; wait; key alt-cmd-1; wait; wait"),
        thread,
    );
    assert_eq!(
        state(&shot, "state.json")["expanded_threads"],
        serde_json::json!([])
    );
    assert_matches("replies-folded", &shot, EXACT);

    let shot = capture(
        "replies-shown",
        &hooks("wait; wait; wait; key alt-cmd-1; wait; wait; click 111,453; wait; wait"),
        thread,
    );
    assert_eq!(
        state(&shot, "state.json")["expanded_threads"],
        serde_json::json!([1])
    );
    assert_matches("replies-shown", &shot, EXACT);
}

/// A thread card shows the lines its thread is on; the bands either side hold
/// the code around them, and clicking one shows it.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_thread_card_shows_the_code_around_it_on_a_click() {
    let home = home_with_placement("comments = \"center\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let shot = capture(
        "thread-context",
        &[
            ("HOME", &home_path),
            (
                "CONCATS_APP_STEPS",
                "wait; wait; wait; key alt-cmd-1; wait; wait; click 35,228; wait; wait",
            ),
        ],
        |repo| comment_on(repo, &["hash-object", "editor.rs"], "why hello?"),
    );
    assert_matches("thread-context", &shot, EXACT);
}

/// A comment another process stores while the window is open — `concats
/// comments add`, a second window — shows up on the next poll, in the diff
/// and as a Comments tab, without the range loading again.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_comment_added_elsewhere_arrives_without_a_reload() {
    let before = artifacts().join("comment-arrives/before.png");
    let shot = capture(
        "comment-arrives",
        &[("CONCATS_APP_STEPS", "wait; wait; wait; wait; wait; wait")],
        |repo| {
            let repo = repo.to_path_buf();
            // Stored once the first frame has presented, so it can only
            // arrive through the poll, not the load.
            std::thread::spawn(move || {
                let deadline = Instant::now() + TIMEOUT;
                while !before.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                comment_on(&repo, &["hash-object", "editor.rs"], "from the CLI");
            });
        },
    );
    let data = state(&shot, "state.json");
    assert_eq!(data["comments"], 1, "{data}");
    assert_eq!(data["range"]["loads"], 1, "no reload: {data}");
    assert_eq!(bar(&data, "left"), ["Comments"], "{data}");
}

/// A Comments card folds to its header on its own: the other card on the
/// same file, and the file's card in the diff, stay open.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_comments_card_folds_on_its_own() {
    let home = home_with_placement("comments = \"center\"");
    let home_path = home.path().to_str().expect("utf-8 temp path").to_owned();
    let two_threads = |repo: &Path| {
        comment_on(repo, &["hash-object", "editor.rs"], "why hello?");
        let blob = git(repo, &["hash-object", "editor.rs"]);
        concats_review::store::Store::open(&repo.join(".git"))
            .add_comment(concats_review::store::Comment {
                body: "Is HashMap the right map here?".into(),
                created_at: 1,
                ..concats_review::store::comment(
                    "editor.rs".into(),
                    concats_review::store::Anchor {
                        blob: gix::ObjectId::from_hex(blob.trim().as_bytes()).expect("blob oid"),
                        start: 0,
                        end: 0,
                    },
                )
            })
            .expect("store the comment");
    };
    let shot = capture(
        "comments-card-fold",
        &[
            ("HOME", &home_path),
            (
                "CONCATS_APP_STEPS",
                "wait; wait; wait; key alt-cmd-1; wait; wait; click 37,205; wait; wait",
            ),
        ],
        two_threads,
    );
    let data = state(&shot, "state.json");
    // The first card's thread is the second comment stored.
    assert_eq!(
        data["cards"]["folded"],
        serde_json::json!(["editor.rs#2"]),
        "{data}"
    );
    assert_matches("comments-card-fold", &shot, EXACT);
}

/// A guide whose prose uses every Markdown element the review list renders.
fn markdown_guide(repo: &Path) {
    let head = git(repo, &["rev-parse", "HEAD"]);
    let markdown = "# Tour of the greeting\n\n\
        ## What changed\n\n\
        The greeting now **says** `hello, world` instead of *hello*, and \
        ***both*** matter; ~~nothing else~~ moved.\n\n\
        1. The literal in `main`\n\
        2. What reads it:\n\
        \x20  - `resolve`, falling back to the environment\n\
        \x20  - nothing else\n\n\
        > Small on purpose: the next commit tests the fallback.\n\n\
        ```rust\nlet greeting = resolve(&names, \"greeting\");\n```\n\n\
        See [the README](README.md) for how to run it.  \n\
        This line follows a hard break.\n\n\
        ---\n\n\
        [editor.rs](editor.rs#L13)\n";
    concats_review::store::save_guide(
        &repo.join(".git"),
        &concats_review::store::Guide {
            base: gix::ObjectId::from_hex(head.trim().as_bytes()).expect("HEAD oid"),
            head: gix::ObjectId::null(gix::hash::Kind::Sha1),
            created_at: 1,
            author: None,
            markdown: markdown.into(),
        },
    )
    .expect("save the guide");
}

/// Guide prose is rendered Markdown, not its source.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn guide_prose_renders_its_markdown() {
    let shot = capture(
        "guide-markdown",
        &[("CONCATS_APP_TAB", "guide")],
        markdown_guide,
    );
    assert_matches("guide-markdown", &shot, SUMMARY_LINE);
}

/// A link in guide prose opens what it names: a file in the checkout, in a
/// File tab.
#[test]
#[ignore = "spawns a GPU process; run with --ignored on a desktop"]
fn a_link_in_guide_prose_opens_its_file() {
    let shot = capture(
        "guide-link",
        &[
            ("CONCATS_APP_TAB", "guide"),
            (
                "CONCATS_APP_STEPS",
                "wait; wait; wait; click 86,428; wait; wait",
            ),
        ],
        markdown_guide,
    );
    let data = state(&shot, "state.json");
    assert_eq!(tabs_titled(&data, "README.md · worktree"), 1, "{data}");
}
