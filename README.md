# Bkpr

Bkpr is being rebuilt as a pane-first native terminal multiplexer on
`feat/pane-first-mux`. This branch replaces the previous host runtime rather than
maintaining a compatibility track. `bkpr` is the command-line entrypoint and opens
the terminal client by default. A separate `bkprd` daemon owns independent PTYs,
terminal state and workspace layouts. Closing a client does not close its panes.
The terminal adapter uses the maintained `libghostty-rs` SDK over pinned
libghostty-vt; the [implementation plan](docs/pane-first-mux/implementation-plan.md)
describes the seven-crate boundaries.

This is development code, not a production release. Workspace layouts and pane
metadata survive daemon restarts; running processes and terminal contents do not.
Foreground harness recognition, managed native conversation capture and OSC 7501
status are available in the TUI, together with launch-scoped context, discovery
and durable peer communication. Local Git/project snapshots and explicit native
harness recovery are available; forge/CI metadata is not implemented yet. Earlier
release notes describe the former implementation, not this branch.

Current `main` and the native-terminal fork are useful starting material when a
domain comes into scope, not frozen specifications or compatibility obligations.
Reuse, adaptation and redesign are all available; the product and security
requirements determine the result. See the
[implementation plan](docs/pane-first-mux/implementation-plan.md),
[architecture assessment](docs/pane-first-mux/assessment.md) and
[engine evidence](docs/pane-first-mux/engine-proof.md) for scope, provenance and
remaining acceptance gates.

## Build

The supported path is Cargo with the pinned engine preparation helper. The Nix
flake provides a **native-only development shell**, not a packaged application:

```sh
nix develop
# Use this worktree's own cache, independent of main's build artifacts.
export CARGO_TARGET_DIR="$PWD/target"
just build
"$CARGO_TARGET_DIR/debug/bkpr" --help
```

There is only the default devshell. No separate sibling-runtime shell or build
is needed. `just build` validates and reuses the prepared engine, preparing it
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
local clipboard-offer patch and builds the pinned static engine. Initial
preparation can require network access, including Zig build dependencies. The
Cargo build checks engine provenance and prepared-file hashes; an arbitrary
system Ghostty installation is not a substitute.

## Native CLI

After building both executables, bare `bkpr` connects to or starts the local
named session `bkpr` and opens its terminal client. Each name owns one `bkprd`
and an independent persisted workspace/layout registry:

```sh
"$CARGO_TARGET_DIR/debug/bkpr"
"$CARGO_TARGET_DIR/debug/bkpr" --session work
"$CARGO_TARGET_DIR/debug/bkpr" -s work -d
"$CARGO_TARGET_DIR/debug/bkpr" list
"$CARGO_TARGET_DIR/debug/bkpr" stop -s work
```

Normal launches preserve your HOME, shell profiles and environment, using the
invocation directory. Closing the client leaves the daemon and panes running.
`-s` / `--session` selects an exact name; Unicode, spaces and slashes are
supported without treating names as filesystem paths. `-d` / `--detach-others`
revokes the selected session's other client attachments once, after the new
client is initialized. It does not close panes, terminate programs or change
layouts. Later normal attachments are not detached again.

`list` discovers running/stopped stored sessions without creating state or
starting a daemon. `stop` terminates only the selected session and its programs;
without `-s` it targets `bkpr`. Restart preserves the logical session identity
and layout records, but gets a new daemon identity and never replays old argv.

`bkpr context`, `bkpr request` and `bkpr keyboard` are headless and never start a
daemon. Inside a managed pane they default to its `BKPR_SOCKET`; explicit `-s`
overrides inherited routing. Normal TUI and `stop` commands default to `bkpr`,
not an inherited pane socket. Explicit `--socket` selects a fixture endpoint;
TUI attachment also requires its matching `--state-root`.

Starting a new daemon with `bkpr` supplies the current client's configured shell.
If loaded metadata has no running execution, it creates a fresh workspace named
`Main` (or `Main 2`, `Main 3`, …) and a usable shell without changing historical
layouts or replaying recorded commands. Starting `bkprd` directly without argv
after `--` is headless: it never creates a bootstrap process. Attaching to an
existing daemon never creates panes or resets layouts or another client's focus.
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

Named paths live under `$XDG_RUNTIME_DIR/bkpr/<name-hash>/s` and
`$XDG_STATE_HOME/bkpr/sessions/<name-hash>/`. Without these variables, runtime
uses a private `bkpr-<uid>/bkpr/` beneath the resolved system `/tmp`, and state
uses `$HOME/.local/state/bkpr/sessions/`. Resolving the system temporary-directory
alias does not permit symlinks in Bkpr-owned descendants.
Launch serialization, private filesystem checks and exact stored/live identities
prevent accidental attachment to another session. A live protocol or identity
failure never causes an automatic restart.

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

Client admission is bounded to 32 per daemon; views to 128 per daemon and 32 per
pane. Lifetime connections, pane waits, registry watchers and controls have
separate capacity, so admitted waiting views do not consume input/control slots.

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
[engine report](docs/pane-first-mux/engine-proof.md#native-pane-context).

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
Claude, Codex, Omp and Antigravity have managed conversation adapters:

| Harness | Native binding mechanism |
| --- | --- |
| Claude | Launch-local hooks capture the native session ID. |
| Codex | A private app-server transport binds the response to the TUI's own start, resume, fork or picker selection, before the first prompt. |
| Omp | A launch-local extension captures the native session manager ID and session transitions. |
| Antigravity | Its MCP request metadata supplies the root conversation ID. Registration alone is not binding or readiness. |

Antigravity requires explicit installation on the account where it runs:

```sh
bkpr install agy --dry-run
bkpr install agy
```

Installation uses Antigravity's own CLI, preserves unrelated settings and refuses
conflicting registrations. Normal launches never install it automatically.
The MCP adapter exposes authenticated context and peer communication. Native
receiver bootstrap requires Antigravity's own command approval; Bkpr never
approves it automatically.

OpenCode, Cursor Agent, Copilot, Aider, Cline and Goose are recognition-only.
`bkpr run cursor` selects `cursor-agent`, never the Cursor GUI. Native help,
version and noninteractive modes run without capture credentials. Modes that
cannot safely use an adapter retain native behavior rather than silently changing
their configuration: examples include Codex custom remote/profile/local-only
startup options and Claude file-based settings. Such launches can remain
recognition-only.

The sidebar distinguishes recognized, managed-unbound, bound and capture-error
states without replacing user pane names. Semantic status is a separate OSC 7501
fact: `.` idle, `~` working, `+` done, `?` blocked, `!` error. Ordinary programs
never acquire harness badges merely by setting a title. The pane chooser
(`Ctrl-b o`) shows the native conversation ID, foreground directory and capture
diagnostics. Backgrounding a job hides its foreground identity; resuming it can
restore the same live binding. Returning to the shell or replacing/exiting a
launch clears stale facts.

Capture grants are private, launch-scoped and not persisted. Only authenticated
native conversation IDs and their working directories become recovery metadata.
Process recognition and foreground directories are sampled off the input/render
path; OSC summaries are pushed even for panes with no active view. Linux managed
capture requires kernel peer-PIDFD support and fails closed when unavailable.
macOS capture is not runtime-verified.

Wrappers depend on their PATH entry surviving shell/direnv changes; they do not
rewrite those environments to force interception. Explicit `bkpr run` remains
available through the matching executable. Native first-run trust, account and
terms prompts remain the user's responsibility; Bkpr does not approve them.

### Explicit harness recovery

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
fallback. Claude/Omp use native `--resume`, Codex uses `resume`, and Antigravity
uses `--conversation`; their normal native approval requirements still apply.

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
uses shells. Native recovery is not process resurrection or a frontend picker.
See [native runtime evidence and remaining harness gates](docs/pane-first-mux/engine-proof.md#explicit-native-recovery).

### Agent communication

Managed Claude, Codex, Omp and Antigravity launches expose the shared Bkpr MCP
tools. Claude, Codex and Omp register it launch-locally as `bkpr`, without
changing persistent MCP configuration. Omp's extension handles native conversation binding
and wake notifications only; its launch-local `bkpr-control` skill describes the
available communication tools. No persistent Omp configuration is changed.

| Tool | Purpose |
| --- | --- |
| `whoami` | Exact launch/native conversation and current backend context |
| `set_title` | Discoverable agent label, separate from the pane name |
| `discover_agents` | Other live launches; current execution workspace by default |
| `send_message` | Durable untrusted peer text to an exact opaque agent ID |
| `read_inbox` | This recipient's unacknowledged messages, with pagination |
| `ack_message` | Explicit processed acknowledgement |
| `inspect_outbox` | Sender-only delivery/read/ack/expiry metadata, never bodies |
| `control` | Allowlisted native recovery listing/resume, with authenticated scope |

Discovery excludes self. `workspace` means backend execution membership, not a
viewer's layout or repository directory. Explicit `session` and `all` scopes are
limited to this daemon; `repository` compares canonical common Git directories
from current foreground CWD and fails closed when unknown. Remote discovery is
not implemented.

Recovery control accepts `{request: {op: "recovery_list"}}` or
`{request: {op: "pane_resume", pane_id, expected}}`, using the unchanged `harness`
descriptor from a fresh listing as `expected`. It defaults to the caller's live
execution workspace; explicit `session`/`all` cover this daemon only.
`repository` scope and arbitrary operations/commands are not supported by
`control`.

Sends require a sender-scoped `idempotency_key`. Retrying the same recipient,
text and scope returns its original receipt; changing any of them conflicts.
Durable send acceptance, native notification acceptance, inbox read and processed
acknowledgement are separate facts. Treat peer text as untrusted context, never
as user instructions. Do not acknowledge merely because a notification arrived.

Omp offers native step-boundary asides, Claude uses tool-boundary context and idle
`asyncRewake`, Codex queues for a future turn on its exact selected thread, and
Antigravity uses native `SendAgentMessage`. None types peer messages into a PTY.
Registration and native binding do not establish receiver readiness: an actual
subscription is required. An API that cannot confirm acceptance produces an
`unknown` receipt rather than a synthetic success; it is not blindly replayed.

Agent receipts and unacknowledged bodies are stored privately below the daemon's
state root. Unacknowledged bodies are retained for seven days after the first
persisted recipient-retirement observation, then purged while receipt metadata
remains. Acknowledgement removes bodies earlier. Authentication tokens and native
receiver credentials are never persisted. A daemon restart retires old launch
authority; a fresh launch does not inherit an exited launch's inbox.

Durable-store failure suspends delivery and all commit callbacks until restart.
An ambiguous mutation is reported as such, not silently retried. Notifications
carry opaque inbox pointers; model processing is established only by explicit
read/processed-ack facts. Ordinary shell and terminal input remain independent of
waiting receivers.

### Local projects and Git

Pane headers and sidebar annotations show the foreground directory's project,
worktree and local branch without changing pane names or workspace membership.
Press `Ctrl-b i` for the focused pane's project inspector; arrows scroll through
wrapped details, and Enter or Esc closes it and returns focus. This also works
on narrow terminals without a sidebar.

The inspector shows the observed CWD, worktree root, common/worktree Git
directories, local HEAD, staged and unstaged file/line/binary totals and untracked
file count. Committed totals compare HEAD directly with a suitable local merge
base, not a sum of commits. An unavailable base or total is **unknown**, not zero;
detached and unborn HEADs remain distinct. This is local Git data, not a PR diff,
remote branch check or CI result. Bkpr does not fetch merely to display it.

A preflight rejects tracked paths selecting configured executable Git `clean` or
`process` filters with an explicit statistics error, rather than silently
changing their content semantics. This also affects clean working trees because
determining dirtiness may invoke a filter. Previously known values remain labeled
stale. Fsmonitor, external diff and textconv are disabled for metadata commands.
This is not an adversarial repository sandbox: configuration and attributes can
change concurrently with the preflight and Git commands.

The backend publishes cached snapshots outside input/render work. Loading,
non-repository and error states are explicit; any retained repository values
during loading/error are labeled stale. These live snapshots are not restored as
current facts after daemon restart. Authenticated `whoami` and detailed agent
discovery include the same backend project snapshot when it matches that launch's
live foreground CWD; compact discovery omits it. Repository-scoped discovery and
messaging require a ready canonical common-Git-directory identity.



### Sidebar, popups and presentation

`Ctrl-Space` (or `Ctrl-b b`) enters the all-workspace sidebar. Selection is local:
browsing does not change the active application or attach hidden terminal views.
Arrows or `j`/`k` move, Home/End and PageUp/PageDown reveal other rows, Left/Right
fold or unfold a workspace or window, and Enter activates the selected workspace,
window or terminal. Esc returns to the original application. The mouse supports
activation, wheel scrolling and double-click folding.
The terminal cursor stays hidden but is positioned on the highlighted item,
including after arrow navigation. Passive metadata cards do not move that
logical focus position; native applications and focused forms regain their own
cursor on return. This also keeps host cursor trails anchored to actual focus.

In the sidebar, `n` creates a workspace and its first terminal, `N` creates a
separate full terminal window, and `r` renames the selected item. `x`/Delete closes
the selected workspace, window or terminal after confirmation, terminating all
its owned processes, including hidden terminals. A separate window is not a split.

`/` opens the metadata finder, distinct from `Ctrl-b /` terminal-content search.
It supports fuzzy terms and `workspace:`, `window:`, `harness:`, `status:`, `cwd:`,
`project:` and `branch:` filters, including quoted values. PR/CI/ticket filters explicitly
report unavailable facts. Narrow windows use Tab to cycle input, results and
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
This preference does not disable application-owned graphics or alter pane sizes.
With confirmed graphics/color support and ready pane contours, the framework
temporarily makes the host's default background match the sidebar matte while
rendering native pane defaults in their original normal background. Logical
color observations remain separate from that physical override. Fallback and
detach restore the exact original host background.
Ready graphical frame cells also use that normal pane background beneath the
atlas. Its opaque outer mask supplies the sidebar matte, while the opened parts
of horizontal border cells retain the pane color instead of exposing a dark strip.

Supporting hosts notify live theme changes through mode 2031; Bkpr re-queries
colors without polling and restores the original confirmed notification setting
on detach. Missing replies or unconfirmed truecolor retain native default colors,
ANSI accents, inverse selection and underlined hover instead of a guessed dark
theme. Terminal names and RGB color replies alone do not establish truecolor.

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
Decorative animation and a command palette are outside the current main-parity pass.
Forge/PR/CI card data and refresh still need a backend-owned contract; the
frontend does not query providers or invent those facts.

### Workspace, window and pane controls

Press `Ctrl-b`, then a command:

| Command | Action |
| --- | --- |
| `b` | Enter the all-workspace sidebar |
| `c` | Create workspace, window and first pane |
| `N` (uppercase) | Create a separate full terminal window in the active workspace |
| `n` or `%` | Spawn a pane in the active window, split left/right |
| `"` | Spawn a pane in the active window, split top/bottom |
| `e` | Enter keyboard split-resize mode |
| `R` (uppercase) | Confirm a new configured shell in the focused unavailable pane's layout position |
| `r` / `p` | Rename workspace / pane |
| `w` / `o` | Choose workspace / show an existing pane |
| `W` (uppercase) | Choose a window in the active workspace |
| `i` | Inspect focused pane's local project/Git snapshot (arrows scroll) |
| `s` | Enter focused pane's scrollback and page upward |
| `y` | Copy the focused view's terminal selection to the host clipboard |
| `/` | Search the focused pane's terminal contents and retained history |
| `[` / `]` | Previous / next workspace |
| Arrows / Tab | Focus direction / next pane |
| Shift+Arrows | Exchange focused pane with nearest pane in that direction |
| `x` | Remove a layout leaf; keep its process running |
| `!` | Terminate a pane, after confirmation |
| `X` | Close the workspace and all its owned terminals, after confirmation |
| `d` | Detach client |
| `Ctrl-b` | Send literal Ctrl-b to the application (live mode only) |

`Ctrl-]` also detaches. Active workspace/window and per-window pane focus are
client-local. Choosing an existing owned pane switches to its owning workspace
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
to another. Pending controls and projection-capacity failures are shown explicitly.
Pane-local transport/input failures stop the affected lane rather than closing
healthy panes. Ambiguous operations are reported and never automatically replayed.
Pointer gestures remain bound to their original pane/view, including clipped
cross-pane drags and cancellation when that projection is removed.

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

`--clipboard ask|allow|deny` defaults to `ask`. Keep `ask` or `deny` unless the
child is trusted. Consent and device responses remain bound to the originating
pane/view even if local focus changes.

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
`Shift+PageDown` enters history and pages toward live output. In history:

| Key | Action |
| --- | --- |
| Up / Down | Older / newer by one row |
| PageUp / PageDown | Older / newer by one visible page, with one row of overlap |
| Home | Oldest retained history |
| Esc / Enter / End | Return to current live output and leave history |

The pane header and focused status line show `HISTORY` and the authoritative
row offset from live output, followed by native retained rows (including the
visible grid). The
application cursor is hidden while inspecting history, including explicit mode
at offset zero. Ordinary typing, literal Ctrl-b forwarding and paste do not
reach the application in this mode; return live explicitly before typing.
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
connection such as SSH, subject to the host's permissions. A **sent** status is
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
If the view is still in history, End returns live before ordinary typing resumes.
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
capped; resource optimization remains tracked in `TODO.md`.

### Workspace and session ownership

A named session owns its workspaces. Each workspace owns independent windows,
their layouts and their terminals; a layout cannot borrow a terminal from another
window or workspace. Clients choose active windows independently.

Closing a window terminates all its owned terminals, including terminals absent
from its displayed layout. Closing the last terminal, including normal shell
exit, closes its workspace. Closing the last workspace closes the session.
Detaching a client or removing a layout projection does not terminate a program.
An intentionally empty headless server stays available until explicitly stopped.

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
  "version": 6,
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

Native client/server connections use binary Planus wire family 6. Registry
operations target workspaces, their independent windows and owned panes;
terminal/view operations are pane-scoped. Active-window selection belongs to
each client. The `request` utility still accepts and prints JSON; that is its
human-facing boundary, not the socket format.

Use a matching launcher/server pair. Existing family-4/5 sessions stay running
and remain usable with their matching old client; a family-6 client refuses
attachment or takeover rather than restarting or replacing them. Retain the
old binaries for those sessions, or explicitly stop the intended session before
starting a fresh one. Recovery records keep their existing private schema.

Client registration does not select an SSH agent. The attaching client captures
its original `SSH_AUTH_SOCK` locally, resolving relative paths against its
invocation directory and preserving non-UTF-8 Unix paths. Its one-shot authenticated
activation publishes that source, or explicit absence when unset/empty, together
with takeover. Repeated activation never republishes the source or repeats takeover.
Source paths are ephemeral, redacted from request debug output and never session metadata.

```sh
"$CARGO_TARGET_DIR/debug/bkpr" context --socket "$BKPR_RUN/control.sock"
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$BKPR_RUN/control.sock" \
  '{"version":6,"op":"inspect"}'
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$BKPR_RUN/control.sock" \
  '{"version":6,"op":"shutdown"}'
```

Check `ok` and `error` before treating an operation as successful. Shutdown closes
the listener even if cleanup/durability reports an error; an error is not a
successful persistence acknowledgement. Never target an unrelated live daemon.

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
`{"version":6,"op":"pane_forget","pane_id":"RETURNED_PANE_ID"}` after the process
has stopped, every layout reference has been removed and all its views detached.
This releases record capacity; it is not termination or automatic history pruning.

The authoritative request models are in
[`bkpr-protocol`](crates/bkpr-protocol/src/registry.rs), with
[pane/view requests](crates/bkpr-protocol/src/lib.rs). These are development
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
rendering acceptance. In particular, the known Kitty graphics initial
presentation/compositor gates remain unresolved; parser acknowledgements and
scene geometry alone do not prove that images were presented. Graphics,
clipboard, Sixel and second-projection acceptance must follow the evidence gates
in the native plan rather than be inferred from a successful build.

## Packaging and releases

There are intentionally **no Nix package or app outputs** on this branch, so
`nix build .#bkpr` and `nix run` are not supported distribution paths. The old
runtime's packaging, cache promotion and publication workflows have been
removed rather than pointed at a placeholder package.

Restoring native packaging requires a sandbox-compatible derivation for the
existing pinned Ghostty source, clipboard patch and Zig toolchain, plus all Zig
build dependencies supplied offline with reproducible hashes. It also requires
native executable/closure checks and the product's runtime acceptance gates.
The current preparation helper is a supported developer prerequisite, not a
network fallback inside a Nix build. No new engine pin or vendoring design is
introduced by this cutover.

Conventional Commits and the existing pre-1.0 policy remain: features and
breaking changes advance the `0.x` minor version; fixes advance patch versions;
maintenance alone is not releasable unless an explicit patch is requested.
Leaving `0.x` requires a deliberate policy change. The retained release analyzer
and notes configuration are for planning only; automated tagging/publication is
not enabled. Optional policy checks use Node.js and the locked npm dependencies:

```sh
npm ci
npm run test:release-policy
```

`npm run release:preview` performs a dry-run plan on the configured release
branch (`main`); it is not a feature-branch build, tag, package or release.
Historical versions remain documented in [CHANGELOG.md](CHANGELOG.md).

## License

MIT; see [LICENSE](LICENSE). Reused external source retains its applicable
license and provenance.
