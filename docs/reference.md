# Bkpr reference

Detailed usage, integration contracts and development instructions. Start with
the [README](../README.md) for installation and common workflows.

Bkpr uses a native `bkprd` daemon to own PTYs, terminal state and workspace
layouts independently of its clients. Layouts and verified recovery metadata
survive daemon restarts; processes and terminal contents do not. The engine
adapter uses the pinned `libghostty-rs` SDK over libghostty-vt.

## Contents

- [Build from source](#build) and [harness-testing shell](#harness-testing-shell)
- [Native CLI](#native-cli), [remote sessions](#remote-sessions) and [pane context](#pane-context)
- [SSH agent forwarding](#ssh-agent-forwarding)
- [Harness capture](#harness-capture) and [explicit recovery](#explicit-harness-recovery)
- [Discoverable control](#discoverable-control) and [agent communication](#agent-communication)
- [Local Git](#local-projects-and-git) and [GitHub PRs and CI](#github-pull-requests-and-ci)
- [Sidebar and presentation](#sidebar-popups-and-presentation)
- [Workspace, window and pane controls](#workspace-window-and-pane-controls)
- [Resizing](#resizing-panes), [scrollback](#scrollback-navigation), [copy](#selection-and-copy) and [search](#terminal-search)
- [Ownership](#workspace-and-session-ownership) and [atomic pane placement](#atomic-pane-creation-and-placement)
- [Requests and shutdown](#requests-and-explicit-shutdown)
- [Tests](#tests), [screenshot maintenance](#screenshot-maintenance) and [packaging](#packaging-and-releases)

## Build

For development, use Cargo with the pinned engine preparation helper. The
default Nix shell supplies the toolchain; packaged installation is described
under [Packaging and releases](#packaging-and-releases).

```sh
nix develop
# Use this worktree's own cache, independent of main's build artifacts.
export CARGO_TARGET_DIR="$PWD/target"
just build
"$CARGO_TARGET_DIR/debug/bkpr" --help
```

The default devshell stays lean; the optional harness-testing shell below adds
native coding-agent CLIs. No separate sibling-runtime shell or build is needed.
`just build` validates and reuses the prepared engine, preparing it
only when missing or invalid, then builds both executables with
`cargo build --locked --workspace`. `just run --help` builds and runs `bkpr`;
`just test` uses the same validated engine reuse.
All recipes respect an explicit `CARGO_TARGET_DIR`; use this worktree's local
`target/`, not the main checkout's cache.

Repository Cargo commands default to eight build jobs through `.cargo/config.toml`;
rustc coordinates its parallel code generation with Cargo's jobserver. Native
engine preparation also uses `zig build -j8`. This leaves spare CPU capacity on the
development workstation without changing optimization settings or forcing a
rebuild with new Rust flags. It isn't a hard limit on every auxiliary OS thread.
Keep compilation bounded on a workstation running live sessions.

**Shared-artifact safety:** a normal build writes `debug/bkpr` and `debug/bkprd` in
the selected Cargo target directory. If either pathname is a currently used application, do not
run `just build`, `just run` or the equivalent Cargo build against it until you
have arranged artifact isolation. Do not replace an installed or live program
as part of trying this branch.

Without Nix, provide the pinned Rust toolchain from `rust-toolchain.toml`, Cargo,
a C linker toolchain, Bash, curl, patch, tar, xz and the usual Unix core/find
utilities. `just` is optional when using the direct commands below.
The preparation helper supports x86-64 Linux, ARM64 Linux and ARM64 macOS;
this is a prerequisite-platform list, not a claim of runtime acceptance on every
platform.

```sh
export CARGO_TARGET_DIR="$PWD/target"
mkdir -p "$CARGO_TARGET_DIR"
bash scripts/prepare-ghostty-vt.sh
export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
export GHOSTTY_INSTALL_DIR="$BKPR_GHOSTTY_VT_DIR"
cargo build --locked --workspace
```

The helper downloads hash-checked Ghostty and Zig archives, applies the checked
local clipboard-offer and retained Kitty image source patches and builds the
pinned static engine. Initial
preparation can require network access, including Zig build dependencies. The
Cargo build checks engine provenance and prepared-file hashes; an arbitrary
system Ghostty installation is not a substitute.

### Harness-testing shell

Use the opt-in shell to add **all eleven recognized harness CLIs** to the
default development tools, including OpenCode 2 under its native `opencode`
command:

```sh
nix develop .#harnesses
```

This shell is available on x86-64 Linux, ARM64 Linux and ARM64 macOS, not
Intel macOS or Windows. It provides terminal CLIs, not IDE substitutes.
Entering a shell installs no login credentials, changes no persistent harness
settings and starts no agent or model request. Native authentication and
provider requirements still apply when a CLI is launched. Claude, Antigravity,
Cursor Agent and Copilot are proprietary; their vendor licenses still apply.
The distribution labels those licenses unfree but marks them evaluable without
Nix's `allowUnfree`; entering this shell does not change their license terms.
There is no global `allowUnfree` change to Bkpr's package set.

| Harness | Command | Pinned version |
| --- | --- | --- |
| Claude Code | `claude` | 2.1.322 |
| Codex | `codex` | 0.167.2 |
| Omp | `omp` | 18.19.0 |
| OpenCode | `opencode` | 2.2.7 |
| Pi | `pi` | 1.5.2 |
| Antigravity | `agy` | 1.5.6 |
| Cursor Agent | `cursor-agent` | 2026.11.11-b912975 |
| GitHub Copilot | `copilot` | 1.0.109 |
| Aider | `aider` | 0.86.1 |
| Cline | `cline` | 3.0.74 |
| Goose | `goose` | 1.55.0 |

Aider comes from this flake's pinned nixpkgs. The other CLIs come from
[`numtide/llm-agents.nix` at `90011236663d4116738fc2d63de6ab64523387b6`](https://github.com/numtide/llm-agents.nix/tree/90011236663d4116738fc2d63de6ab64523387b6),
with its own locked dependencies preserved rather than following Bkpr's nixpkgs.
The flake offers that distribution's signed `https://cache.numtide.com` binary
cache alongside Bkpr's cache; Nix may ask whether to trust these flake settings.
This uses maintained CLI packages and their pinned upstream artifacts; it does
not download moving `latest` releases on shell entry. The shell adds only a
canonical-name symlink to that distribution's `opencode2` executable; it does not
include OpenCode 1.

Inside a Bkpr pane, use explicit managed launches such as `bkpr run claude`
or `bkpr run pi` when testing the supported adapters. `nix develop` can place
native executables ahead of Bkpr's private PATH wrappers, so a bare command is
not proof of managed capture. `bkpr run opencode` uses OpenCode 2's private
native standalone service with launch-local Bkpr MCP/capture configuration.
Explicit external-service and utility modes remain native and unmanaged.
See [Harness capture](#harness-capture) for the supported native behavior.

## Native CLI

After building both executables, bare `bkpr` connects to or starts the host/user
daemon, opens the named session `bkpr`, and enters its terminal client. All local
names share that daemon while retaining independent session registries:

```sh
"$CARGO_TARGET_DIR/debug/bkpr"
"$CARGO_TARGET_DIR/debug/bkpr" --session work
"$CARGO_TARGET_DIR/debug/bkpr" -s work -d
"$CARGO_TARGET_DIR/debug/bkpr" list
"$CARGO_TARGET_DIR/debug/bkpr" stop -s work
```

Help is split by task: `bkpr --help` shows the overview and common options,
`bkpr tui --help` shows terminal/connection options, and `bkpr COMMAND --help`
focuses on one command. Use `bkpr keys` for keyboard shortcuts.
`bkpr help COMMAND` works too, including `bkpr help run` and `bkpr help install`.

The Nix package installs Bash, Zsh and Fish completions in their standard
`share/` locations. Enable your shell's normal completion support to load them.
For other installations, `bkpr completions SHELL` writes a script to stdout;
supported shells are Bash, Zsh, Fish, PowerShell and Elvish.
Completions come from the same Clap command definitions as argument parsing,
including subcommands, options and fixed values. They do not query live sessions
or run harnesses. Completion generation does not create user state.
The upstream Fish generator currently omits positional argument values; named
option values still complete.
For Fish value completion at the root, use `--clipboard=...`; the spaced form
also works under `bkpr tui --clipboard ...`.

The Nix package ships a compiled `bkpr` terminfo entry in `share/terminfo`,
generated from the pinned engine's `src/terminfo/ghostty.zig`. It retains Ghostty's
capabilities, including truecolor, styled underlines and synchronized output,
with Bkpr names and corrected DA2/XTVERSION response patterns. The generated
source records the upstream revision and is available at
`share/bkpr/terminfo/bkpr.terminfo`; install it on another host with
`tic -x -o ~/.terminfo bkpr.terminfo`.

The daemon extends `TERMINFO_DIRS` for pane processes, preserving existing search
order and empty components that select the system defaults. An unset variable
retains default lookup. Packages supply the database location through
`BKPR_TERMINFO_DIR`; ordinary installations can use `<prefix>/share/terminfo`
beside `<prefix>/bin/bkprd`. The resource hint itself is not passed to panes.
Pane processes still receive `TERM=xterm-256color`; installing this entry does
not switch terminal identity or enable additional graphics transports.

Normal launches preserve your HOME, shell profiles and environment, using the
invocation directory. Closing the client leaves the daemon and panes running.
`-s` / `--session` selects an exact name; Unicode, spaces and slashes are
supported without treating names as filesystem paths. `-d` / `--detach-others`
revokes the selected session's other client attachments once, after the new
client is initialized. It does not close panes, terminate programs or change
layouts. Later normal attachments are not detached again.

`list` discovers running/stopped stored sessions without creating state or
starting a daemon. `stop` terminates only the selected session and its programs;
without `-s` it targets `bkpr`. Reopening preserves the logical session identity
and recovery intent but gets a new session runtime instance. The host incarnation
changes only when `bkprd` restarts. Closing the final session leaves the host idle.
`bkpr daemon info` inspects the host without starting it; `bkpr daemon stop`
explicitly stops the host and all its sessions.

`bkpr context`, `bkpr request` and `bkpr keyboard` are headless and never start a
daemon. Inside a managed pane they default to its `BKPR_SOCKET`; explicit `-s`
overrides inherited routing. Normal TUI and `stop` commands default to `bkpr`,
not an inherited pane socket. Explicit TUI `--socket` plus `--state-root` selects
a private host namespace and its default session. A headless `--socket` alone
selects a session data endpoint. Host-only raw requests require an explicit host
socket; they never infer one from a pane's `BKPR_SOCKET`.

Opening a stopped local session with `bkpr` supplies the current client's configured
shell, HOME and CWD as immutable session defaults. Cold opening restores saved
layout/recovery intent with current commands, or creates `Main` and a usable shell
when needed. Starting `bkprd` directly without `--session` or argv creates an empty
host; `--session NAME` opens a headless session without executing anything. Argv
after `--` explicitly authorizes initial bootstrap/restoration. Attaching to an
already-open session never creates panes or resets layouts or another client's focus.
The TUI initially prefers a workspace with a running projected pane; historical
workspaces remain explicitly selectable.

For an isolated preview, start the daemon explicitly in one terminal:

```sh
export CARGO_TARGET_DIR="$PWD/target"
umask 077
export BKPR_RUN="$(mktemp -d "${TMPDIR:-/tmp}/bkpr.XXXXXX")"
mkdir "$BKPR_RUN/home" "$BKPR_RUN/state"
printf 'Runtime directory: %s\n' "$BKPR_RUN"
"$CARGO_TARGET_DIR/debug/bkprd" \
  --socket "$BKPR_RUN/control.sock" \
  --state-root "$BKPR_RUN/state" \
  --home "$BKPR_RUN/home" --cwd "$BKPR_RUN/home" \
  -- /bin/sh -i
```

This prints readiness and remains in the foreground; that is the daemon, not a
hung TUI. In another terminal, set `BKPR_RUN` to the printed directory:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" tui \
  --socket "$BKPR_RUN/control.sock" --state-root "$BKPR_RUN/state"
```

Socket/state directories must be private and current-user-owned. The socket is
mode `0600`; a live listener is never replaced, and one daemon exclusively owns
each state root. Keep Unix socket paths short. Explicit `--home` selects a
scrubbed fixture environment, not an OS/filesystem sandbox. `--bkprd PATH`
selects the server executable; `bkprd --bkpr PATH` selects the matching utility
and harness wrappers when testing independently staged binaries.

The host control socket is `$XDG_RUNTIME_DIR/bkpr/d`; named session data sockets
remain `$XDG_RUNTIME_DIR/bkpr/<name-hash>/s`. The host owns
`$XDG_STATE_HOME/bkpr/`, with saved session records under `sessions/<name-hash>/`.
Without these variables, runtime uses a private `bkpr-<uid>/bkpr/` beneath the
resolved system `/tmp`, and state uses `$HOME/.local/state/bkpr/`. Resolving the
system temporary-directory alias does not permit symlinks in Bkpr-owned descendants.
Launch serialization, private filesystem checks and exact stored/live identities
prevent accidental attachment to another session. A live protocol or identity
failure never causes an automatic restart.

The host keeps its startup environment. Pane launches use their session's configured
defaults and do not import an attaching client's arbitrary environment; this includes
`PATH`, `TMPDIR` and account-wrapper choices. Restart the host explicitly to change
its inherited environment. Reopening a stopped session supplies current shell/CWD/HOME
defaults; it does not rewrite active sessions. Fixture isolation remains separate
from normal execution.

A cold `bkpr --session work` restart restores saved owned terminals in their
original workspaces/windows and split positions, including hidden terminals.
Ordinary terminals start the **current shell command** in their last verified
working directory; old arbitrary commands are not replayed. A verified harness
conversation still active at shutdown resumes through its fixed native adapter
with current configuration and fresh launch/inbox authority.

Missing directories or native refusal retain the stopped layout and recovery
intent, report the failure and allow independent terminals to restore. There is
no silent directory fallback or fresh-conversation substitution. Returning from
a harness to its shell before shutdown restores a shell, not that historical
conversation; its last verified descriptor remains available for explicit
`bkpr resume`.

Attaching to a live daemon never repeats restoration. Direct `bkprd` startup
without shell argv remains headless and executes nothing. The existing 200-ms
foreground observer checkpoints changed verified directory/role observations
off input/render paths; abrupt daemon loss can miss an unobserved or uncommitted
change. Graceful shutdown refreshes those observations before teardown.

Client registration keeps a dedicated lifetime connection. Takeover, disconnect
or owner-process exit revokes managed views and pending authority without
requiring another client to acknowledge. Credentials are ephemeral, process-bound
and never written to pane environments or session records. Ordinary pane execs
receive immutable canonical session/pane/workspace snapshots described below.
On Linux, managed clients require kernel `SO_PEERPIDFD` support and fail before
takeover when that prerequisite is unavailable.

There is no fixed pane, workspace, attached-client or view count ceiling.
Creation, restoration, selection and search preserve their ownership checks.
Resource limits apply to bytes and work in flight: native messages remain bounded
to 8 MiB/request and 96 MiB/response, and the durable registry to 4 MiB.
Local transport reserves small-frame capacity separately from large frames and
limits decode/encode work without retaining worker slots across pane callbacks,
long polls or socket writes. Threads, descriptors, PTYs and terminal state still
consume real OS resources; this is not an unlimited-memory guarantee.

### Remote sessions

Open the normal TUI on one or more local and **already-running** remote sessions:

```sh
bkpr -r devbox
bkpr --remote devbox --session work
bkpr -s bkpr -s devbox:work -s otherbox:review
bkpr -r devbox -s work -s otherbox:review -s local:bkpr
bkpr -s local:foo:bar

# Take over once, only after terminal setup and each owner's defaults are ready.
bkpr -s bkpr -s devbox:work -d
```

Repeat `-s/--session` for multiple TUI targets. `host:name` selects an SSH host;
`local:name` forces a local target and escapes literal colons in its name.
`-r/--remote HOST` supplies the host for every unqualified target. Without `-s`,
the name defaults to `bkpr`. Utilities accept one literal `-s` name, without
host-prefix interpretation or broadcast; use `bkpr remote -r HOST` for remote
utilities. Remote executable and SSH settings apply to remote targets only.

With multiple sessions, the sidebar is one foldable tree: session, workspace,
window, then pane, with a blank line between sessions. Session rows use the Nerd
Font beekeeper icon. Remote hosts sit right-aligned and muted; connecting hosts
use a colored Nerd Font spinner, and failures use a colored network-off icon.
Healthy sessions have no status badge. Windows use the two-cell `` honeycomb
slot and neutral labels; project colors belong to pane bees. Bright tree guides
trace only the selected item's path to its ancestors. Fancy chrome rounds each
turn and leaves off-path arms of junctions muted. Plain chrome keeps Unicode
guides at the same two-cell indentation. The active pane's title and metadata
share one full-width highlight.
In workspaces with several windows, a window with one sidebar pane is shown
as that pane directly under the workspace.
A single online session keeps the compact workspace tree. Hovering or browsing
another owner's metadata does not activate its panes. Click or Enter activates
the selected owner. In the sidebar, `S` jumps to the current session's tree
context; `R` on a session row forces a fresh connection.

Each remote attachment is direct: local TUI → one no-PTY OpenSSH/Yamux connection
→ remote `bkpr` helper → remote daemon. Remote-only invocations need no local
daemon, session state, HOME or working directory. Mixed invocations prepare local
sessions independently, never as gateways for their remote siblings. Each daemon
owns its processes, layouts, workspaces, CWD, filesystem validation, Git metadata
and native harness callbacks. Theme, graphics preference, terminal capabilities
and transient toasts belong to the physical client instance. An initially empty
existing session stays empty until an explicit creation action.

Both hosts need compatible native wire family 11 and remote control version 2 builds.
Remote TUI attachment also requires authoritative session spawn defaults.
Unavailable defaults or an incompatible/missing session fail before takeover,
without local-path fallbacks, installation, bootstrap or automatic restart.
Local launch overrides (`--cwd`, `--home`, `--bkprd` and shell arguments) apply only
to local targets and are rejected for remote-only invocation. Explicit socket and
state-root paths cannot be combined with named `-s` targets.

System OpenSSH supplies host aliases, identities and ProxyJump. Authentication
is noninteractive: establish host-key trust and credentials beforehand.
`--ssh-config PATH` selects a local per-invocation config;
`--remote-bkpr-bin PATH` selects the remote executable (default: remote PATH
`bkpr`). The remote login shell must accept POSIX-style executable quoting.
Session names travel in the binary handshake, not in the shell command.
The former `--ssh` and `--helper` flags are removed, without aliases.

Headless workflows remain available:

```sh
# Identity discovery without native client registration or activation.
bkpr remote -r devbox -s work inspect

# One persistent attachment; Operation JSON objects in, JSON-line results out.
printf '%s\n' '{"op":"inspect"}' | bkpr remote -r devbox -s work connect

# One operation through a short-lived managed attachment.
bkpr remote -r devbox -s work request '{"op":"inspect"}'
```

Remote TUI and persistent `connect` accept `-d` / `--detach-others`. Takeover
retires other attachments only after successful initialization, without closing
their programs. Identity-only `inspect` does not select an agent source or
disturb clients.

Agent forwarding is off even if SSH config enables it. Explicit
`-A` / `--forward-agent` on the TUI, `connect` or `request` uses OpenSSH forwarding and
the remote forwarded socket, never a local pathname. Grant it only to trusted
hosts. Launch credentials and pane-context variables are removed from the SSH
child's environment.

`bkpr_client::run_sessions` accepts independently owned local and remote
`SessionTarget`s through the same application, framework and native `Endpoint`.
Local preparation runs once after terminal initialization, off the UI thread.
Registration and defaults discovery precede one-shot activation/takeover.
The eager `RemoteSession::connect` remains available to headless callers.
Scene, image, input and clipboard frames stay binary through bounded connections;
there is no administrative fallback or merged authority.

Connection errors show a short explanation in a centered popup within the session area.
Use `Ctrl-b !` for the displayed session's diagnostic and `R` on its Sessions
sidebar row to retry. A previously active attachment reconnects automatically
after unintentional transport loss, with bounded backoff and fresh authority/snapshots.
The logical session remains pinned; a replacement identity is rejected and aliases
of the same live session are deduplicated. Reconnect never repeats local bootstrap,
takeover, input or ambiguous mutations. A lost accepted reply remains an
**unknown outcome**.

Intentional detach/takeover and authoritative session closure remove that session
and its subtree from this instance. Remaining sessions stay usable; ending the
last session exits after terminal restoration. Bare transport loss is not session
closure. Detaching the client leaves backend programs running. Headless `connect`
reports explicit detachment reasons, treats unexpected loss as failure and does
not automatically reconnect. Clipboard reads retain the host's permission policy.

### Daemon spawn defaults

`SessionInfo` includes immutable `spawn_defaults`: command argv, working
directory, home and environment-isolation policy. These are daemon startup
configuration, not an environment export or a saved pane recipe.

The command supplied to `bkprd` after `--` is its current default command, as
used for cold restoration. A headless daemon instead captures its own nonempty
`SHELL`. Explicit daemon `--home` and `--cwd` override daemon HOME and invocation
cwd; `--home` also selects isolated child environments. Missing or invalid
headless configuration leaves defaults unavailable, without inventing a shell
or path. Headless startup and metadata queries still execute nothing.

Authenticated registered clients can read identity/defaults before activation,
without takeover, agent-source selection or view admission. Other inactive
operations remain rejected. Remote initialization must obtain valid daemon
defaults rather than local or SSH-helper environment values; an older daemon
without this metadata fails before takeover and is never automatically restarted.

Defaults are not persisted or inferred from existing executions. A new daemon
captures its current configuration; creating or replacing panes does not change
it. Explicit callers retain arbitrary commands through the existing spawn API.

### Pane context

Every native pane exec receives `BKPR_SESSION_ID`, `BKPR_SESSION_NAME` and
`BKPR_PANE_ID`. Panes with execution-workspace membership also receive
`BKPR_WORKSPACE_ID` and `BKPR_WORKSPACE_NAME`; standalone panes leave that pair
unset. These are immutable process snapshots, not credentials. Names are
resolved before exec, including concurrent automatically named workspaces.
Parent Bkpr/multiplexer context is removed before the new binding is injected.

Read the current labels and execution membership instead of relying on old
environment values after a rename or layout change:

```sh
# Inside the actual native pane:
bkpr context --json

# Outside a pane, select an exact live pane in the chosen session:
bkpr --session work context --pane PANE_ID --json
```

The default output is indented JSON; `--json` emits compact JSON. The response
contains logical session/daemon identities, pane ID/title, current workspace and
window IDs/names, and registry revision. It does not include native agent
credentials, terminal contents or client focus.

Automatic selection verifies the caller's process birth, native ancestry and
controlling PTY. Same-PTY background jobs work; stale environment IDs, an outer
pane inherited by a nested PTY, and outside callers do not select a pane.
Hidden or borrowed projections do not change canonical execution membership.
Stopped/closing/unknown executions are unavailable, not guessed from history.
Use the existing versioned `request`/`inspect` API for the whole registry.

Context queries remain headless and never start a daemon. Existing processes
keep their old environment; existing daemons keep their old code. Use a fresh
session name or explicitly restart the daemon to exercise the new exec/context
behavior. Linux runtime evidence and platform limits are recorded in the
[engine report](pane-first-mux/engine-proof.md#native-pane-context).

### SSH agent forwarding

Normal managed panes receive a daemon-owned `SSH_AUTH_SOCK`. Its pathname stays
the same when clients attach or detach, including when no client remains, so
already-running shells need no environment update.

The latest live **activated** client supplies the upstream agent. Registration
alone changes nothing. An unset or empty source explicitly disables forwarding,
even if an older client has an agent; disconnecting the selected client falls
back to the latest remaining active attachment. Read-only input, pane sizing
and clipboard permissions do not select the agent.

A changed attachment cancels old connections, including pending approvals;
partially sent signing requests are never replayed against another agent.
Detaching an unrelated client leaves current connections intact. The transport
preserves opaque agent messages and connection-scoped extensions, with bounded
buffering and admission. The five-second admission deadline is not a signing
or approval timeout.

Explicit isolated fixture panes receive no agent socket. Bkpr does not change
SSH/Git configuration, profiles or credentials. Keep the control pathname short
enough for its `.agent` companion; both must fit the portable Unix socket limit.


### Harness capture

Managed shells receive private PATH wrappers, without edits to shell profiles or
the harnesses' persistent configuration. Use ordinary commands, or launch explicitly:

```sh
claude
codex
omp
bkpr run codex -- resume
bkpr run omp -- --help
```

`bkpr run HARNESS [--] ARGS...` preserves native arguments and exit behavior.
Claude, Codex, Omp, OpenCode 2, Pi and Antigravity have managed conversation adapters:

| Harness | Native binding mechanism |
| --- | --- |
| Claude | Launch-local hooks capture the native session ID. |
| Codex | A private app-server transport binds the response to the TUI's own start, resume, fork or picker selection, before the first prompt. |
| Omp | A launch-local extension captures the native session manager ID and session transitions. |
| Antigravity | Its MCP request metadata supplies the root conversation ID. Registration alone is not binding or readiness. |
| Pi | A launch-local extension awaits the native session-manager ID before input and follows native session replacement. |
| OpenCode 2 | Native server/TUI plugins bind the selected root before prompt admission and fence MCP calls to that conversation generation. |

#### Durable native integrations

`bkpr install HARNESS` supports `claude`, `codex`, `omp`, `pi`, `opencode`
(OpenCode 2) and `agy`. Run it on the host/account where that harness runs:

```sh
bkpr install claude --dry-run
bkpr install claude
```

| Option | Behavior |
| --- | --- |
| `--inspect` | Report configured ownership and known native restrictions; loaded state stays unknown. |
| `--dry-run` | Validate and show installation changes without writing or starting a harness. |
| `--update` | Update owned code/settings, preserving native preferences and unrelated configuration. |
| `--uninstall` | Remove this profile's owned integration; unrelated servers, hooks and settings remain. |
| `--json` | Return a structured report instead of human-readable output. |

Claude uses native user MCP settings and command hooks; Codex uses its native
TOML MCP entry. Omp and Pi use native extension discovery; Omp also configures its
native MCP entry, while Pi registers MCP through its extension API. OpenCode 2
uses the same native plugin package in its server and CLI configuration plus
its native MCP entry. Antigravity uses its native MCP configuration and
root-conversation metadata. Supported native profile/directory environment
overrides are honored; for example, `OMP_PROFILE=work bkpr install omp`.

Installation stores code references and ownership records, never launch IDs,
tokens, sockets, or live bindings. Managed launches supply private
credentials separately. Outside a valid managed launch, installed extensions
and hooks stay inactive and the installed MCP exposes no tools. Durable
installation does not adopt an unwrapped process: ordinary Bkpr wrappers or
`bkpr run HARNESS` still establish launch authority.

Normal launches do not install or update profiles. Missing installations retain
the private launch-local path where supported; stale, edited or conflicting
installations require explicit repair rather than duplicate injection.
Restart/reload the native harness after changes. Native trust, authentication,
disabled settings and tool approvals remain authoritative; Antigravity receiver
bootstrap still requires its native command approval.

Only named Bkpr collaboration tools get default approvals: channel discovery,
history, members, reads, posts and wake lookup, plus agent discovery and direct
inbox communication. Control, terminal input and attachment transfers aren't
included. Claude uses exact `permissions.allow` names in managed launches and
explicit installs. Native deny and ask rules take precedence; classifier
decisions in Claude auto mode can still block a call.

Codex applies defaults only to fresh threads in a managed launch, whether the
MCP registration is temporary or installed. Explicit native restrictions and
tool filters still apply. Codex install persists the MCP registration, not tool
approvals. Resuming or forking uses native approval policy without Bkpr adding
defaults.

JSONC/TOML edits preserve unrelated settings and formatting. New directories are
0700 and files 0600; existing configuration modes are preserved. Documents are
bounded to 16 MiB. Symlinks, hardlinks, unsafe ownership and conflicting entries
are refused rather than overwritten.

Each changed file is atomically replaced and synced, under a Bkpr installation
lock. This is not a globally atomic transaction with native applications:
uncooperative writers can still race it. Detected changes abort; rollback only
restores bytes still matching Bkpr's write. Errors never claim a durable commit.
An interrupted operation retains a credential-free ownership journal for
`--update` or `--uninstall`; uncertain restoration is reported explicitly.

Cursor Agent, Copilot, Aider, Cline and Goose are recognition-only.
`bkpr run cursor` selects `cursor-agent`, never the Cursor GUI. Native help,
version and noninteractive modes run without capture credentials. Modes that
cannot safely use an adapter retain native behavior rather than silently changing
their configuration: examples include Codex custom remote/profile/local-only
startup options and Claude file-based settings. Such launches can remain
recognition-only.

OpenCode's managed integration targets 2.2.7 and uses native `--standalone`.
Its private service and MCP children belong to the captured TUI; native rendering,
OSC 7501 status, titles and composer remain unchanged. Explicit `--server` and
utility/headless modes keep native behavior without capture or injected MCP.
Native configuration, instructions and plugins are preserved. Inbox wakes use
the native future-turn queue, not simulated typing or active-turn interruption.
`bkpr run opencode2` and the in-pane `opencode2` wrapper select that executable,
not a different `opencode` found on `PATH`.

Backend recovery opens the exact saved, existing native conversation through
OpenCode's public routing API, with no fresh-session fallback or old inbox
adoption. Ordinary user-supplied `--session` keeps its native behavior.
OpenCode 1 is retired. Shared-service ownership and multi-client authority remain
[separate research](pane-first-mux/implementation-plan.md#opencode-2-shared-service-integration-assessment);
standalone support does not establish those contracts.

Pi uses its native extension API and built-in MCP with default codemode exposure
(integration targets Pi 1.5.2). Native configuration and same-name MCP server
precedence remain unchanged. Its own terminal API supplies OSC 7501 status and
titles; `PI_PROGRAM_STATUS` retains native behavior. Headless/protocol, utility,
ephemeral-session and disabled-MCP/extension modes remain unmanaged. Inbox wakes
use native custom-message steering, without replacing the user's unsent draft.

The sidebar distinguishes recognized, managed-unbound, bound and capture-error
states without replacing user pane names. Semantic status is a separate OSC 7501
or authenticated native fact: `.` idle, `~` working, `+` done, `?` blocked, `!` error.
Ordinary programs never acquire harness badges merely by setting a title. Backgrounding a job hides
its foreground identity; resuming it can
restore the same live binding. Returning to the shell or replacing/exiting a
launch clears stale facts.

Capture grants are private and launch-scoped; recovery never stores them. Only
authenticated native conversation IDs and their working directories become
recovery metadata. Pi's temporary extension/MCP package has an owner-only root
(0700) and files (0600); capture bearer credentials are not placed in the native
process's environment.

Process recognition and foreground directories are sampled off the input/render
path; semantic summaries are pushed even for panes with no active view. Linux
managed capture requires kernel peer-PIDFD support and fails closed when
unavailable. macOS capture is not runtime-verified.

Wrappers depend on their PATH entry surviving shell/direnv changes; they do not
rewrite those environments to force interception. Explicit `bkpr run` remains
available through the matching executable. Native first-run trust, account and
terms prompts remain the user's responsibility; Bkpr does not approve them.

### Explicit harness recovery

In the TUI, `Ctrl-b r` opens a searchable recovery overlay for the selected
sidebar session, including SSH attachments; otherwise it uses the current
session. Search by title, harness, status or native conversation ID. Rows only
select: Enter or the Resume button explicitly starts an eligible candidate.
Live and unavailable candidates retain their reason, and read-only attachments
can inspect but never resume.

Recovery, pane Finder and theme selection share floating search/results cards,
with a separate details card where needed and a tinted query area. Plain
rendering keeps the same layout with rounded Unicode borders. Shortcuts stay in
the bottom-left key hint popup, above the active overlay, and follow its current
focus and available actions rather than repeating inside the dialog.

Recovery keeps the list full-width and places its details below it. Rows show
the agent title alongside the colored harness icon and admission status, with
directory and conversation ID columns when space permits. Titles follow the
sidebar's display rules; they are not native conversation IDs.
An explicit pane label takes precedence. Otherwise, the last retained title
from the bound foreground harness survives normal exit and restart. For an
automatic pane label with no retained title, the picker shows the harness name
rather than the shell name. Older titles that were never retained can't be
reconstructed.

`Ctrl-r` refreshes while keeping the query and exact surviving selection.
Tab switches between the list and scrollable details; arrows, PageUp/PageDown
and Home/End navigate the focused area. Esc closes the picker. Closing before
submission cancels the attempt, but hiding an already submitted start doesn't
undo it. Unknown outcomes aren't replayed. A late receipt can't steal focus from
a newer overlay. `Ctrl-b R` remains the separate explicit shell-replacement action.

Start or attach to the named session normally, then inspect its saved native
conversations without starting a TUI or executing anything:

```sh
bkpr --session work resume
bkpr --session work resume --pane PANE_ID
```

Replace `PANE_ID` with an exact ID from the list. Headless `resume` never starts
a daemon; if it is stopped, open `bkpr --session work` first. A candidate is
`live`, `recoverable` or `unavailable`, with a reason where applicable.
`recoverable` means an explicit attempt is eligible, not that Bkpr has verified
the native conversation store, account, provider or first-run permissions.

Resume uses the saved native ID and raw working directory with **current native
configuration**. It does not save or replay old commands, model/provider/profile
flags, approvals or sandbox settings. The harness may restore its own session
state; incompatible or missing native state remains a native refusal. Bkpr
never switches provider, forks, retries or starts a fresh conversation as a
fallback. Claude/Omp use native `--resume`, Codex uses `resume`, Pi uses
`--session`, OpenCode uses its public conversation-routing API, and Antigravity
uses `--conversation`; normal native approvals apply.

The result reports `pending_native_binding`, not proof that the conversation has
loaded. A successful authenticated native bind transfers the saved descriptor
to the new pane with fresh launch/inbox authority. Startup failure leaves the
original descriptor available. Stale selections and concurrent duplicate
resumes are rejected, including two pane records for the same conversation.
A stopped projected pane is replaced in its window; a detached historical
record creates a workspace. Running shells are never replaced implicitly.

Private registry version 4 adds verified terminal directories and separates active
automatic-resume intent from historical native descriptors. Terminal contents,
old argv, live grants and native settings are not recovery records. Explicit
pane/workspace destruction removes intent. Older records retain explicit native
descriptors, but unknown historical activity is not guessed: their cold restart
uses shells. Native recovery isn't process resurrection.
See [native runtime evidence and remaining harness gates](pane-first-mux/engine-proof.md#explicit-native-recovery).

### Discoverable control

The CLI and shared MCP use one typed operation catalog. Common signatures are
advertised; consult help/schema when an operation's contract is unfamiliar:

```sh
bkpr api help
bkpr api schema pane.create
bkpr -s work api call session.inspect --arguments '{}'
```

Help/schema are local and do not start a daemon or terminal client. Calls target
an existing endpoint. A managed agent uses its authenticated launch and defaults
to its live execution workspace; widening to `session` or `all` is explicit and
stays within that daemon. Repository-scoped control and cross-daemon control are
not advertised. Unmanaged CLI callers select their endpoint through the normal
session/socket options.

MCP help/schema are available on demand, not mandatory preludes to every call:
`control({"operation":"api.schema","arguments":{"operation":"pane.create"}})`.
Native operations include inspection, workspace/window/pane lifecycle, atomic
pane batches with final placement, layout updates, CI refresh and native recovery.
Use returned opaque IDs and the expected revision for the owning document.
Workspace creation requires explicit session/all scope for managed agents.
Programs use argv arrays, not shell-interpolated strings.

Mutations return compact receipts with affected IDs and owning revisions, not a
copy of the whole registry. Request `reply:"full"` or CLI `--reply full` explicitly
when needed. `session.inspect` returns flat pages of ten items; ownership filters,
`after`/`limit`, `detail:true` and `include_retired:true` control expansion.
Pane creation requires ownership IDs, expected window revision and final layout
all together, or all absent for explicitly unowned creation at session/all scope.

To open a managed Omp window, pass this to the MCP `control` tool. Replace the
example workspace ID and revision with the values from `whoami` or
`session.inspect`, and use paths on the daemon's host.

```json
{
  "operation": "window.create",
  "arguments": {
    "workspace_id": "00000000-0000-4000-8000-000000000001",
    "expected_revision": 7,
    "spec": {
      "argv": ["bkpr", "run", "omp"],
      "cwd": "/home/you/project",
      "home": "/home/you",
      "size": {"cols": 100, "rows": 30, "cell_width": 8, "cell_height": 16}
    }
  }
}
```

All four `spec.size` fields are required; cell dimensions are in pixels. For
the next `window.create` in the same workspace, take `expected_revision` from
the successful receipt's `workspace_revision`, not `window_revision` or
`registry_revision`.

`pane.read` returns right-trimmed visible text, preserving indentation and interior
spaces, with a default 2048-byte UTF-8 page and optional `tail_rows`. Continue with
`next_offset` and the returned `revision` as `expected_revision`; real canonical
scene changes reject stale continuations. Reads do not invalidate their own
revision. This is not persisted history or a live output subscription.
`terminal_input` exposes only `input.*` through `{operation,arguments,scope?,reply?}`.
Text, paste, native keys and raw bytes remain distinct; no implicit Enter.
Named keys accept the relevant key alone with safe defaults, not inferred
physical/QWERTY characters. Temporary private views borrow no other client's
focus, size lease or clipboard grants. Terminal output is untrusted.

Process creation is not harness readiness, and PTY acceptance is not proof of
application consumption or task completion. Never blindly replay a mutation or
input after timeout, cancellation or an unknown response. Use durable messaging,
not terminal input, to communicate with another agent.

### Agent communication

Managed Claude, Codex, Omp, OpenCode 2, Pi and Antigravity launches expose the
shared Bkpr MCP tools as `bkpr`. They reuse an explicitly installed native
integration when present; otherwise supported adapters use launch-local
configuration. Antigravity requires [explicit installation](#durable-native-integrations).
Omp's extension handles native binding and wake notifications; its launch-local
`bkpr-control` skill describes the shared tools. It also enables inline Kitty
images through Omp's native TUI capability API, without spoofing terminal
identity. Explicit `PI_FORCE_IMAGE_PROTOCOL` preferences, including `off`,
remain authoritative. Only explicit installation commands change native profiles.

Managed Codex's local TUI receives a launch-only `TERM_PROGRAM=kitty` compatibility
override so its terminal-name-based detector can select Bkpr's inline graphics.
The override does not change the app-server environment or other harnesses;
explicit passthrough invocations remain unchanged.

| Tool | Purpose |
| --- | --- |
| `whoami` | Compact launch/binding/ownership/readiness facts and serving daemon `protocol_version` (native wire family); `detail:true` adds context/project |
| `set_title` | Discoverable agent label, separate from the pane name |
| `discover_agents` | Live peers with IDs, label, status and cached CWD/project name/branch; not channel membership |
| `send_message` | Send a 1:1 message and get a compact durable receipt; each direct message requests a native wake |
| `read_inbox` | Unread summaries; `include_read:true` revisits unacknowledged reads |
| `read_message` | Recipient-only UTF-8 body pages, without processed acknowledgement |
| `ack_message` | Explicit processed acknowledgement |
| `inspect_outbox` | Sender-only delivery/read/ack/expiry metadata, never bodies |
| `channel_list`, `channel_members` | Accessible workspace channels and policy; exact channel roster and member IDs |
| `channel_create`, `channel_post` | Create a named group channel or post to `#team`, `#desk` or an exact channel ID |
| `channel_history`, `channel_read` | Up to ten body-free summaries, or an exact message in UTF-8 byte pages |
| `channel_upload`, `channel_fetch` | Upload a local PNG to a channel or save an exact readable channel attachment to a new local file; both return metadata only |
| `channel_wake` | Look up a native wake batch for this launch, without reading or acknowledging its posts |
| `control` | Discover help/schema; inspect and manage native workspaces, windows, panes, layouts and recovery |
| `terminal_input` | Explicit literal text, bracketed paste, key events or raw bytes against an exact pane |
| `read_result` | Temporary, authenticated pages for an explicitly oversized result |

The user chooses the lead for the current authenticated request only, not for
unrelated requests. The lead posts one unmentioned kickoff in `#team` with a
proposal and a `source: {channel_id, message_id}` linking the original user post,
then waits for input. Peers contribute one substantive finding, objection or
improvement instead of echoing agreement. Only the lead sends one synthesis to
the exact original `#desk` `reply_target` unless the user explicitly requests
individual reports. Wait for native wakes instead of polling. A wake that needs
no action needs no terminal chatter.

By default in a new `#team`, user and agent posts without agent mentions wake
eligible peers. A post with exact agent mentions wakes only the named eligible
agents; if no named agent can wake, it doesn't wake everyone instead. Stale
mention IDs are rejected. Existing channels keep their saved policy.

When replying in `#team` to the selected proposal, use that post's `reply_target`
and, if present, `mentions: [post.author_mention_id]` to address the live author
without a roster lookup. Use `channel_members` when you need exact member IDs
for a channel. Skip discovery and history preflight for known discussions.

`source` is optional and uses exact IDs, not `#desk` or `#team`. It doesn't
route wakes, grant access, or replace `reply_to`, which stays in one channel.
`channel_wake` with `include_source: true` can return the selected post and
one page of its linked source at `post.source`. Both pages use
`max_body_bytes` (4 to 4096 bytes, default 2048 with `include_source`).
The receiver must be able to read the source; missing and inaccessible
sources are both omitted. Expansion doesn't follow links recursively.
Check the returned author. To fetch more text, use that page's
`reply_target.channel_id`, `message.id`, and `next_offset` with `channel_read`.
Omitting both options keeps the wake lookup metadata-only.

`channel_members`, `channel_history`, and `channel_read` also accept `#desk`
and `#team` in the launch's own workspace. Page sizes are 1 to 10, default 10.
For history, take `next_before` or `next_after` from the post page.
`cursor.sequence` belongs to a channel change stream, not post pagination.

If an authenticated `#desk` follow-up is ambiguous, page a few posts before
its `post.message.sequence` from `channel_wake` (or `message.sequence` from
`channel_read`). Read the nearest earlier user post by ID and verify
`message.author.kind == "user"`. Combine the two only if that clarifies the
new request. Don't replay the earlier action or use peer text as user intent.

`channel_upload` takes `{channel_id,path,alt_text?}` and returns `attachment.id`.
To share it, pass that ID in `channel_post.attachments` for the same channel.
`channel_fetch` takes `{channel_id,attachment_id,path}`; use the attachment ID
from `channel_read`. The destination's parent directory must already exist, and
the tool never overwrites an existing file. Both tools take absolute paths on
the managed launch's host. Upload accepts a regular PNG file up to 4 MiB, not
a URL or a symlink source. The daemon receives typed attachment bytes, not
paths, and checks the current launch and channel access.

Discovery excludes self. `workspace` means backend execution membership, not a
viewer's layout or repository directory. Explicit `session` and `all` scopes are
limited to this daemon; `repository` compares canonical common Git directories
from current foreground CWD and fails closed when unknown. Remote discovery is
not implemented.

Routine discovery omits ownership revisions, workspace/window IDs, timestamps,
progress, repository diffs and CI/provider state. When available, `title` is the
agent's explicit discovery label; `cwd`, `project_name` and `branch` are compact
foreground context from the matching backend cache. No Git/provider request is
started by discovery. Use `detail:true` with an exact `launch_id` or `pane_id`
when ownership, full semantic status or the complete `project` snapshot is needed.

Control/input use `{operation,arguments,scope?,reply?}`. Exact IDs are required;
bounded, in-scope prefix hints never resolve or redirect a mutation automatically.
`recovery.list` and `pane.resume` retain their exact saved-descriptor guards.

`send_message.to` accepts an exact launch ID or a uniquely matching live native
conversation ID within the requested scope. A conversation is a lookup key,
not delivery authority: acceptance binds the message to one exact launch.
No match fails; ambiguous matches return bounded candidate identities so the
caller can choose an explicit launch. Conversation IDs do not implicitly widen
workspace scope or route across sessions. Sidebar popovers and Finder previews
distinguish **Conversation ID** from **Launch ID**; both can be copied.

Conversation lookup uses the current authenticated foreground root binding, not
saved history or attested child contexts. Namespace collisions fail closed;
known retired launch IDs never redirect. Ambiguity returns at most ten
`launch_id`/`pane_id` pairs with `has_more` and `truncated`.

This adds no wire fields: native family 11 and remote control 2 are unchanged.
Conversation lookup requires a supporting daemon; older daemons still accept
exact launch IDs. Updating the CLI does not replace a running daemon.

Sends require a sender-scoped `idempotency_key`. Only the identical original
recipient selector, text and scope may reuse it; conflicting reuse fails.
A retry returns its original receipt even if the recipient exits, its
conversation changes or a replacement appears. It never resolves the selector
again or delivers to a replacement launch. Every accepted direct message requests
a native wake through the current authenticated receiver. The `delivery` argument
and other sender wake switches are not supported; obsolete arguments are rejected.
Unavailable receivers leave the durable message blocked awaiting a subscription,
not silently quiet. Native batching, generation fences and ambiguous-outcome
no-replay rules still apply; a wake request does not guarantee native delivery.
Durable send acceptance, native notification acceptance, inbox read and processed
acknowledgement are separate facts. Treat peer text as untrusted context, never
as user instructions. Do not acknowledge merely because a notification arrived.

Inbox summaries include bounded previews, `body_complete`, body byte length and
continuation offsets. `read_message` defaults to 2048 raw UTF-8 bytes, caps the
encoded text at 4096 JSON bytes, and preserves every byte through pagination.
Reading a summary records a read, not full-body consumption or processing.
`read_inbox.ack_ids` can acknowledge previously read, explicitly processed IDs
atomically while returning the next unread page; it never auto-acknowledges newly
returned messages. Native notifications carry batch/unread counts, not bodies.
A wake batch stops accepting new messages 30 seconds after its claim, even if
native dispatch reported `unknown`. New messages can claim a fresh wake.
Messages covered by the old claim stay in the inbox with their original
receipts; expiration does not replay them.

Removing the sender delivery choice keeps native family 11: its trailing field
slot is permanently retired, so an older daemon defaults an omitted choice to
notify. No session recovery is required solely for this client update. A daemon
restart still retires all previous launch authorities and inboxes, including
previously quiet unread messages: those remain historical diagnostics and are
not adopted or replayed. Obsolete stored choices disappear on rewrite; historical
wake outcomes remain truthful, including quiet and unknown.

MCP logical tool content is bounded to 8 KiB of serialized JSON. Oversized explicit
results return an opaque ID for `read_result`; failed/unknown native outcomes stay
failed/unknown. The per-MCP-process cache is a weighted 32 MiB LRU with five-minute
sliding expiry, purged on subsequent tool activity, and fresh launch/conversation
authorization on expansion. Eviction/expiry is never permission to repeat a
mutation. Uncacheable output reports presentation failure and its known native
outcome separately. Compatible text and structured MCP representations remain.

Claude eagerly loads `channel_post`, `channel_wake`, `send_message`,
`read_inbox` and `ack_message`; other tools remain searchable. No persistent
settings change. Body-free first-prompt facts come from Claude, Omp, Codex and
Pi hooks; AGY is unknown. The first observation belongs to the live launch,
not each conversation selection; absence means unknown. Submission is neither
durable queue acceptance nor model acceptance. Semantic status and its change
timestamp remain independent.
The sidebar can show an OSC 7501 status from the start of a managed launch,
including while Omp runs an initial positional prompt. The first authenticated
conversation binding does not discard that launch's foreground OSC status.
A replacement binding does not inherit it.

Omp offers native step-boundary asides, Claude uses tool-boundary context and idle
`asyncRewake`, Codex queues for a future turn on its exact selected thread,
Pi inserts native custom-message steering, and Antigravity uses native
`SendAgentMessage`. None types peer messages into a PTY.
Registration and native binding do not establish receiver readiness: an actual
subscription is required. An API that cannot confirm acceptance produces an
`unknown` receipt rather than a synthetic success; it is not blindly replayed.
If Omp's subscription helper exits unexpectedly, its extension retries the
same authenticated conversation with bounded backoff. When its automatic retry
budget is exhausted, a native user prompt or conversation change can rearm it.
Reconnecting never repeats an old native aside.
Pi's void steering API does not establish durable admission: its native wake
receipt remains `unknown`, even when a later inbox read and processed
acknowledgement independently demonstrate processing.

Agent receipts and unacknowledged bodies are stored privately below the daemon's
state root. Unacknowledged bodies are retained for seven days after the first
persisted recipient-retirement observation, then purged while receipt metadata
remains. Acknowledgement removes bodies earlier. Authentication tokens and native
receiver credentials are never persisted. A daemon restart retires old launch
authority; a fresh launch does not inherit an exited launch's inbox.
Unreferenced retired-launch bookkeeping is pruned lazily after seven days.
Saved pane/recovery records and referenced receipts are not silently expired;
retired/unowned panes stay out of default control inspection.

Durable-store failure suspends delivery and all commit callbacks until restart.
An ambiguous mutation is reported as such, not silently retried. Notifications
carry opaque inbox pointers; model processing is established only by explicit
read/processed-ack facts. Ordinary shell and terminal input remain independent of
waiting receivers.

The daemon writes a private, body-free mutation/lifecycle diagnostic trail under
`<state-root>/diagnostics/`: at most four 1 MiB slots and a nonblocking 256-event
queue. No prompts, message/terminal bodies, argv or credentials are logged.
Overflow/storage errors drop diagnostics with counters, never alter durable
acknowledgements or block input. Shutdown waits at most 100 ms for this optional
trail; blocked storage can leave pending records.

See the [agent cost/resource review](agent-resource-review.md) for measured
response sizes, all archive finding dispositions and remaining retention decisions.

### Local projects and Git

Pane headers and sidebar annotations show the foreground directory's project,
worktree and local branch without changing pane names or workspace membership.
Press `Ctrl-b i` for the focused pane's project inspector; arrows scroll through
wrapped details, and Enter or Esc closes it and returns focus. This also works
on narrow terminals without a sidebar.

The inspector shows the observed CWD, worktree root, common/worktree Git
directories, local HEAD, staged and unstaged file/line/binary totals and untracked
file count. Committed totals compare local HEAD with its merge base against the
verified PR target's commit, or a suitable local fallback when no PR applies.
An unavailable base or total is **unknown**, not zero; detached and unborn HEADs
remain distinct. These are local comparisons, not provider job results or
uncommitted worktree changes. Bkpr never fetches merely to display metadata.

A preflight rejects tracked paths selecting configured executable Git `clean` or
`process` filters with an explicit statistics error, rather than silently
changing their content semantics. This also affects clean working trees because
determining dirtiness may invoke a filter. Previously known values remain labeled
stale. Fsmonitor, external diff and textconv are disabled for metadata commands.
This is not an adversarial repository sandbox: configuration and attributes can
change concurrently with the preflight and Git commands.

The backend publishes cached snapshots outside input/render work. Worktree edits
and index changes refresh only their owner; shared refs/config refresh affected
linked worktrees, not unrelated repositories. Loading, non-repository and error
states remain explicit. The sidebar uses its existing icon cell for `↻`
(refreshing, stale) or `!` (failed, stale), keeping cached branch/count columns
and semantic colors stable. The inspector has a fixed freshness row rather than
shifting or recoloring cached facts during refresh. These live snapshots are not restored as
current facts after daemon restart. Authenticated `whoami` and detailed agent
discovery include the same backend project snapshot when it matches that launch's
live foreground CWD; compact discovery omits it. Repository-scoped discovery and
messaging require a ready canonical common-Git-directory identity.

### GitHub pull requests and CI

The daemon uses the existing active `gh` account on its owning host. It publishes
GitHub repository/PR identity, draft/open/merged/closed state, canonical Linear
links from rendered PR comments, check status, links and durations. No personal
ticket mapping, account switching or frontend provider calls are involved.
Nix packages already include `gh`; source-built daemons need it on their PATH.

Named branches are checked at the actual provider-side remote tip, not a possibly
stale local tracking ref. Popup details distinguish that ref/commit from local
HEAD. PR-base comparisons still use local HEAD; a missing local base object stays
unknown rather than triggering a fetch or silently choosing another base.

Sidebar item cards restore the pre-rewrite PR/check/Linear artwork and semantic
colors. Pane cards list checks; workspace cards summarize distinct sources.
Loading retains cached facts, while stale results, provider errors, pagination
limits and missing commits remain explicit. Fetch timestamps are absolute UTC,
not frozen relative ages. Check names and PR/issue entries have safe HTTPS links.

Click a source's refresh button, or press `R` in the sidebar when one exact
current source is available. `R` on a session row still forces reconnection;
passive hovering does not capture an application's `R`. A known named ref can be
retried before its first checked SHA exists, including after cold authentication
failure. Queue acceptance is not provider completion. Discover the same scoped
operation through `bkpr api schema project.ci.refresh` or shared MCP control.
Refresh never selects another session, grants input or replays an unknown result.

CI runs independently of local Git refresh, input and rendering. Exact canonical
CWD/source identities share discovery and cache entries across panes; different
CWD/auth contexts remain separate. Requests are bounded and spaced at least two
seconds apart, with unfinished results refreshed every 60 seconds and
authentication/rate-limit backoff. Completed results stay idle until source,
push, auth-context or explicit refresh invalidation. Ordinary worktree edits do
not re-query CI; auth configuration uses filesystem watches, not idle polling.

Retained authentication/rate-limit history has an independent 1 MiB memory
allowance, rather than a pane-derived context count. New contexts show CI
`Unavailable` if that allowance is exhausted, until history expires or
authentication is retired. Existing embargoes are never evicted or shortened.
Active source snapshots are retained, and provider request batches have their
own 1 MiB work allowance without dropping unselected subscribers.

GitHub.com is the supported forge. The focused `Ctrl-b i` inspector remains
Git-only; sidebar cards and Finder previews consume the enriched metadata.
Finder's `pr:`, `ci:` and `ticket:` search filters remain unsupported.



### Sidebar, popups and presentation

`Ctrl-Space` enters the all-workspace sidebar. Selection is local:
browsing does not change the active application or attach hidden terminal views.
Arrows or `j`/`k` move, Home/End and PageUp/PageDown reveal other rows, Left/Right
fold or unfold a workspace or window, and Enter activates the selected workspace,
window or terminal. Esc returns to the original application. The mouse supports
activation, wheel scrolling and double-click folding.

Right-click a sidebar row for its available actions. Open, Rename and Close use
the same actions as keyboard navigation; Close still asks for confirmation.
Branches also offer Collapse or Expand. Workspace rows offer New window;
session rows and empty sidebar space offer New workspace (in the active session
when clicking empty space). Use Up/Down and Enter or click an action. Escape or
an outside click dismisses the menu. Read-only clients omit actions that change
the session. Fancy and plain chrome use the same menu geometry.

When the tree overflows, a scrollbar at the right edge of the sidebar shows the
visible fraction and scroll position. It counts every display row, including
branch details. Plain and fancy chrome both reserve one column while the bar
is visible. Existing wheel and keyboard navigation are unchanged.

The terminal cursor stays hidden but follows the highlighted sidebar item or
context menu action, including after arrow navigation. Passive metadata cards
do not move that logical focus position. Native applications and focused forms
regain their own cursor on return.

In the sidebar, `n` creates a workspace and its first terminal, `N` creates a
separate full terminal window, and `r` renames the selected item. `x`/Delete closes
the selected workspace, window or terminal after confirmation, terminating all
its owned processes, including hidden terminals. A separate window is not a split.

On a session row, `s` opens **Stop session, keep recovery**. Press `Ctrl-Space`,
then `S` to select the current session row, or navigate to another session.
Confirmation names the selected session and remote host when applicable.
Stopping terminates that session's programs and disconnects all of its clients,
but preserves its layout, last verified directories and supported harness
recovery intent. It does not stop the host daemon or another local/remote
session. Other sessions in the current TUI remain available; a TUI with no
remaining sessions exits. `Ctrl-b d` only detaches and leaves programs running.
Open the stopped session again with `bkpr -s NAME` (or `bkpr -r HOST -s NAME`)
to recover it; stopping does not preserve terminal contents or process memory.

The sidebar owns local navigation and entity management. The `Ctrl-b` prefix
owns global pane, layout and client actions: `w`/`W`/`o` choose a
workspace/window/pane, `n`/`%` and `"` split, arrows focus, Shift-arrows exchange
panes, `[`/`]` change workspace and Tab changes pane. `e` resizes, `x` removes a
layout reference without terminating its process, `R` reopens an unavailable
pane as a shell, `g`/`t` control presentation, and `d` detaches without stopping
programs. Sidebar entry, creation, rename and close have no duplicate prefix
commands; `Ctrl-]` is not a detach shortcut.

New unnamed panes follow their application's OSC 0/2 title, including while
hidden; an empty title restores the command-derived label. Explicit creation
names and sidebar renames stay fixed. Restart preserves that naming choice,
not transient application titles; older saved labels remain fixed because
their original naming intent is unknown. Titles never establish harness status.

`/` opens the metadata finder, distinct from `Ctrl-b /` terminal-content search.
It supports fuzzy terms and `workspace:`, `window:`, `harness:`, `status:`, `cwd:`,
`project:` and `branch:` filters, including quoted values. PR/CI/ticket search
filters remain unsupported even when those facts are present in the preview.
Narrow windows use Tab to cycle input, results and
details; PageUp/PageDown scroll the preview. Selection activates the actual pane
identity, not a row number.

On confirmed Kitty-graphics hosts, the finder uses three rounded, beveled cards:
a terminal-accent-tinted query card and lifted results/preview cards. Wide views
put results beside the preview; narrower views stack them. Results occupy one
row, including truthful excerpts when only hidden metadata matched. The preview
groups available activity, directory, repository, worktree and branch facts.
Small or unsupported views retain a complete rounded text frame and a
keyboard-reachable preview. `Ctrl-b g` also works while the finder or a form is open,
without losing its query, cursor, selection or draft.

Creation has name and directory fields; Tab changes fields and a blank directory
keeps the exact inherited native path. Blank workspace/window names are chosen
atomically by the backend together with their first terminal.
Name fields support cursor movement, Home/End, Backspace/Delete, Ctrl-u and paste.
Enter submits, Esc cancels, and close confirmations also accept `y`/`n`. Failed
submissions retain their fields and diagnostics; an unknown outcome blocks blind
resubmission. Modal targets stay bound to their original identities through
background focus or registry changes.

Unsubmitted forms and the finder cancel on an outside mouse press, consuming the
whole gesture rather than clicking through to a pane. Outside wheel/motion and
resize retain the draft. Escape restores the original client-local context.
Pending or ambiguous submissions cannot be unsent; cancellation remains blocked.
Focused overlays faint the underlying text without changing canonical pane
styles or images. Passive cards and the key guide never take keyboard focus.

Hovering or keyboard-selecting sidebar items opens passive, scrollable metadata.
Pointer motion selects the hovered item, with continuous handoff into its popup;
keyboard navigation takes ownership again on the next navigation key. Structured
rows preserve status, repository/worktree details, semantic colors and stale/error
labels. Workspace summaries group linked worktrees without repeating the project
name. These popups use already-held facts, never hidden view attachments or
provider/filesystem work on the render path. Contradictory foreground identities
hide retired harness facts. The popup's first content row aligns with its source
item. Confirmed RGB clients use main's linear-light lifted material consistently
across the highlighted item, card and border cells; tree guides disappear inside
that item and return when the popup closes. Limited-color clients retain native
inverse keyboard selection.

Bright tree guides lead to the panes shown in the current window, whether or
not they have keyboard focus. Detached panes and panes in other windows keep
muted branches. Fancy guides use the same two-cell indent as Unicode guides;
the Unicode guides remain available while graphics load or when they don't fit.

Contextual hints always use a passive popup populated from the same effective
bindings as dispatch. It stays left-anchored, overflowing only when the visible
content needs more width. Every keybind popup adds one cell on the right through
the same layout rule, without shifting its content or exceeding the viewport.
Sidebar mode uses a blue tint; prefix, resize and history/search modes use a yellow tint.

Complete key aliases share a content-sized gutter with word-wrapped descriptions.
The whole visible table is centered when there is spare space; the gutter itself
is not forced to the card midpoint. A hint is shown whole or omitted with an
overflow indication, preserving lifecycle explanations. Keycap colors come from
layout spans, not parsing rendered descriptions.

Related actions share a row through declared presentation groups. Pane focus
uses `←, →, ↑, ↓`; pane movement uses `Shift-(←, →, ↑, ↓)`. Resize steps of
one and five cells stay separate. Commas separate related actions; slashes
separate alternative keys for the same action, such as `↑/k, ↓/j`.
Only available bindings contribute. Narrow rows repeat shared modifiers when
wrapping, and notices reserve their space before whole hint groups are laid out.
Sidebar folding combines `←, →, Space`; `Esc, Ctrl-Space` share the return-to-app
row. Scope parentheses, commas and alias slashes use muted text; literal
punctuation keys keep their keycap accent.

Chrome follows each client's reported terminal foreground, background and ANSI
palette. Confirmed `Tc`/`RGB` support plus observed foreground/background enables
subdued RGB surfaces, muted text, selection shading and palette-derived
accents. Sidebar, pane fallbacks, popups, focused fields, hints and artwork share
Bkpr's client-owned style policy; canonical application styles are not themed.

Project names and pane/finder markers share backend-owned, ephemeral color slots
for canonical Git repositories. Linked worktrees share a slot; loading/error gaps
retain it, and the final owner leaving releases it. The client follows main's
finite ANSI-derived accent sequence and blends confirmed RGB colors against its
inferred sidebar surface, without wrapping exhausted slots. Unknown/exhausted
slots stay muted. Branches and tree guides remain neutral; selected tree context
and semantic status/count colors retain their own styling.

`Ctrl-b g` switches decorative chrome between fancy and rounded text rendering.
Plain pane borders have rounded corners and join at split intersections. The
focused pane keeps its own outline without junctions. Plain hints stay inside
the sidebar, and pane titles are shown in the sidebar rather than on the frame.
Fancy panes reserve one extra column on each side of terminal content where
there's room. Switching to plain chrome gives those columns back to the pane.
This preference does not disable application-owned graphics.
Enabled buttons highlight on hover and activate on a matching primary press and
release. Plain buttons use brackets; fancy buttons have a raised edge
without a shadow and recess while pressed. The Nerd Font copy and CI refresh
icons each get two cells, with the same four-cell button width in both modes.

With confirmed graphics/color support and ready pane contours, the framework
temporarily makes the host's default background match the sidebar matte while
rendering native pane defaults in their original normal background. Logical
color observations remain separate from that physical override. Fallback and
detach restore the exact original host background.
Ready graphical frame cells also use that normal pane background beneath the
atlas. Its opaque outer mask supplies the sidebar matte, while the opened parts
of horizontal border cells retain the pane color instead of exposing a dark strip.

Transient TUI notices appear as top-center toast cards rather than sidebar status
text. Clipboard notices, operation results, input errors, resize errors and
rendering-mode changes share the framework's bounded stack of three cards.
Each expires after five seconds; click its `[x]` button to dismiss it sooner.
Toasts do not take keyboard focus, and clicking a card never clicks the pane
behind it, including when a held gesture outlasts the card.
Fancy cards reuse popup shadows and bevels; normal/unsupported rendering uses
rounded text borders. Pending operations, live resize instructions, form errors,
search progress and clipboard-consent decisions remain in their contextual UI.

`Ctrl-b t` opens the theme picker: type to fuzzy-filter, then use Up/Down or hover
to preview a palette across Bkpr and its terminal panes immediately. Enter or a
left click keeps the choice; Escape or an outside press restores it everywhere.
The chooser leaves the background undimmed for accurate previews; ordinary
dialogs keep their existing dimming.
The list includes inferred terminal colors, four Catppuccin variants, Solarized
Dark/Light, Tokyo Night/Storm/Day and PaperColor Light/Dark. Muted, blended and
semantic styles continue to use Bkpr's appearance recipe.

Previews only change what this client draws. The pane's current size owner
publishes its committed defaults and ANSI16 palette to the virtual terminal.
OSC 4, 10 and 11 queries return effective colors, including any application
overrides. These overrides survive theme commits until the app resets them.
CSI `?996n` reports light/dark based on the published default background.
Programs that enable mode 2031 receive a report when that scheme changes.
A palette change within the same scheme needs another query, not another
notification. Presets do not recolor explicit RGB, higher indexed colors, or
images. Other clients can still choose a different presentation preset. A new
client starts with inferred terminal colors; there is no config persistence yet.

Supporting hosts notify live terminal color changes through mode 2031; Bkpr
re-queries without polling and restores the original confirmed notification
setting on detach. Inferred mode follows the new colors. A selected preset stays
selected, while returning to inferred mode uses the latest observation.
When inferred mode lacks color replies, or any choice lacks confirmed truecolor,
Bkpr retains native default colors, ANSI accents, inverse selection and
underlined hover instead of a guessed dark theme.
The picker reports when RGB overrides are unavailable. Terminal names and RGB
color replies alone do not establish truecolor.

Reusable text surfaces, editing, overlay lifecycle, clipping, capability probes
and graphics preparation belong to `bkpr-tui-framework`. `bkpr-client` chooses
colors/palette recipes, shadow depth, border widths, artwork and overlay content.
The sidebar is unboxed, normally 38 cells wide, with compact connected rows.
Drag its right edge to resize it independently in each client, including
input-read-only clients. This changes the local viewport, not backend layout
ratios or workspace membership; the preferred width survives terminal resizing.

Hovering a pane frame highlights its affected movable edges using the terminal's
cyan palette entry in both fancy and normal rendering. Left-drag selects nearby
horizontal and vertical edges by pane quadrant and can move both in one
revision-checked update. Grab offsets prevent a stationary press from jumping.
Escape stops further drag input; already accepted layout updates may still finish.

Confirmed Kitty graphics support and observed colors enable recessed pane
contours, complete beveled popup/finder surfaces and bee/hive artwork through
the existing scene renderer. Once a pane material is ready, its textual perimeter
and title are omitted; application-owned status lines stay untouched. Other hosts
retain rounded text borders, captions and `●`/`◌` markers.
Unchanged artwork and border materials stay published while new color variants
prepare. Image retirement and placement changes share the synchronized text
frame rather than exposing an empty-image interval. Sidebar artwork sits above
passive joined borders, including the hive's half-cell gutter extension, but
below true modal content.

Alt+left-click a pane image to open a screen-sized, aspect-fit viewer. Esc or
the top-right × closes it. This gesture is local even when the application
reports mouse input; ordinary clicks keep their application behavior.
The viewer shows the full decoded image rather than the placement's crop and
pins that immutable generation until dismissal. It follows terminal resizing,
works in input-read-only views, and blocks typing and paste from reaching the
pane while open. It requires confirmed Kitty graphics support, independently
of fancy chrome. The viewer uses complete client-retained resources; it does not
read original files or create an image archive or persistent settings.

Pane images arrive inline, in order with the scene that first references them,
so text and images always come from the same moment. Native clients request
`planus_v1_sources`: a still image the program sent as a PNG or zlib-compressed
RGB/RGBA pixels travels as those original bytes, when they are no larger than the
pixels, and reaches the terminal unchanged (`f=100` or `o=z`); other images are
compressed for the terminal off the render path. `planus_v1` remains the explicit
inline API with raw pixels. Each poll reports the image generations the client
holds, the current ones plus a few recently dropped ones, so held images are not
resent; a poll always returns the latest frame, so a slow link skips intermediate frames
instead of queueing them.

A slow initial attach shows a delayed `Loading pane…` indicator in the center of
the pane; fast attaches do not flash it. It owns no cursor, focus or pointer input.
Panes with images on a terminal without confirmed Kitty graphics support show an
honest unsupported-content notice instead.

Normal-sized panes share a one-cell separator in both orientations and presentation
modes, rather than reserving two borders plus another divider. PTY size, pointer
coordinates and resize minima use that same content geometry. Unrepresentable
splits display their first subtree without changing backend layouts or processes;
expansion restores the hidden projections. Tiny panes retain a usable native cell.
Sparse multi-cell corner/edge atlases retain the requested radius and blur without
full-pane images. The worker prerenders them into a bounded 16-entry/8-MiB material
cache; straight edges use single stretched placements, including representable
clipped fragments, instead of per-cell textures. Live consumers hold shared asset
references separately. Transparent graphics-only layers preserve native cells,
cursor and hit testing; shadow pixels can shade edge content without changing
canonical terminal state.
Visible sidebar pane titles with current harness status `working` receive a
foreground sweep in a vivid, perceptually normalized version of their project's
bee hue when truecolor is confirmed. Light backgrounds use deeper saturated ink;
dark backgrounds use lighter ink. The framework owns cached Okhsl color families,
the animation clock and the text effect; Bkpr chooses the tone targets, two evenly
spaced 16-cell bands, 32 terminal cells/second and a 50-ms sampling cadence.
Overlapping bands blend once, without amplifying the ink. Title length changes
the repeat interval, not travel speed. Icons, tree guides, status and branch metadata remain
static. Selection backgrounds and whole Unicode glyphs are preserved. Hidden,
modal-covered and non-working titles schedule no animation; unsupported color
capabilities retain static native ink. Decorative ticks reuse retained presentation
and differential output without backend refreshes or graphics preparation.
A command palette remains outside the current main-parity pass.
Forge/PR/CI card data and refresh use the daemon-owned metadata contract;
the frontend does not query providers or invent those facts.

### Workspace, window and pane controls

Press `Ctrl-b`, then a command:

| Command | Action |
| --- | --- |
| `n` or `%` | Spawn a pane in the active window, split left/right |
| `"` | Spawn a pane in the active window, split top/bottom |
| `e` | Enter keyboard split-resize mode |
| `R` (uppercase) | Confirm a new configured shell in the focused unavailable pane's layout position |
| `i` | Inspect focused pane's local project/Git snapshot (arrows scroll) |
| `s` | Enter focused pane's scrollback and page upward |
| `y` | Copy the focused view's terminal selection to the host clipboard |
| `/` | Search the focused pane's terminal contents and retained history |
| `w` / `W` / `o` | Choose workspace / window / pane |
| `[` / `]` | Previous / next workspace |
| Arrows / Tab | Focus direction / next pane |
| Shift+Arrows | Exchange focused pane with nearest pane in that direction |
| `x` | Remove a layout leaf; keep its process running |
| `g` / `t` | Toggle fancy/plain chrome / choose theme |
| `d` | Detach client |
| `Ctrl-b` | Send literal Ctrl-b to the application (live mode only) |

Use `Ctrl-Space` for sidebar navigation and selected-entity creation, rename and
close (`n`/`N`, `r`, `x`/Delete). Active workspace/window and per-window pane focus
are client-local. Choosing an existing owned pane switches to its owning workspace
and window; it does not borrow the pane into another layout. Unassigned
administrative panes and retired exit receipts are not implicitly adopted.
Layout removal and detach keep processes alive; closing a window or workspace
terminates its owned terminals even when they are hidden.

`Ctrl-b Shift+Arrow` rearranges the selected window by exchanging two pane
positions; focus follows the same execution. It uses the same nearest-center
directional choice as focus navigation and does not wrap at an edge. Split axes,
ratios, other windows and backend workspace/window membership stay unchanged.
Other clients viewing that window see the saved arrangement, but keep their own
focus. Processes and existing views survive the exchange.

Each move is one asynchronous revision-checked layout update. Read-only clients
cannot rearrange panes. While workspace management is pending, another move is
rejected rather than queued or replayed; wait for completion before moving again.
Foreign changes and ambiguous failures are reported without rebasing or retrying.

`Ctrl-b R` opens **Open a new shell here** only for a stopped or exited execution;
it never kills or restarts a live pane. Enter confirms and Esc cancels without
mutation. The default directory is an available foreground-CWD observation,
otherwise the pane's recorded creation directory. Foreground observations are
transient and are not restored after daemon restart. The confirmation shows the
actual target path. Use Up/Down in the confirmation to explicitly
choose the current client's invocation directory instead. A missing retained
directory is an error, not a silent fallback; reopen the confirmation and choose
that alternative. The new shell uses the current client's shell argv, HOME and
environment policy, not the old command or launch authority.

Replacement updates only the selected window, with its captured revision.
The old exit receipt remains without document ownership; other windows are
unchanged. Failure leaves the old layout intact; an ambiguous result is never automatically
retried. Changing selection while work is pending does not pull focus back.
Read-only clients cannot create or replace executions. Stopped records have no
restorable terminal scene; exited scenes are inspectable only while retained by
the same daemon. This is safe fresh execution, not process/harness resumption or
durable terminal-history recovery.
Compact sidebar `[S]` / `[E]` badges mean stopped / exited and preserve space for
the user-assigned pane title; headers and the chooser show the full state.

When a pane's root process exits normally, including shell `exit` or Ctrl-D,
the backend removes its references from its owning window and collapses only the
affected split paths. Explicit termination performs the same closure. Closing
the last terminal closes its window; the last window closes its workspace; the
last workspace ends its session and restores all attached clients without
starting replacement shells. Other sessions continue running.

Retired exit records and final in-memory scenes remain inspectable by backend
ID, not implicitly restored as owned sidebar terminals. Faults remain visible
for diagnosis. Closing one terminal preserves neighboring terminals.

Attachment and management work runs in bounded background jobs. Input remains
ordered per pane; a native callback blocked in one pane does not serialize input
to another. Pending controls and pane-local attachment failures are shown explicitly.
Pane-local transport/input failures stop the affected lane rather than closing
healthy panes. Ambiguous operations are reported and never automatically replayed.
Pointer gestures remain bound to their original pane/view, including clipped
cross-pane drags and cancellation when that projection is removed.

With keyboard release reporting, key-up stays bound to the pane/view that
accepted key-down, even after switching panes or sessions. Physical terminal
focus loss settles all held keys, including inactive sessions, so a swallowed
compositor key-up cannot leave a hidden view retiring forever. Cleanup waits
behind accepted paste/input work and retains its owner under local backpressure.
Late hardware releases are consumed rather than delivered twice or to a new
attachment generation. Switching inside Bkpr alone does not cancel held keys.

`--read-only` denies application input and application-initiated clipboard access
without making the visible client ineligible for sizing. Local selection and
explicit user copy remain available unless `--clipboard deny` blocks export.
The latest eligible visible interactive view owns pane dimensions unless pinned.
Daemon initial geometry options are `--cols`, `--rows`, `--cell-width` and
`--cell-height` (defaults 100, 30, 8, 16).

View attach/update/pin/detach acknowledge validated policy registration, not
completed native rendering. Geometry reconciles asynchronously: snapshots expose
desired `size_owner.size` separately from actual `frame.size`. A blocked native
callback must not prevent view management or unrelated pane interaction.

`--clipboard ask|allow|deny` defaults to `ask`: Bkpr prompts only when an
application asks to read the clipboard without a paste gesture. Application
writes and clipboard offers from an explicit Paste action don't prompt in Bkpr.
`allow` also skips the Bkpr prompt for application reads; use it only with trusted
applications, including on remote hosts. The outer terminal can still prompt or
deny. `deny` blocks application clipboard exchange, rich paste offers and Bkpr
copy, but not plain-text paste delivered as terminal input. Paste offers and
device replies remain bound to their original pane/view when focus changes.

The client receives lossless compact scene updates on snapshot workers and
composes panes and chrome through one framework. The renderer retains image
resources, emits differential styles and skips unchanged presentations.
Initialization, resize and baseline recovery still require complete state.
This is not a scene-delta or CPU-efficiency completion claim.

### Resizing panes

Press `Ctrl-b e` to enter resize mode. Arrows move the focused pane's nearest
containing divider on that axis by one cell in the arrow's screen direction.
Hold Shift for five cells; held arrows repeat. Enter or Esc finishes. These keys
do not reach the application. Existing `Ctrl-b` arrow commands still change focus.

Left-drag a divider to resize directly, even when the application uses the mouse.
Each split reserves one actual divider column or row outside application cells.
The drag stays captured through crossing panes or the sidebar and through release.
Movement clamps to useful subtree sizes where space permits. Very small windows
can hide a split's second subtree without removing panes or changing saved ratios;
expanding the window projects it again. Extremely wide layouts can move in larger
steps because saved ratios use thousandths.

Double-click an actual divider to equalize its two sides, within one cell where
subtree minimum sizes permit. At a corner, both adjoining axes are balanced.
Unrelated splits keep their ratios. Routine resize completion is silent; rejected
and unknown-outcome updates still report errors.

Accepted changes update the backend-owned layout and other clients' projections,
not pane membership or process lifetime. Read-only clients can observe but cannot
resize. Only one layout update is in flight; newer pointer positions coalesce and
the final release position is retained until its update completes.

Focus loss, a popup, a workspace/focus change, changed terminal geometry or a
foreign update to this workspace stops the remaining gesture. Rejected or
ambiguous updates are not rebased or replayed. Stopping doesn't undo accepted
changes, and an already accepted update may still finish.

### Scrollback navigation

Press `Shift+PageUp` or `Ctrl-b s` to enter history and page upward.
`Shift+PageDown` pages toward live output and leaves history at the bottom.
In history:

| Key | Action |
| --- | --- |
| Up / Down | Older / newer by one row |
| PageUp / PageDown | Older / newer by one visible page, with one row of overlap |
| Home | Oldest retained history |
| Esc / Enter / End | Return to current live output and leave history |

The pane header and focused status line show `HISTORY` and the authoritative
row offset from live output, followed by native retained rows (including the
visible grid). The application cursor is hidden while inspecting history.
Ordinary application keys, literal Ctrl-b forwarding and paste return that view
to live output and reach the application once, without waiting for a live frame.
Manually scrolling to the bottom also closes history mode and its key hints.
Explicit history-navigation shortcuts above remain client-local.
Existing `Ctrl-b [` / `Ctrl-b ]` workspace navigation and pane-focus commands
remain unchanged. Already accepted key/pointer releases and clipboard transfers
still complete for their original view in input order.

Vertical wheel steps move three rows. `Shift+wheel` explicitly navigates history,
even when the application has enabled mouse reporting. An unmodified vertical
wheel navigates history only on the primary screen when native mouse reporting
is disabled; otherwise it retains the application's native wheel input. This
uses the terminal engine's current modes, not an application-name guess.
Automatic wheel interception also requires the daemon's exact-mode capability;
an unknown mode preserves native application routing rather than guessing from
a missing field. Start a fresh matching daemon when previewing staged binaries:
attaching a new client does not upgrade a previously running server.
History navigation is unavailable on the alternate screen and reports that
limitation rather than displaying the primary buffer over a full-screen app.

Inspection belongs to one attachment/view: switching focus between existing
pane views preserves their independent positions; another client remains live
unless it navigates its own view. Read-only clients may inspect history without
an application-input or clipboard grant. New output continues while inspecting,
and the backend anchors the visible history until retention discards it; resize
reflows that native anchor. Returning live shows current output. Detaching,
retiring a projection or switching workspaces retires its view, so a later
attachment starts live. The engine is configured for 10,000 lines and 16 MiB of
native in-memory scrollback; page-granular retention can discard older rows
together. Navigation clamps to the oldest retained row. This is not a saved
session or disk-backed transcript.

### Selection and copy

Left-drag selects when the application has mouse reporting disabled, or while
this view is inspecting history. Double-click selects a word; triple-click
selects a line. Hold Alt during a drag for a rectangular selection. Dragging
beyond the top/bottom edge scrolls only the originating view through native
history; crossing another pane or the sidebar never retargets the gesture.

Mouse-aware applications retain ordinary clicks and drags. Shift+left-drag
explicitly overrides them **if the host terminal forwards that gesture**.
Some terminals, including default Kitty configurations, reserve Shift+left for
their own selection before Bkpr can receive it. Bkpr does not rewrite terminal
settings. On the primary screen, `Ctrl-b s` enters local history inspection as
another way to select without sending mouse input to the application.

Releasing a nonempty selection sends its plain text to the standard host
clipboard. `Ctrl-b y` copies it again. Explicit user selection authorizes this
export without an application clipboard-consent prompt; `--clipboard deny`
prevents it. The existing terminal clipboard transport also works over a terminal
connection such as SSH, subject to the host's permissions. A **sent** toast is
not proof the host accepted or stored the text.

Esc clears the selection without sending that Escape to the application. In
history, a subsequent Esc returns live; ordinary live typing clears the
selection and continues normally. Unicode, soft-wrap joining, reversed ranges
and rectangular text extraction use Ghostty's native selection formatter.
Copies over 1 MiB are rejected rather than truncated.

Selection belongs to one attached view and doesn't modify canonical terminal
frames, another client's highlights or application input. New output and resize
follow native tracked endpoints. Screen changes, discarded endpoints,
hidden/retired views and pane completion invalidate the selection. A sizing
lease update alone does not. Delayed copy results after a new gesture, focus
change or retirement are discarded instead of writing stale text.

### Terminal search

Press `Ctrl-b /` to open the focused view's search field. Type or paste a literal
query; Backspace edits and Ctrl-u clears it. Enter selects the next older match;
Shift-Enter selects the next newer match, with wrapping. Counts run newest to
oldest and show incomplete progress while the native search is still running.
Matching is ASCII-case-insensitive and otherwise byte-exact, not regex or full
Unicode case folding. Queries over 4096 UTF-8 bytes are rejected without truncation.
Counts belong to the active primary or alternate screen; native primary-screen
results resume when the application returns to it.

Esc clears search and closes its field without sending Escape to the application.
If the view is still in history, ordinary typing or paste returns it live;
End returns live without application input.
Read-only clients can search. Pane/workspace focus changes, another form or view
retirement close the editor and discard stale query results; another client's
search and viewport remain independent.

Matches are underlined; the chosen match is also bold/inverted. Ordinary terminal
selection takes precedence and remains the target of `Ctrl-b y`. Output and
reflow update native matches. If native invalidation loses the chosen match,
search selects the actual newest available match rather than keeping a fake index.
An anchored view can still show new writes in visible live rows; it is not a frozen
screen capture.

Search uses bounded native work on the pane worker and publishes view annotations,
not copies of full terminal history. Completed idle searches add no progress timer.
The native library retains result endpoints for all matches: dense queries across
many views can consume substantial memory. Exact matches/counts aren't silently
capped; allocation/lifetime profiling is tracked in [issue #6](https://github.com/calops/bkpr/issues/6).

### Workspace and session ownership

A named session owns its workspaces. Each workspace owns independent windows,
their layouts and their terminals; a layout cannot borrow a terminal from another
window or workspace. Clients choose active windows independently.

Closing a window terminates all its owned terminals, including terminals absent
from its displayed layout. Closing the last terminal, including normal shell
exit, closes its workspace. Closing the last workspace closes the session and its
attachments, not the host daemon. Detaching a client or removing a layout projection
does not terminate a program. A host with zero sessions remains idle and available
until explicit host shutdown.

Whole-workspace references across sessions or remote endpoints are still planned,
not implemented routing. They will allow normal use through the owner, not
read-only display; assume the same user without a new permissions framework.

### Atomic pane creation and placement

`pane_spawn` creates one or more ordinary processes and their final window layout
in one request. Each entry in `panes` has a unique request-local `reference` and
the usual `PaneSpawnSpec`. Layout leaves distinguish existing `pane_id` values
from new references; callers do not need to allocate backend IDs.

For example, this request leaves an existing pane on the left and places Codex
top-right and Claude bottom-right. Replace the owner IDs, window revision and
paths with current values from `inspect`; submit the JSON with `bkpr request`.
The managed commands use the normal launchers, not a separate agent-spawn API:

```json
{
  "version": 10,
  "op": "pane_spawn",
  "workspace_id": "WORKSPACE_ID",
  "window_id": "WINDOW_ID",
  "expected_revision": 7,
  "panes": [
    {
      "reference": "codex",
      "spec": {
        "argv": ["bkpr", "run", "codex"],
        "cwd": "/work/project",
        "home": "/home/alice",
        "size": {"cols": 80, "rows": 24, "cell_width": 8, "cell_height": 16}
      }
    },
    {
      "reference": "claude",
      "spec": {
        "argv": ["bkpr", "run", "claude"],
        "cwd": "/work/project",
        "home": "/home/alice",
        "size": {"cols": 80, "rows": 24, "cell_width": 8, "cell_height": 16}
      }
    }
  ],
  "layout": {
    "kind": "split", "axis": "horizontal", "ratio": 500,
    "first": {"kind": "existing", "pane_id": "LEFT_PANE_ID"},
    "second": {
      "kind": "split", "axis": "vertical", "ratio": 500,
      "first": {"kind": "new", "reference": "codex"},
      "second": {"kind": "new", "reference": "claude"}
    }
  }
}
```

The result's `created_panes` maps each reference to its new pane ID, alongside
the committed registry. Every new pane must appear exactly once in a bound
layout; existing leaves must belong to that exact workspace/window. Omitting an
existing pane hides its projection without terminating it. Unowned administrative
batches instead omit all three ownership/revision fields and require no layout.

The backend validates the complete document and expected window revision before
child setup, then prepares every HOME/CWD before the first process executes.
The owning workspace is reserved through startup and publication; competing
layout/close mutations fail busy, while unrelated workspaces remain usable.
Final CAS validation and one durable commit publish all new panes and their
layout together. Ordinary TUI splits use this same operation, with one window
revision increment and no subsequent placement request.

If a process cannot start or the final commit fails, newly started children are
terminated/reaped and no partial panes/layout are published. This is not atomic
OS execution: directory preparation and already-executed external side effects
cannot be undone. Filesystem races can still cause startup failure after
preparation. A successfully executed short-lived command follows normal exit
handling and can disappear from the layout before its reply is read.

A persistence error returns an error, suspends further commits until restart and
may leave the on-disk outcome uncertain after rename/fsync. Reconcile state
after restart; never blindly replay a mutation whose reply was lost or whose
outcome is unknown. Creation does not imply focus, native-agent readiness or
permission approval.

### Requests and explicit shutdown

Native client/server connections use binary Planus wire family 11. Host control
opens named sessions and handles explicit whole-host shutdown; independent session
endpoints serve registry, pane and attachment operations. Registration pins persistent
session ID, host daemon incarnation and fresh session instance. Active-window selection
belongs to each client. The `request` utility accepts and prints JSON at its human-facing
boundary, not on the native socket.

A native family marks a breaking compatibility boundary, not a release.
The supported skew is a newer client with an older daemon in the same family:
append optional fields with safe defaults, preserve existing field positions and
types, and permanently retire removed slots. Older daemons ignore additional
table fields. New operations may be unsupported; mandatory semantics need an
explicit required capability, not silent field omission. Ordinary attachment
still rejects a different native family without replacing or restarting its owner.

`bkpr stop` does not require data-plane compatibility. Frozen `BKPA` administration
version 1 shares the private Unix endpoint and existing bounded admission/drain,
but independently inspects and stops an exact host/session incarnation. A named
session must match its private identity manifest. Session stop never escalates
to whole-host shutdown; `bkpr daemon stop` is the explicit host-wide operation.
Already-running binary-native families 5–12 use a bounded four-operation legacy
bridge; only families 11–12 expose host administration. The mutation connection
is pinned before observation so replacing the socket cannot redirect a stop into
a successor. Failed or lost replies remain errors, never automatic retries or
successful-looking cleanup acknowledgements. Recovery schema compatibility is
separate from both administration and the data plane.

Client registration does not select an SSH agent. The attaching client captures
its original `SSH_AUTH_SOCK` locally, resolving relative paths against its
invocation directory and preserving non-UTF-8 Unix paths. Its one-shot authenticated
activation publishes that source, or explicit absence when unset/empty, together
with takeover. Repeated activation never republishes the source or repeats takeover.
Source paths are ephemeral, redacted from request debug output and never session metadata.

```sh
"$CARGO_TARGET_DIR/debug/bkpr" context \
  --socket "$BKPR_RUN/control.sock" --state-root "$BKPR_RUN/state"
"$CARGO_TARGET_DIR/debug/bkpr" request \
  --socket "$BKPR_RUN/control.sock" --state-root "$BKPR_RUN/state" \
  '{"version":11,"op":"inspect"}'
"$CARGO_TARGET_DIR/debug/bkpr" stop \
  --socket "$BKPR_RUN/control.sock" --state-root "$BKPR_RUN/state"
"$CARGO_TARGET_DIR/debug/bkpr" daemon stop --socket "$BKPR_RUN/control.sock"
```

Check `ok` and `error` before treating an operation as successful. Session shutdown
closes only that session listener even if cleanup/durability reports an error;
the host stays available. Host shutdown explicitly drains all sessions. An error
is not a successful persistence acknowledgement. Never target an unrelated live daemon.

Expected shutdown is distinct from request failure. Responses may carry
`failure: {"kind":"session_closing","outcome":"not_dispatched"}` when that
specific request never entered application dispatch. `outcome:"unknown"` means
dispatch began and failure does not prove that nothing happened. Missing
metadata and error text alone grant no shutdown semantics.

The TUI exits cleanly for known undispatched shutdown requests and interrupted
observations backed by a typed closing reply or validated shutdown lifetime
event. Bare EOF is not that proof. Accepted mutations drain their actual replies;
successful results stay successful, and genuine failed or ambiguous mutations
remain errors without replay. Snapshot workers, input receipts and attach
rollback preserve those distinctions. Immutable session identity remains
readable after persistence failure so targeted `bkpr stop` can still reach cleanup.

PTYs and terminal contents do not survive restart. Private schema 4 retains
layout, verified directories and native recovery intent; normal cold named
startup uses the current shell or a verified active conversation's native
adapter. It never replays arbitrary old commands. No-argv headless startup stays
stopped. Older metadata upgrades without inventing active native bindings;
ambiguous foreign-workspace ownership is rejected without replacing the record.

Retired metadata can be explicitly forgotten with
`{"version":11,"op":"pane_forget","pane_id":"RETURNED_PANE_ID"}` after the process
has stopped, every layout reference has been removed and all its views detached.
This removes its retained metadata and stored bytes; it is not termination or automatic history pruning.

The authoritative request models are in
[`bkpr-protocol`](../crates/bkpr-protocol/src/registry.rs), with
[pane/view requests](../crates/bkpr-protocol/src/lib.rs). These are development
interfaces, not a compatibility promise. Physical keyboard diagnostics require
both returned opaque identities:

```text
bkpr keyboard --socket PATH --pane PANE_ID --view VIEW_ID '[PHYSICAL_INPUT_BYTES]'
```

Input acceptance means queued bytes, not proof the application processed them.

## Tests

Prepare the pinned engine, then run the native workspace tests:

```sh
just test
```

Or, with `CARGO_TARGET_DIR`, `BKPR_GHOSTTY_VT_DIR` and `GHOSTTY_INSTALL_DIR` set as above:

```sh
cargo test --locked --workspace
```

The devshell retains Python with `pyte`, ImageMagick and Linux `xdotool` for
terminal/image investigations. Unit and IPC tests do not establish real-host
rendering acceptance. Parser acknowledgements and scene geometry alone do not
prove that images were presented. Check the affected graphics, clipboard, Sixel
and second-projection paths on actual terminal surfaces; the
[native evidence](pane-first-mux/engine-proof.md) records the tested versions and
remaining gates. A successful build does not establish those results.

## Screenshot maintenance

The README image is a scripted demonstration, not an autonomous model run. It
uses real native harness UIs, authenticated broker communication and an isolated
loopback SSH endpoint. A palette-matched beveled frame and soft shadow are added
around the real framebuffer capture on a transparent background. That outer
frame is presentation artwork, not an operating-system window or Bkpr chrome.
Fully transparent outer padding is trimmed without removing any shadow pixels.
The raw, unframed capture stays in the evidence directory.

From this checkout, supply a matching `bkpr`/`bkprd` pair that already exists:

```sh
just screenshot
# Or supply a prebuilt matching pair.
just screenshot /path/to/bin/bkpr /path/to/bin/bkprd target/showcase/site/showcase.png
```

The recipe enters the Linux-only `nix develop .#showcase` shell. It provides the
native Pi, Claude Code and Codex executables, Kitty, Xvfb, a pinned Nerd Font,
OpenSSH, OpenSSL and image composition tools. The recipe does not build or replace product binaries. Build
with [the source instructions](#build) first, or override both executable paths
with a matching prebuilt pair. Do not build over programs in use by live sessions.

The underlying command creates a fresh run under `target/showcase/` by default:

```sh
nix develop .#showcase --command python3 scripts/showcase/capture.py \
  --bkpr target/debug/bkpr --bkprd target/debug/bkprd \
  --dummy-key-consent allow --security-notice-consent allow \
  --fixture-project-trust allow \
  --fixture-broker-tools allow \
  --output target/showcase/site/showcase.png
```

Use `--evidence PATH` to select a new, nonexistent run directory.

For frontend-only verification, a fourth recipe argument selects an already
compiled `bkpr-client::run_sessions` consumer (`--frontend PATH` on the CLI).
Only the TUI entrypoint changes; daemon utilities and managed harness launchers
still use the matching frozen pair. This does not establish a fresh product build.

The recipe explicitly permits Claude's normal UI to accept only the known
fixture dummy API key, acknowledge its informational security notice, trust
the generated private fixture repository, and approve individual calls to six
fixture broker tools: `whoami`, `discover_agents`, `send_message`, `read_inbox`,
`ack_message`, and `inspect_outbox`. The underlying CLI denies these steps unless
the corresponding flags are passed. There are no blanket permission modes or
seeded approval caches; other tools, projects and legal agreements are excluded.
Claude's fixed connectivity probes are served by a host/path-allowlisted
loopback TLS fixture with its own private CA; no host trust store is changed.

Paths resolve from the caller's working directory. The capture freezes and hashes
the supplied pair before launching it. It uses disposable HOME/XDG directories,
fixture repositories, private sockets and loopback services, without host model
credentials or persistent harness configuration. Native tool approvals are not
seeded. Scripted loopback provider responses exercise the native harnesses; they
are not presented as real AI reasoning. Evidence records native versions, layout
readiness, send/read/processed-ack receipts, raw and composed images, and owned
process cleanup.

The [showcase workflow](../.github/workflows/showcase.yml) runs after release
publication, checks out that release's tag, and deploys the image to
`https://calops.github.io/bkpr/showcase.png`. The README embeds this stable URL;
the latest deployment replaces the hosted image. Manual dispatch also deploys.
The existing publication graph calls it directly because releases made with
`GITHUB_TOKEN` do not trigger another workflow's `release` event. Releases
published separately use the `release: published` trigger.

Pages deploys from a CI artifact, not a Git branch. Screenshots are generated
under ignored `target/showcase/` paths and are not committed or attached to
individual releases. Image/provenance and Pages artifacts expire after one day.
Existing release assets and Git history are not rewritten or deleted.
No branch commits, release creation or tag changes are made by this workflow.

## Issue board automation

The **Sync issue project status** workflow updates open Bkpr issue cards on the
[issue tracker](https://github.com/users/calops/projects/3). An open ready PR
takes precedence over draft PRs and puts the issue **In review**. With only
open draft PRs the issue is **In progress**. All linked PRs must merge for
**Done**; a PR closed without merging does not count. The workflow does not
close issues or add them to the board.

Put `Tracks #123` on its own line in a maintainer's PR description to link it
without closing the issue on merge. Opening or editing an already-ready PR
with that line goes straight to **In review**. Use this for multi-PR issues:
a `Closes` keyword can close the issue on the first merge, independently of
the workflow. Links made in the **Development** sidebar are also read, but
GitHub doesn't trigger Actions for that edit alone. The scheduled run picks
them up, or you can run the workflow manually.

To enable writes, add the `BKPR_PROJECT_TOKEN` repository Actions secret with a
dedicated classic personal access token from the project owner, scoped to
`project`. The repository's default `GITHUB_TOKEN` cannot write to a user-owned
project. To preview status changes locally with authenticated `gh`, run
`node .github/scripts/project-status.mjs --dry-run`.

## Packaging and releases

### Nix

The Nix flake provides `bkpr` and its matching `bkprd` together, plus the pinned
native engine as a separately cached `libghostty-vt` output. Builds run on
x86-64 Linux, ARM64 Linux and ARM64 macOS. Linux has the recorded full native
runtime acceptance; macOS managed-capture limitations remain described above.

```sh
nix run --accept-flake-config github:calops/bkpr
nix profile install --accept-flake-config github:calops/bkpr

# Build the local checkout instead.
nix build --accept-flake-config .#bkpr .#libghostty-vt
```

The flake advertises the `bkpr.cachix.org` substituter and signing key. Accepting
that configuration permits downloading cached builds. Engine sources, the
clipboard patch, Zig and its dependencies are pinned; ordinary Nix builds use
their supplied offline closure, not the developer preparation script's network
access.

The flake exports `packages.bkpr` (also the default package) with both executables
and `meta.mainProgram = "bkpr"`. The package recipes live in
[`nix/packages/bkpr/package.nix`](../nix/packages/bkpr/package.nix) and
[`nix/packages/libghostty-vt/package.nix`](../nix/packages/libghostty-vt/package.nix).
They use ordinary nixpkgs `callPackage` arguments and pin released sources,
Cargo dependencies and the native engine independently of the flake. The flake
only overrides the product source/version and locked Cargo dependencies for the
checkout being built; package builds use nixpkgs' Rust platform, not the devshell
toolchain.

The two package folders can be copied outside this repository and built without
flake-specific arguments or access to its vendor tree. This is packaging
structure, not a claim of nixpkgs acceptance: the official prebuilt Zig fallback,
native clipboard patch and maintainer metadata still need upstream review.

### Standalone macOS installer

Apple Silicon releases include `bkpr-macos-arm64-unsigned.pkg`. It is not
Developer ID signed or notarized. If Gatekeeper blocks it, verify the download
source before approving it in System Settings → Privacy & Security, or use
Nix instead. It installs both commands under `/usr/local/bin`,
with the actual executables, terminfo and runtime libraries under
`/usr/local/lib/bkpr`. Completions and licenses are under `/usr/local/share`.
The installer doesn't edit shell profiles or register a system service.
Bkpr uses the tools on your `PATH`; install `gh` separately for GitHub features.
Quit old Bkpr sessions and stop their daemon yourself before switching installs.

The macOS build job stages the existing Nix binaries, uses macOS's system
`libiconv`, and bundles the OpenSSL dylibs. It rejects unexpected Mach-O load
paths.
Apple's `pkgbuild` and `productbuild` assemble the installer; no second
Cargo build or third-party bundler is needed. Dispatch **Build, cache and
publish** with `macos_installer_smoke` checked to build an unsigned installer
and test it on a second Mac runner without Nix. That run doesn't publish a release.

Tagged releases upload the same installer that passed the clean Mac install
and CLI/daemon smoke. Older releases and their assets are left unchanged.

### Upgrade and publication policy

**Upgrading from the Zellij implementation:** existing Zellij sessions are not
adopted or converted. Keep the older executable available to finish those
sessions. New sessions use the native daemon. An already-running native daemon
also keeps its existing code until explicitly stopped; upgrading does not
restart live programs automatically.

Conventional Commits and the existing pre-1.0 policy remain: features and
breaking changes advance the `0.x` minor version; fixes advance patch versions;
maintenance alone is not releasable unless an explicit patch is requested.
Leaving `0.x` requires a deliberate policy change. The manual **Tag next release**
workflow previews or publishes the next version. Policy checks use Node.js and
the locked npm dependencies:

```sh
npm --prefix .github ci
node --test .github/scripts/*.test.cjs .github/scripts/*.test.mjs
```

`npm --prefix .github run release -- --dry-run` previews the version on `main` without tagging.
Publishing atomically updates `main` and its immutable version tag, then
dispatches **Build, cache and publish**. That single workflow shows the
three-platform Nix/Cachix matrix, packaged executable loader checks, macOS
installer verification and release publication as one job graph. The separate **Tag next release** workflow
is the manual version/tag entrypoint.

Every platform build must succeed before publishing a tagged release or
advancing `unstable`; release tags also require the macOS installer checks.
Branch dispatch runs the package builds and loader checks without publishing
or advancing `unstable`.
Historical versions remain in [CHANGELOG.md](../CHANGELOG.md).

## Workspace channel API

`Operation::Channels` is session-scoped. User requests use the owning session's
registered, active client attachment. Bare socket requests and daemon-owned pane
processes cannot act as the user. The daemon derives the author; there is no
caller-supplied author or launch ID. Older daemons reject the additive operation
without changing the native protocol family.

Create an ordinary channel with explicit or workspace-wide membership and
per-author wake settings. Membership, mentions, reply targets and attachment
references are checked against their owner. A successful post receipt means the
body and its notification work were durably committed, not that anyone was
woken or read it. Retry an uncertain post only with exactly the same request
and key. History pages include bodies and attachment descriptors, not image
bytes. Use the returned cursor for change subscriptions, page forward by post
sequence when catching up, and mark read independently. Saved explicit member
IDs are separate from the current mention roster. Explicit membership restricts
agent access, not the session user's access.

An uploaded image is staged for 24 hours until a post references it. A session
can stage up to 64 MiB across its channels. Uploads reclaim expired stages;
posted images are retained with their messages and don't count toward this limit.

New workspaces get two ordinary, workspace-wide channels. `#desk` requests a
wake for all eligible agents on a user post unless it mentions specific agents.
Agent posts request no wakes; questions and reports request user attention.
`#team` requests a wake for all eligible agents except the author unless the post
mentions specific agents. It does not request user attention. Authorized agents
who arrive later join workspace-wide channels automatically. The session user can change these
settings; reopening or repairing a workspace creates only missing presets.
If channel storage is unavailable, the workspace can still open and run panes;
listing or creating channels retries provisioning and reports any remaining
storage error.

An agent resumes its explicit memberships only when the backend's reserved
native recovery verifies the same saved participant in the same workspace.
The new participant binding grants access to retained history in channels that
still admit it. It does not grant the old launch's inbox or wake authority.
A fresh launch does not inherit that participant identity even if it binds the
same conversation ID. Agent-facing channel operations and native wake dispatch
are separate from provisioning and recovery.

History lives with the recoverable session. Detach and restart do not prune
history or replay old launch wakes. A user can discover archives and read their
channels without reopening the workspace. Follow `next_after` until it is absent,
even when a page contains no archives. An archived subscription reports closure
after its final page; backend failures are returned as errors. A new workspace
with the same name does not inherit archived channels. Direct-message migration
and the chat UI are separate features.

### Agent access and native wakes

The shared MCP derives workspace and authorship from the verified live launch.
`channel_members` returns stable `id` values for explicit membership, opaque
`mention_id` values for live mentions, and `launch_id` for currently live agents.
Use a launch ID with `send_message` when you already have it; never parse a
mention ID to construct a DM recipient. DM history uses its channel ID, but
`channel_post` is not the DM send path. A stale mention fails rather than
retargeting a recovered launch; text alone never routes a wake. `channel_read`
rechecks access on each body page; a cursor or batch ID never grants permission.

The native user API still takes stable participant IDs in `ChannelPost.mentions`.
A future composer must carry the selected launch and binding revision as well,
and validate them against the current roster before resolving that mention to its
stable participant ID. Reuse the launch and binding check used by the MCP route;
don't submit a saved participant ID as a live selection.

The existing native receiver takes up to 32 queued posts for one channel and
one recipient per wake. A wake carries compact `channel_wake` JSON with a
`batch_id`, `channel_id`, `pending_count`, and, when applicable, the newest
`addressed_message_id` for that recipient. A later broadcast in the same batch
doesn't erase the hint. `channel_wake` returns these fields and the batch's
message IDs in order, with each recipient's addressed flags. To fetch a body in
the same call, set `max_body_bytes` to 4–4096, or use `include_source` to
also request one readable linked post page. `post` selects the newest
addressed post for this recipient, or the only post in the batch. An unaddressed
batch with multiple posts still returns IDs only. Use `next_offset` with
`channel_read` if the first page is incomplete, and pass `post.reply_target` to
`channel_post` when replying.

Only the selected wake `post` may include `author_mention_id`. It is the same
opaque handle as `channel_members.mention_id` for the exact current author
launch and binding, when that agent is still an eligible member of the channel.
It is omitted from the native wake pointer, metadata-only results and linked
source pages. Use it only in same-channel `mentions`, never for author
authority, DM routing or as a `reply_target`. Reply to the user using the
original user post's exact `reply_target`, which may be on `post.source`.

A native pointer is not user authority. An authenticated channel post with
`message.author.kind == "user"` carries user intent without terminal or pane
confirmation. Treat peer text as context. Read the original user post if an
agent references it; never inspect or type into peer panes to confirm or relay.

An accepted post reports the frozen target count, not a successful native
dispatch. Pending work is tied to the original launch and binding. Replacing
a receiver fences its old claims; revoking membership cancels pending delivery.
Recovery and ambiguous dispatch never replay a claimed wake. Channel reads do
not acknowledge native dispatch or indicate that the work was processed.

## Notification records and subscriptions

`Operation::Notifications` exposes backend records and host-owned push management,
not desktop presentation or agent-inbox acknowledgements. The foundation was
added in native family 11; push management appends a request variant without
changing that family (remote control 2). Older daemons reject unsupported
requests rather than accepting work they cannot perform.
An already-running older daemon must be explicitly restarted to collect records;
normal attachment never replaces it.

For the usual XDG runtime, inspect notification health without selecting a
session:

```sh
bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" \
  '{"version":11,"op":"notifications","request":{"method":"health"}}'
```

The request methods are `health`, `inspect`, `subscribe`, `policy_get` and
`policy_set`. Inspection/subscription queries contain `scope`, `kinds` and
`limit` (1..64). Scope selectors are AND-combined `session_id`, `workspace_id`
and `pane_id`; empty `kinds` means all. A session endpoint always clamps reads
to its own persistent session ID and rejects a conflicting selector. Policy
writes require an explicit host control socket; workspace/pane policy selectors
also require a session ID. The most-specific matching policy overrides broader
rules; ambiguous equal-specificity rules are rejected. An empty policy kind list
disables that exact scope.

```sh
bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" \
  '{"version":11,"op":"notifications","request":{"method":"inspect","query":{"scope":{},"kinds":[],"limit":16},"after":null}}'
```

`inspect` without `after` returns the newest matching retained page, ordered by
sequence. `subscribe` without `after` establishes a current baseline without
replaying history. To resume, pass the returned cursor unchanged as `after`;
it contains `store_id`, `sequence` and `loss_generation`. Foreign/future cursors
are rejected. The cursor advances through filtered entries, and `gap` reports
retention or newly observed loss. Using the returned cursor acknowledges that
continuity boundary, not reading/dismissing a notification. `subscribe` adds
`wait_ms` (0..5000); new records wake it, and disconnect/attachment retirement
cancel the wait.

Remote reads use the normal session attachment and stay scoped to that session:

```sh
bkpr -r devbox -s work remote request \
  '{"op":"notifications","request":{"method":"subscribe","query":{"scope":{},"kinds":[],"limit":16},"after":null,"wait_ms":5000}}'
```

Local raw replies and remote request replies expose the typed result under
`result`. Records include structured source identities and semantic categories,
not terminal contents, prompts, arbitrary status messages or capture tokens.
Inspection/subscription is not a provider delivery receipt. Attached TUIs may
independently present best-effort OS alerts through confirmed OSC 99, with local
focus suppression and source activation. Unconfirmed terminals do not attempt
desktop presentation. New alerts include the Bkpr bee when the terminal
advertises icon support. See the [notification design](notifications.md) for bounds,
failure semantics, desktop presentation and the selected UnifiedPush publisher.

### Push destinations

Push management requires the host control socket, never a session socket. It
does not open an HTTP enrollment listener. Requests use the nested shape
`{"version":11,"op":"notifications","request":{"method":"push","request":{...}}}`.

| Inner method | Contract |
| --- | --- |
| `public_key` | Persistent host VAPID public key for distributor registration. |
| `enroll` | Subscription, network policy, source scope, kinds, optional device label and request ID; commits a pending generation and encrypted challenge. |
| `confirm` | Destination ID, generation and challenge nonce; activates only after proof. |
| `list` | Bounded secret-free destination/settings summaries, including pending and retired state. |
| `update` | Expected destination generation, scope, kinds and request ID; changes future delivery without replaying history. |
| `test` | Destination ID, generation and request ID; durably queues an explicit test. |
| `revoke` | Exact destination generation; removes subscription secrets and cancels undispatched work. |
| `diagnostics` | Bounded job states and redacted provider outcomes, not device delivery receipts. |

Read the public key:

```sh
bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" \
  '{"version":11,"op":"notifications","request":{"method":"push","request":{"method":"public_key"}}}'
```

Subscription endpoints/authentication and confirmation nonces are secrets. Send
their complete request JSON through stdin, not shell arguments/history:

```sh
bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" - < private-enrollment.json
# The same authenticated host operation over SSH:
ssh devbox 'bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" -' < private-enrollment.json
```

Keep that input file private, or stream directly from the receiving application.
Enrollment takes `subscription: {endpoint, p256dh, auth}` with uncompressed P-256
and 16-byte authentication keys encoded as unpadded base64url;
`policy: {allow_private_cidrs: []}` defaults to public HTTPS destinations.
Private providers require explicit permitted CIDRs and still require valid TLS.
The nonce is delivered only inside encrypted push, never in the enrollment reply.

Rotation supplies `replaces: {destination_id, generation}`. The existing active
subscription remains valid until the new generation is proven; expired
subscriptions can renew under the same logical identity. Generation checks
fence stale updates/completions. Empty push `kinds` mute automatic delivery;
explicit tests remain available, and re-enabling sends only future events.

Enrollment, filter updates and tests deduplicate identical request IDs/arguments
for seven days; their receipts expose `idempotency_expires_at_ms`. Do not replay
an ambiguous mutation after that horizon. Confirmation/revocation are
revision-idempotent. A queued receipt requires a successful durable commit.
Revocation cannot retract an already admitted network attempt.

Provider acceptance, retryable rejection, expiry and ambiguous outcomes are
distinct. Interrupted/ambiguous dispatch is not blindly replayed after restart.
HTTP success does not prove that a distributor delivered, or a phone displayed,
the message. A receiving mobile application and distributor registration remain
separate work.

## License

MIT; see [LICENSE](../LICENSE). Reused external source retains its applicable
license and provenance.
