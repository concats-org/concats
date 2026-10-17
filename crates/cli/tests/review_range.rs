#![cfg(feature = "review")]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::{Command, Output},
};

fn git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn manifest(repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_concats"))
        .current_dir(repo)
        .arg("manifest")
        .output()
        .unwrap()
}

/// A bare command takes its range from the app open on the checkout, and
/// without one it says what to pass rather than guessing.
#[test]
fn a_bare_command_reviews_what_the_app_shows_for_the_checkout() {
    let sandbox = tempfile::tempdir().unwrap();
    let repo = sandbox.path().canonicalize().unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("file.rs"), "old\n").unwrap();
    git(&repo, &["add", "file.rs"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    fs::write(repo.join("file.rs"), "new\n").unwrap();

    let alone = manifest(&repo);
    assert_eq!(alone.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&alone.stderr).contains(concats_state::SOCKET));

    // The app's side of it: one answer, the way it answers `GET /range`.
    let socket = repo.join(concats_state::SOCKET);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let app = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let range = r#"{"repo":".","base":"HEAD","head":"WORKTREE"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{range}",
            range.len()
        )
        .unwrap();
        request
    });

    let shown = manifest(&repo);
    assert_eq!(
        app.join().unwrap().trim(),
        format!("GET {} HTTP/1.1", concats_state::RANGE)
    );
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(stdout.contains("file.rs"), "{stdout}");
}
