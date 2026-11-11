//! A new execution is not an inherited agent launch or an outer multiplexer pane.
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub(crate) fn is_parent_context(key: &OsStr) -> bool {
    let key = key.as_encoded_bytes();
    key.starts_with(b"BKPR_")
        || key.starts_with(b"ZELLIJ_")
        || matches!(
            key,
            b"ZELLIJ"
                | b"TMUX"
                | b"TMUX_PANE"
                | b"STY"
                | b"WINDOW"
                | b"KITTY_WINDOW_ID"
                | b"KITTY_PID"
                | b"WEZTERM_PANE"
                | b"WEZTERM_UNIX_SOCKET"
                | b"ALACRITTY_SOCKET"
                | b"ALACRITTY_WINDOW_ID"
                | b"GNOME_TERMINAL_SCREEN"
                | b"GNOME_TERMINAL_SERVICE"
                | b"ITERM_SESSION_ID"
                | b"TERM_SESSION_ID"
                | b"TERM_PROGRAM"
                | b"TERM_PROGRAM_VERSION"
                | b"LC_TERMINAL"
                | b"LC_TERMINAL_VERSION"
                | b"VTE_VERSION"
        )
}

pub(crate) fn strip_parent_context(command: &mut portable_pty::CommandBuilder) {
    // vars_os preserves non-UTF8 values; a malformed credential is still stripped by key.
    for (key, _) in std::env::vars_os() {
        if is_parent_context(&key) {
            command.env_remove(key);
        }
    }
}

fn installed_terminfo() -> Option<&'static Path> {
    static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            std::env::var_os("BKPR_TERMINFO_DIR")
                .map(PathBuf::from)
                .filter(|path| contains_bkpr_terminfo(path))
                .or_else(|| {
                    let executable = std::env::current_exe().ok()?;
                    let directory = executable.parent()?.parent()?.join("share/terminfo");
                    contains_bkpr_terminfo(&directory).then_some(directory)
                })
        })
        .as_deref()
}

fn contains_bkpr_terminfo(directory: &Path) -> bool {
    directory.is_absolute()
        && ["b/bkpr", "62/bkpr"]
            .iter()
            .any(|entry| directory.join(entry).is_file())
}

fn terminfo_search_path(existing: Option<&OsStr>, directory: &OsStr) -> OsString {
    let existing = existing.unwrap_or(OsStr::new(""));
    if existing
        .as_encoded_bytes()
        .split(|byte| *byte == b':')
        .any(|part| part == directory.as_encoded_bytes())
    {
        return existing.to_os_string();
    }
    let mut result = OsString::with_capacity(existing.len() + 1 + directory.len());
    result.push(existing);
    result.push(":");
    result.push(directory);
    result
}

/// Extend a child's terminal database search without changing its terminal type.
pub(crate) fn configure_terminfo(
    command: &mut portable_pty::CommandBuilder,
    isolated_environment: bool,
) {
    if let Some(directory) = installed_terminfo() {
        let existing = (!isolated_environment)
            .then(|| std::env::var_os("TERMINFO_DIRS"))
            .flatten();
        command.env(
            "TERMINFO_DIRS",
            terminfo_search_path(existing.as_deref(), directory.as_os_str()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_database_search_preserves_precedence_and_default_markers() {
        for (existing, expected) in [
            (None, ":/bundle"),
            (Some(""), ":/bundle"),
            (Some("/custom"), "/custom:/bundle"),
            (Some("/first::/last"), "/first::/last:/bundle"),
            (Some("/first:/bundle:"), "/first:/bundle:"),
        ] {
            assert_eq!(
                terminfo_search_path(existing.map(OsStr::new), OsStr::new("/bundle")),
                OsStr::new(expected),
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn terminal_database_search_preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let existing = OsStr::from_bytes(b"/custom-\xff::/last");
        let directory = OsStr::from_bytes(b"/bundle-\xfe");
        assert_eq!(
            terminfo_search_path(Some(existing), directory),
            OsStr::from_bytes(b"/custom-\xff::/last:/bundle-\xfe"),
        );
        let existing = OsStr::from_bytes(b"/custom-\xff:/bundle-\xfe:");
        assert_eq!(terminfo_search_path(Some(existing), directory), existing);
    }
    #[test]
    fn launch_authority_and_outer_pane_markers_never_cross_the_boundary() {
        for key in [
            "BKPR_RELAY_TOKEN",
            "BKPR_RELAY_AGENT_ID",
            "BKPR_RELAY_SOCKET",
            "BKPR_RELAY_HARNESS",
            "BKPR_PANE_ID",
            "BKPR_WORKSPACE_ID",
            "BKPR_SESSION_ID",
            "BKPR_HARNESS_WRAPPER_DIR",
            "ZELLIJ",
            "ZELLIJ_PANE_ID",
            "TMUX",
            "TMUX_PANE",
            "STY",
            "WINDOW",
            "KITTY_WINDOW_ID",
            "WEZTERM_PANE",
            "TERM_PROGRAM",
            "ITERM_SESSION_ID",
        ] {
            assert!(is_parent_context(OsStr::new(key)), "inherited {key}");
        }
        for key in [
            "PATH",
            "HOME",
            "SHELL",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "LANG",
            "SSH_AUTH_SOCK",
            "EDITOR",
            "GIT_CONFIG_GLOBAL",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(
                !is_parent_context(OsStr::new(key)),
                "removed user environment {key}"
            );
        }
    }
}
