//! A new execution is not an inherited agent launch or an outer multiplexer pane.
use std::ffi::OsStr;

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

#[cfg(test)]
mod tests {
    use super::*;
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
