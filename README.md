# shephrd

Workspace-first terminals for Zellij, with a sidebar for projects, coding harnesses and GitHub checks.

Zellij owns the terminals, scrollback and session persistence. Shephrd adds workspace grouping, editable names, a pane finder and repository details. Existing Zellij splits and tabs appear in the sidebar too.

## Install and run

With Nix, from this checkout:

```sh
nix run .
nix run . -- --session project
```

To install the command in your profile:

```sh
nix profile install .
shephrd --session project
```

The package includes the native launcher, WASM plugin, patched Zellij 0.45.1, Git and GitHub CLI. A Nerd Font is recommended for the sidebar icons. `shephrd --help` lists launch options, including the session name and initial sidebar width.

On first launch, click the sidebar and approve its Zellij permission request with `y`. Authorization is cached for the plugin artifact. Shephrd uses a private Zellij configuration; it doesn't replace your personal configuration or start a system service.

Launching the same session attaches to its running terminals and preserves workspace labels. Detaching leaves them running. Closing a terminal terminates it; closing a workspace asks for confirmation before terminating its terminals. Processes need not survive termination of the Zellij server.

### SSH agent forwarding

Panes receive a stable, session-local `SSH_AUTH_SOCK`. Shephrd forwards it to the incoming agent of the most recently attached live Shephrd client, whether launched locally or through SSH. Reattaching updates existing shells; detaching restores the previous client's agent. An unset agent or no attached client never falls back to an unrelated identity.

Attach with `shephrd --session project` to register the current agent. Direct `zellij attach` doesn't update it. Sessions created before this forwarding support need a fresh session so their processes inherit the stable socket.

## Workspaces and navigation

Application input starts in Zellij's **Locked** mode, so terminal shortcuts go to the application. Press **Ctrl+Space** to focus the sidebar, or **Ctrl+g** to unlock stock Zellij controls. The rounded guide follows the current mode and effective keybindings: Locked stays inside the sidebar; other modes can extend into the pane area without taking focus. Keys are right-aligned, descriptions left-aligned, with a centered gap when space permits; long binding groups wrap without dropping keys.

A left click activates a workspace or terminal without moving keyboard focus into the sidebar. Hover highlights rows but doesn't steal focus. Right click opens persistent details; clicking outside dismisses them without consuming the application's click. Details include available activity, CWD, repository, pull request and checks, with clickable links.

Each sidebar-created terminal opens in its own native tab, grouped under its workspace. Stock splits remain available. Drag the separator to resize the sidebar, or use Zellij's resize mode. New sidebar-created tabs inherit the current width, folds and scroll position while their live state arrives.

| Context | Key | Action |
| --- | --- | --- |
| Application | Ctrl+Space | Focus sidebar controls |
| Sidebar controls | Ctrl+Space / Esc | Return to the application in Locked mode |
| Any mode | Ctrl+g | Toggle Locked mode |
| Unlocked | Ctrl+p, then r / d | Split right / below |
| Unlocked | Ctrl+t, then n | Create a native tab |
| Unlocked | Alt+arrows | Move pane focus; left/right cross tabs at the edges |
| Sidebar controls | Tab / Shift+Tab | Cycle pane focus |
| Sidebar controls | Ctrl+d / Ctrl+q | Detach / quit the session |
| Sidebar | arrows / j / k | Select a row |
| Sidebar | Enter | Activate the selected terminal |
| Sidebar | Space | Fold or unfold a workspace |
| Sidebar | n / N | Create a workspace / terminal in the selected workspace |
| Sidebar | r / x | Rename / confirm termination |
| Sidebar | / | Open the pane finder |
| Dialog | Tab / Ctrl+u | Switch fields / clear the current field |
| Dialog | Enter / Esc | Submit / cancel |

Double click a workspace group to fold or unfold it. Stock resize, move, scroll and session modes remain available after unlocking.

### Pane finder

Press **Ctrl+Space**, then **/** to open a centered, whole-window finder. Search pane metadata without switching terminals. The selected match shows the same details as its context popup.

Results underline matching text in blue and keep their project's sidebar color. Metadata-only matches include a short excerpt. Finder key hints appear in the sidebar.

Plain text searches across metadata. Add filters to narrow the results:

```text
project:shephrd branch:main
harness:claude status:working
workspace:"client work" ci:failed
pr:123 ticket:ENG-42
cwd:"/home/me/projects"
```

Supported filters are `pr:`, `branch:`, `project:`, `workspace:`, `harness:`, `status:`, `ci:`, `cwd:` and `ticket:`. Terms combine with AND; quote values that contain spaces. Use arrows or **Ctrl+n / Ctrl+p** to select a match, **Enter** to activate it and **Esc** to cancel. Search uses collected metadata, not terminal screen scraping or a remote request per keystroke.

The preview links the PR number next to the repository path. Verified Linear issue links from PR metadata appear directly below it. Links are clickable; other actions, including CI refresh, remain in the context popup. Shephrd doesn't change Linear or harness profiles to provide these links.

## Repository and CI details

Solid project dots identify repositories; project colors stay stable across branch changes and linked worktrees. The second line follows the foreground program's actual CWD and shows its branch, available whole-branch additions/deletions and an independent CI badge. Outside Git, it shows the program description instead. Collapsed workspaces summarize their harness and CI state.

Branch totals compare committed `HEAD` with its merge-base, not uncommitted changes or the base branch's current tip. They cover the whole repository, including when the foreground CWD is nested. The base comes from local Git configuration and refs; Shephrd doesn't fetch automatically. Missing or unrelated bases are unavailable, not zero. Details show the comparison basis and binary changes separately.

GitHub CI monitoring uses `gh`'s active authentication context. Run `gh auth login` if needed. Shephrd doesn't switch accounts or edit credentials. Monitoring requires an explicit `github.com` HTTPS/SSH remote and a current commit; GitHub Enterprise and SSH aliases aren't supported.

Requests are shared across panes and worktrees, batched, and limited to at most one request per minute per session. Pending checks refresh after a minute; unresolved results refresh less often. Complete terminal results stay cached until the commit/source changes or you click refresh in the details. Use refresh for same-commit reruns, newly added checks or PR-state changes. Refresh still respects request spacing and provider limits.

Checks belong to the exact current commit. An unpushed or inaccessible commit is unknown, never passing. Missing, stale and incomplete data remain distinct from successful checks. Git metadata updates through filesystem events; CI uses the bounded refresh schedule above.

Open PR links follow the branch even while local commits are unpushed. Linear links come from the PR description and its first ten comments; no Linear credentials or configuration are required.

## Coding harnesses

Recognition and semantic status are separate. Shephrd recognizes Claude Code, Codex, OpenCode, omp, Antigravity, Cursor Agent, GitHub Copilot CLI, Aider, Cline CLI and Goose. Recognition supplies identity and available titles, not an invented working or idle state.

Harness status comes from native **OSC 7501** reports. Titles, progress bars, notifications and rendered terminal text aren't substitutes. Non-reporting harnesses remain **unknown**. Ordinary programs have no harness-status badge, even if they emit generic program-status reports; their Git and CI details still work.

The upstream capabilities below were checked on **2026-10-14**:

| Harness | Semantic reporting |
| --- | --- |
| Claude Code | Native OSC 7501 since [2.1.295](https://github.com/anthropics/claude-code/releases/tag/v2.1.295) |
| Codex | Available in [0.163.0-alpha.12](https://github.com/openai/codex/releases/tag/rust-v0.163.0-alpha.12), not the checked stable 0.162.1 |
| omp | Native OSC 7501 since [18.8.7](https://github.com/can1357/oh-my-pi/releases/tag/v18.8.7); `terminal.programStatus` defaults to enabled |
| OpenCode | [OSC 7501 PR](https://github.com/anomalyco/opencode/pull/54560); not released at the checked date |
| Antigravity, Cursor, Copilot, Aider, Cline, Goose | Recognition only; no native OSC 7501 producer established at the checked date |

An alpha release isn't stable availability, and an open PR isn't a release commitment. Upstream Pi isn't omp.

### Claude Code

Run an up-to-date `claude` directly in a Shephrd terminal. Native reports supply lifecycle state, attention and available activity messages.

For working activity text with Claude 2.1.296, opt into its [fullscreen renderer](https://code.claude.com/docs/en/fullscreen) for that launch:

```sh
claude --settings '{"tui":"fullscreen"}'
```

This research-preview renderer handles conversation scrollback and selection inside Claude. Activity describes current work, not necessarily the original task. Successful completion can emit `done` without a summary, so the activity description clears.

Shephrd adds a launch-local title hook for supported interactive `claude` invocations. It supplies a first-prompt fallback label without replacing later native titles or explicit Shephrd renames. Resume/continue, noninteractive invocations and explicit settings files pass through unchanged. Aliases, shell functions and absolute executable paths can bypass it. `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` disables the supplementation too. The hook doesn't change global settings, credentials, permission policy or semantic status.

### Other harnesses

Run `omp` directly with version 18.8.7 or newer and native program status enabled. Shephrd doesn't inject an extension or configure an adapter for older versions.

Antigravity is recognition-only. Shephrd doesn't wrap `agy`/`antigravity`, install callbacks, edit settings or redirect its profile. Other recognition-only harnesses also need native semantic reports before their status can be shown.

## Develop

The Rust workspace contains the native launcher/metadata helper in `crates/shephrd` and the WASI sidebar/plugin in `crates/shephrd-zellij`. Zellij owns application PTYs; a session-wide plugin owns workspace metadata, with one native helper for Git and shared CI requests.

Use the devshell and Cargo for iteration. Prepare the patched local Zellij runtime using the steps in [AGENTS.md](AGENTS.md#patched-zellij-runtime) before launching a development session; stock Zellij doesn't provide the required native events.

```sh
nix develop
cargo build --locked -p shephrd
cargo build --locked -p shephrd-zellij --target wasm32-wasip1
cargo run --locked -p shephrd -- --session dev
```

The devshell selects the debug WASM artifact and an existing `target/zellij-dev/target/debug/zellij`. Use a fresh session for native runtime changes; an existing server keeps its original code and configuration. Retain `target/` for incremental builds.

```sh
# In the devshell:
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked -p shephrd-zellij --target wasm32-wasip1 -- -D warnings

# Packaging validation, not the inner development loop:
nix build .#shephrd
```

`shephrd-native` and `shephrd-zellij` are also exposed as separate Nix package outputs. Development overrides are `SHEPHRD_ZELLIJ` / `--zellij` and `SHEPHRD_PLUGIN` / `--plugin`. Stable maintenance contracts and native patch workflow are in [AGENTS.md](AGENTS.md).

## Limitations

- No built-in remote connection manager or saved-machine profiles. Run Shephrd on a remote host through SSH as usual.
- Live Git watching requires Linux inotify and pidfd support (kernel 5.3+). Nix packages are exposed for Linux and Darwin, but that doesn't imply equivalent runtime coverage.
- Unicode names work through paste. Typed non-Latin dialog input is constrained by the pinned Zellij plugin key decoder.
- Notification passthrough isn't implemented.
- Semantic coverage depends on each harness's actual native producer. No transcript parsing or automatic harness resurrection is provided.
