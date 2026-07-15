//! Visual snapshot tests for the editor's surfaces.
//!
//! Each test drives the real app headlessly, through the same `CONCATS_APP_*`
//! hooks the capture script uses, and compares the frame to a committed golden
//! pixel for pixel. A caret that fails to land, a line that moves by one pixel
//! or a colour that shifts fails the test. That is what we want: before these,
//! such states could only be checked by looking at a picture.
//!
//! ```text
//! cargo test -p concats-app --test snapshots -- --ignored          # run them
//! SNAPSHOT_BLESS=1 cargo test -p concats-app --test snapshots -- --ignored
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
//! ## Known flake: the two interaction scenarios
//!
//! The three scenarios without a pointer are stable across runs. The two with
//! one, `a_click_places_a_caret` and `typing_at_the_caret_reaches_the_buffer`,
//! still differ between runs. So what is left of the nondeterminism sits in
//! resolving a click to a caret, not in rendering.
//!
//! It used to be all five, for another reason: the colours a frame was drawn
//! from depended on whether the highlight worker had landed. Drawing from one
//! source fixed that and left this.
//!
//! Don't paper over it with a comparison tolerance. Strict equality is there to
//! notice this kind of difference, and a threshold wide enough to hide it would
//! hide most regressions worth catching.
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

use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Logical window size. Fixed, because the layout responds to width — a capture
/// at another size is a different picture, not the same one scaled.
const CANVAS: &str = "1280x880";

/// How long to wait for a cold load, the hook ticks and the capture.
const TIMEOUT: Duration = Duration::from_secs(40);

/// One app at a time: they are real windows competing for the window server,
/// and cargo runs tests in parallel by default. Held for the whole capture.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// How far apart two images may be and still count as matching.
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
/// diff to show rather than a plain listing. Byte-for-byte the fixture the
/// capture script builds, so the goldens correspond.
fn fixture(dir: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git is on PATH");
        assert!(status.success(), "git {args:?} failed");
    };
    std::fs::create_dir_all(dir).expect("fixture dir");
    git(&["init", "-q", "."]);
    git(&["config", "user.email", "design@example.com"]);
    git(&["config", "user.name", "Design"]);
    git(&["config", "commit.gpgsign", "false"]);
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
    git(&["add", "-A"]);
    git(&["commit", "-qm", "initial"]);
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

/// Drive the app once and return the frames it captured: the state as the load
/// left it, and the state after the hooks ran.
fn capture_pair(name: &str, hooks: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let after = capture(name, hooks);
    (after.with_file_name("before.png"), after)
}

/// Drive the app once and return the frame it captured.
fn capture(name: &str, hooks: &[(&str, &str)]) -> PathBuf {
    let _serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
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
    let repo = sandbox.path().join("repo");
    fixture(&repo);
    let shot = work.join("actual.png");

    let mut app = Command::new(env!("CARGO_BIN_EXE_concats-app"));
    app.current_dir(&repo)
        .args(["--repo", ".", "--base", "HEAD", "--head", "WORKTREE"])
        .env("CONCATS_APP_SIZE", CANVAS)
        // NOTE: paint is paced by a CADisplayLink, which only ticks while the
        // window is on an active display. A test run has no such window, so no
        // frame ever presents and `capture_next_frame_to_file` never completes
        // — the app exits having written nothing. Timer pacing still draws.
        .env("MAKEPAD_DISPLAY_LINK", "0")
        .env("CONCATS_APP_SHOT", &shot)
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
        .stderr(std::process::Stdio::null());
    for (key, value) in hooks {
        app.env(key, value);
    }
    let mut running = Running(app.spawn().expect("the app binary is built"));

    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match running.0.try_wait().expect("wait on the app") {
            Some(_) => {
                assert!(
                    image::open(&shot).is_ok(),
                    "{name}: the app exited without a readable frame — see {}",
                    work.display()
                );
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

fn snapshot(name: &str, hooks: &[(&str, &str)]) {
    let actual = capture(name, hooks);
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
fn assert_interaction(name: &str, hooks: &[(&str, &str)]) {
    let (before, after) = capture_pair(name, hooks);
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
            ("CONCATS_APP_CLICK", "200,98"),
        ],
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
            ("CONCATS_APP_CLICK", "200,98"),
            ("CONCATS_APP_TYPE", " // edited"),
        ],
    );
}
