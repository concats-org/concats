//! The environment a shell in a concats terminal starts with: the one a
//! terminal of its own gives it, not the one the app was launched from.
//!
//! Started from Ghostty or inside tmux, the app carries that terminal's
//! variables, and programs read them to pick their features: Claude Code
//! drops to 256 colours under `TMUX`, Codex picks its keyboard mode from
//! `TERM_PROGRAM` and `TMUX`. Those have to go rather than be emptied, since
//! an empty `TMUX` still says tmux, and a PTY's options can only add
//! variables. So a shell that inherits any starts through `env -u`.

use std::{collections::HashMap, path::Path};

use alacritty_terminal::tty;

/// Variables that describe the terminal the app was launched from.
const FOREIGN: [&str; 3] = ["TMUX", "TMUX_PANE", "TERMINFO"];
/// Prefixes of the same, one per terminal that sets a family of them.
const FOREIGN_PREFIXES: [&str; 3] = ["GHOSTTY_", "ITERM_", "KITTY_"];

/// What a new shell starts with, read off this process: the variables to set,
/// and the shell to start. `script` is `/bin/sh`'s arguments when a test wants
/// a shell of its own; otherwise it is the user's login shell, as alacritty
/// starts it when given none, behind `env -u` when anything has to go. That
/// needs the user and shell from the environment; without them the shell
/// starts as alacritty would, foreign variables and all.
pub fn shell_from_env(
    script: Option<Vec<String>>,
) -> (HashMap<String, String>, Option<tty::Shell>) {
    let (set, foreign) = shell_env(std::env::vars());
    let shell = match script {
        Some(args) => Some(without(&foreign, "/bin/sh", args)),
        None if foreign.is_empty() => None,
        None => {
            let var = |name| std::env::var(name).ok();
            let (shell, user, home) = (var("SHELL"), var("USER"), var("HOME"));
            shell.zip(user).zip(home).map(|((shell, user), home)| {
                let quiet = Path::new(&home).join(".hushlogin").exists();
                let (program, args) = login_command(&shell, &user, quiet);
                without(&foreign, &program, args)
            })
        }
    };
    (set, shell)
}

/// What a shell's environment changes from `parent`: the variables to set,
/// and the inherited ones to remove, sorted.
pub fn shell_env(
    parent: impl IntoIterator<Item = (String, String)>,
) -> (HashMap<String, String>, Vec<String>) {
    let parent: HashMap<String, String> = parent.into_iter().collect();
    // NOTE: the terminfo entry every system has; the `alacritty` one ships
    // with alacritty, not with us.
    let mut set = HashMap::from([
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        ("TERM_PROGRAM".to_string(), "concats".to_string()),
        (
            "TERM_PROGRAM_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
    ]);
    // An app started from the Finder has no locale, and a shell without one
    // treats every byte above 127 as unprintable.
    if !parent.contains_key("LANG") {
        set.insert("LANG".to_string(), "en_US.UTF-8".to_string());
    }
    let mut remove: Vec<String> = parent
        .into_keys()
        .filter(|name| {
            FOREIGN.contains(&name.as_str())
                || FOREIGN_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    remove.sort();
    (set, remove)
}

/// `program args…` started with the `remove` variables unset.
pub fn without(remove: &[String], program: &str, args: Vec<String>) -> tty::Shell {
    if remove.is_empty() {
        return tty::Shell::new(program.to_string(), args);
    }
    let unset = remove
        .iter()
        .flat_map(|name| ["-u".to_string(), name.clone()]);
    let command = [program.to_string()].into_iter().chain(args);
    tty::Shell::new("/usr/bin/env".to_string(), unset.chain(command).collect())
}

/// alacritty's macOS login: `login` makes the shell a tty session, and the
/// `exec -a -name` makes it a login shell, which `-l` stops `login` doing.
/// `quiet` is whether the user has a `~/.hushlogin`.
#[cfg(target_os = "macos")]
fn login_command(shell: &str, user: &str, quiet: bool) -> (String, Vec<String>) {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    let flags = if quiet { "-qflp" } else { "-flp" };
    let exec = format!("exec -a -{name} {shell}");
    let args = [flags, user, "/bin/zsh", "-fc", &exec].map(String::from);
    ("/usr/bin/login".to_string(), args.to_vec())
}

#[cfg(not(target_os = "macos"))]
fn login_command(shell: &str, _user: &str, _quiet: bool) -> (String, Vec<String>) {
    (shell.to_string(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_shell_says_it_runs_in_concats_and_forgets_the_launching_terminal() {
        let (set, remove) = shell_env(vars(&[
            ("TERM_PROGRAM", "ghostty"),
            ("TMUX", ""),
            ("TMUX_PANE", "%1"),
            ("TERMINFO", "/Applications/Ghostty.app/terminfo"),
            ("GHOSTTY_RESOURCES_DIR", "/x"),
            ("ITERM_SESSION_ID", "w0"),
            ("KITTY_WINDOW_ID", "1"),
            ("HOME", "/Users/a"),
        ]));
        assert_eq!(set["TERM_PROGRAM"], "concats");
        assert_eq!(set["TERM"], "xterm-256color");
        assert_eq!(set["LANG"], "en_US.UTF-8");
        assert_eq!(
            remove,
            [
                "GHOSTTY_RESOURCES_DIR",
                "ITERM_SESSION_ID",
                "KITTY_WINDOW_ID",
                "TERMINFO",
                "TMUX",
                "TMUX_PANE"
            ]
        );
    }

    #[test]
    fn a_locale_the_app_has_is_kept() {
        let (set, remove) = shell_env(vars(&[("LANG", "de_DE.UTF-8")]));
        assert!(!set.contains_key("LANG"));
        assert_eq!(remove, Vec::<String>::new());
    }

    #[test]
    fn removed_variables_are_unset_before_the_program_starts() {
        let interactive = || vec!["-i".to_string()];
        assert_eq!(
            without(&["TMUX".to_string()], "/bin/sh", interactive()),
            tty::Shell::new(
                "/usr/bin/env".to_string(),
                ["-u", "TMUX", "/bin/sh", "-i"].map(String::from).to_vec()
            )
        );
        assert_eq!(
            without(&[], "/bin/sh", interactive()),
            tty::Shell::new("/bin/sh".to_string(), interactive())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_login_shell_is_the_one_alacritty_starts() {
        let (program, args) = login_command("/opt/homebrew/bin/fish", "ada", false);
        assert_eq!(program, "/usr/bin/login");
        assert_eq!(
            args,
            [
                "-flp",
                "ada",
                "/bin/zsh",
                "-fc",
                "exec -a -fish /opt/homebrew/bin/fish"
            ]
        );
        assert_eq!(login_command("/bin/zsh", "ada", true).1[0], "-qflp");
    }
}
