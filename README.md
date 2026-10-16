# bkpr

Workspace-first terminals for Zellij, with a sidebar for projects, coding harnesses and GitHub checks.

Zellij owns the terminals, scrollback and session persistence. Bkpr adds workspace grouping, editable names, a pane finder and repository details. Existing Zellij splits and tabs appear in the sidebar too.

## Install and run

With Nix, from this checkout:

```sh
nix run .
nix run . -- --session project
```

To install the command in your profile:

```sh
nix profile install .
bkpr --session project
```

The package includes the native launcher, WASM plugin, patched Zellij 0.45.1, Git and GitHub CLI. A Nerd Font is recommended for the sidebar icons. `bkpr --help` lists launch options, including the session name and initial sidebar width.

Tagged revisions are built for x86_64/aarch64 Linux and aarch64 macOS and uploaded to [bkpr.cachix.org](https://bkpr.cachix.org). The pinned Nixpkgs no longer supports x86_64 macOS. After the `unstable` tag's **Cache tagged packages** workflow succeeds, run it without building Bkpr locally:

```sh
nix run github:calops/bkpr/unstable
```

Nix may prompt once to trust this flake's cache settings; accept to use the substituter. Other dependencies can come from their usual public caches. Moving `unstable` to another commit triggers a new cache build when the updated tag is pushed. Maintainers must set the repository Actions secret `CACHIX_AUTH_TOKEN` to a Cachix auth token with write access to the `bkpr` cache; the public key in `flake.nix` is for downloading, not uploading. Untagged commits are not published to this cache.

On first launch, click the sidebar and approve its Zellij permission request with `y`. Authorization is cached for the plugin artifact. Bkpr uses a private Zellij configuration; it doesn't replace your personal configuration or start a system service.

Launching the same session attaches to its running terminals and preserves workspace labels. Detaching leaves them running. Closing a terminal terminates it; closing a workspace asks for confirmation before terminating its terminals. Processes need not survive termination of the Zellij server.

### SSH agent forwarding

Panes receive a stable, session-local `SSH_AUTH_SOCK`. Bkpr forwards it to the incoming agent of the most recently attached live Bkpr client, whether launched locally or through SSH. Reattaching updates existing shells; detaching restores the previous client's agent. An unset agent or no attached client never falls back to an unrelated identity.

Attach with `bkpr --session project` to register the current agent. Direct `zellij attach` doesn't update it. Sessions created before this forwarding support need a fresh session so their processes inherit the stable socket.

## Workspaces and navigation

Application input starts in Zellij's **Locked** mode, so terminal shortcuts go to the application. Press **Ctrl+Space** to focus the sidebar, or **Ctrl+g** to unlock stock Zellij controls. The rounded shortcut guide follows the current mode and effective keybindings: it stays inside the sidebar when all keys and descriptions fit without wrapping, and otherwise expands into the pane area. A wider guide moves to the far right while the sidebar is focused to leave room for item details.

A left click activates a workspace or terminal without moving keyboard focus into the sidebar. Hover highlights a row and opens its details; moving into the popup keeps it open, and leaving both closes it. Sidebar focus opens the selected row's details automatically, and keyboard navigation follows the selection. Right click has no special popup behavior. Details include available activity, CWD, repository, pull request and checks, with clickable links.

The first sidebar line centers the beekeeper icon and project name. Workspaces and terminals use dot markers; recognized harnesses keep their separate identity icons beside their names. Item rows keep a left gutter for the hover outline and use the rightmost sidebar column.

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
project:bkpr branch:main
harness:claude status:working
workspace:"client work" ci:failed
pr:123 ticket:ENG-42
cwd:"/home/me/projects"
```

Supported filters are `pr:`, `branch:`, `project:`, `workspace:`, `harness:`, `status:`, `ci:`, `cwd:` and `ticket:`. Terms combine with AND; quote values that contain spaces. Use arrows or **Ctrl+n / Ctrl+p** to select a match, **Enter** to activate it and **Esc** to cancel. Search uses collected metadata, not terminal screen scraping or a remote request per keystroke.

The preview links the PR number next to the repository path, followed by its title and any verified Linear ticket. Links are clickable; other actions, including CI refresh, remain in the context popup. Bkpr doesn't change Linear or harness profiles to provide these links.

## Repository and CI details

Solid project dots identify repositories; project colors stay stable across branch changes and linked worktrees. The second line follows the foreground program's actual CWD and shows its branch, available whole-branch additions/deletions and an independent CI badge. Outside Git, it shows the program description instead. Collapsed workspaces summarize their harness and CI state.

Branch totals compare committed `HEAD` with its merge-base, not uncommitted changes or the base branch's current tip. They cover the whole repository, including when the foreground CWD is nested. When a PR is associated, its target commit is authoritative, including stacked PRs. Otherwise, local Git configuration and refs choose the base. Bkpr never fetches automatically: a known target missing locally makes totals unavailable rather than falling back to main.

GitHub CI monitoring uses `gh`'s active authentication context. Run `gh auth login` if needed. Bkpr doesn't switch accounts or edit credentials. Monitoring requires an explicit `github.com` HTTPS/SSH remote and a current commit; GitHub Enterprise and SSH aliases aren't supported.

First loads fetch eagerly, with requests shared across panes and worktrees, batched, and spaced at least two seconds apart per session. Published commits with non-green checks refresh every 60 seconds. Green or entirely skipped checks, no checks, and unpublished commits stay idle; Git metadata events detect new commits and pushes. Transport failures use bounded backoff, and provider rate limits remain authoritative. Use refresh for same-commit reruns, newly added checks or PR-state changes.

Checks belong to the exact current commit. An unpushed or inaccessible commit is unknown, never passing. Missing, stale and incomplete data remain distinct from successful checks. A new local commit can retain its branch's last-known open PR metadata, but never the previous commit's check results.

Results persist per canonical CWD under `$XDG_STATE_HOME/bkpr/projects` (default `~/.local/state/bkpr/projects`). Repository, branch, commit and Git refs must still match before restoring them. Unchanged green results need no request after restart; non-green results appear immediately from cache and resume checking.

Open PR links follow the branch even while local commits are unpushed. Linear associations come from canonical issue links in the PR's first ten rendered comments, without workspace-specific mappings, Linear credentials or configuration.

## Coding harnesses

Recognition and semantic status are separate. Bkpr recognizes Claude Code, Codex, OpenCode, omp, Antigravity, Cursor Agent, GitHub Copilot CLI, Aider, Cline CLI and Goose. Recognition supplies identity and available titles, not an invented working or idle state.

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

Run an up-to-date `claude` directly in a Bkpr terminal. Native reports supply lifecycle state, attention and available activity messages.

For working activity text with Claude 2.1.296, opt into its [fullscreen renderer](https://code.claude.com/docs/en/fullscreen) for that launch:

```sh
claude --settings '{"tui":"fullscreen"}'
```

This research-preview renderer handles conversation scrollback and selection inside Claude. Activity describes current work, not necessarily the original task. Successful completion can emit `done` without a summary, so the activity description clears.

Bkpr adds a launch-local title hook for supported interactive `claude` invocations. It supplies a first-prompt fallback label without replacing later native titles or explicit Bkpr renames. Resume/continue and noninteractive invocations do not receive this title hook. Aliases, shell functions and absolute executable paths can bypass the wrapper. `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` disables title supplementation. The hook doesn't change global settings, credentials, permission policy or semantic status.

### Other harnesses

Run `omp` directly with version 18.8.7 or newer and native program status enabled. Semantic status still comes from omp's native reports, not Bkpr's communication adapter.

Antigravity is recognition-only. Bkpr doesn't wrap `agy`/`antigravity`, install callbacks, edit settings or redirect its profile. Other recognition-only harnesses also need native semantic reports before their status can be shown.

### Agent communication (experimental)

Fresh Bkpr sessions supply launch-local wrappers for `claude`, `omp` and `codex`. Each supported interactive launch receives its own authenticated agent ID and a Bkpr MCP server without changing user-level harness settings. Tools: `whoami`, `discover_agents`, `send_message`, `read_inbox`, `ack_message` and `set_title`. Discovery defaults to the canonical Git worktree (or canonical working directory outside Git); `scope: "all"` explicitly opts into cross-workspace discovery or delivery. `BKPR_RELAY_WORKSPACE` can override the grouping for a launch. Peer records include harness, title, Git project and branch, and available cached PR and ticket titles. The broker authenticates senders. `read_inbox` returns up to ten messages with `has_more` and `next_cursor`; acknowledge only after processing. Messages persist until acknowledged, regardless of notification delivery.

Agent IDs are random 128-bit per-launch identifiers, independent of each harness's native session ID; bearer tokens are separate and never shown in discovery. IDs are not reused automatically on resume. Discovery includes registered agents with `active` and `accepting` flags, so exited agents may remain visible while their durable mailboxes exist.

Offline delivery is limited by that per-launch identity: a new Claude or omp launch cannot automatically read an exited launch's inbox. A failed Codex native queue wake-up does not retry automatically; its message stays readable through the original server's MCP connection. Do not use discovery's `active: false` entries as evidence of a currently reachable peer.

Claude uses [experimental MCP Channels](https://code.claude.com/docs/en/channels-reference), subject to interactive consent and organization policy. Its existing title hook captures the native session ID when enabled; explicit settings files retain their title-hook bypass and therefore do not capture that ID. Omp uses a launch-local extension to schedule a follow-up turn from an MCP notification. The extension runs inside omp's own runtime; Bkpr adds no Node dependency. Codex uses its native remote queue after a trusted `SessionStart` hook has associated its session with the Bkpr agent. These mechanisms do not type into a user's input field; Codex's queue nevertheless presents a broker message pointer as internal user-role input. Notifications contain a pointer, not the full peer message: agents read the body from their MCP inbox. A wake-up failure leaves that message pending.

Codex's dedicated native app-server outlives its terminal so its queue remains available. It prints a private manifest location and an exact `bkpr codex-stop --manifest <path>` command. Acknowledge all pending messages before stopping it; Bkpr refuses to stop a server with unread messages. Omp's temporary extension package is removed on normal exit, but forced termination can leave its private package in the managed session runtime.

Wrappers are found through the managed session's `PATH`, not through aliases or absolute executable paths. Already-running processes and sessions created before these wrappers are installed do not gain them retroactively. Interactive consent and trust requirements cannot be bypassed by Bkpr. Codex noninteractive `exec` and `review` do not support this native remote queue and fail clearly instead of silently claiming message delivery.

## Develop

The Rust workspace contains the native launcher/metadata helper in `crates/bkpr` and the WASI sidebar/plugin in `crates/bkpr-zellij`. Zellij owns application PTYs; a session-wide plugin owns workspace metadata, with one native helper for Git and shared CI requests.

Use the devshell and Cargo for iteration. Prepare the patched local Zellij runtime using the steps in [AGENTS.md](AGENTS.md#patched-zellij-runtime) before launching a development session; stock Zellij doesn't provide the required native events.

```sh
nix develop
cargo build --locked -p bkpr
cargo build --locked -p bkpr-zellij --target wasm32-wasip1
cargo run --locked -p bkpr -- --session dev
```

The devshell selects the debug WASM artifact and an existing `target/zellij-dev/target/debug/zellij`. Use a fresh session for native runtime changes; an existing server keeps its original code and configuration. Retain `target/` for incremental builds.

```sh
# In the devshell:
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked -p bkpr-zellij --target wasm32-wasip1 -- -D warnings

# Packaging validation, not the inner development loop:
nix build .#bkpr
```

`bkpr-native` and `bkpr-zellij` are also exposed as separate Nix package outputs. Development overrides are `BKPR_ZELLIJ` / `--zellij` and `BKPR_PLUGIN` / `--plugin`. Stable maintenance contracts and native patch workflow are in [AGENTS.md](AGENTS.md).

## Limitations

- No built-in remote connection manager or saved-machine profiles. Run Bkpr on a remote host through SSH as usual.
- Live Git watching requires Linux inotify and pidfd support (kernel 5.3+). Nix packages are exposed for Linux and Darwin, but that doesn't imply equivalent runtime coverage.
- Unicode names work through paste. Typed non-Latin dialog input is constrained by the pinned Zellij plugin key decoder.
- Notification passthrough isn't implemented.
- Semantic coverage depends on each harness's actual native producer. No transcript parsing or automatic harness resurrection is provided.
