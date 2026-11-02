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
and durable peer communication. Local Git/project snapshots are available; forge/CI
metadata and harness recovery selection are not implemented yet. Earlier release
notes describe the former implementation, not features available on this branch.

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
is needed. `just build` prepares the engine and builds both executables with
`cargo build --locked --workspace`. `just run --help` builds and runs `bkpr`.
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

Client registration keeps a dedicated lifetime connection. Takeover, disconnect
or owner-process exit revokes managed views and pending authority without
requiring another client to acknowledge. Credentials are ephemeral, process-bound
and never written to pane environments or session records. Ordinary pane execs
receive immutable `BKPR_SESSION_ID` and `BKPR_SESSION_NAME` snapshots. On Linux,
managed clients require kernel `SO_PEERPIDFD` support and fail before takeover
when that prerequisite is unavailable.

Client admission is bounded to 32 per daemon; views to 128 per daemon and 32 per
pane. Lifetime connections, pane waits, registry watchers and controls have
separate capacity, so admitted waiting views do not consume input/control slots.

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

Capture grants are private, launch-scoped and not persisted. Layout recovery
does not yet restore harness associations. Process recognition and foreground
directories are sampled off the input/render path; OSC summaries are pushed even
for panes with no active view. Linux managed capture requires kernel peer-PIDFD
support and fails closed when unavailable. macOS capture is not runtime-verified.

Wrappers depend on their PATH entry surviving shell/direnv changes; they do not
rewrite those environments to force interception. Explicit `bkpr run` remains
available through the matching executable. Native first-run trust, account and
terms prompts remain the user's responsibility; Bkpr does not approve them.

### Agent communication

Managed Claude, Codex, Omp and Antigravity launches expose the shared Bkpr MCP
tools. Claude, Codex and Omp use unique launch-local MCP server names so existing
registrations remain intact. Omp's extension handles native conversation binding
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

Discovery excludes self. `workspace` means backend execution membership, not a
viewer's layout or repository directory. Explicit `session` and `all` scopes are
limited to this daemon; `repository` compares canonical common Git directories
from current foreground CWD and fails closed when unknown. Remote discovery is
not implemented.

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
fold or unfold a workspace, and Enter activates the selected workspace or terminal.
Esc returns to the original application. The mouse supports activation, wheel
scrolling and double-click folding.

In the sidebar, `n` opens workspace creation, `r` renames the selected item and
`x`/Delete closes a selected terminal and its owned processes after confirmation.
Workspace cascading close and `N` for a separate terminal window await their
backend contracts; they are not substituted with layout removal or a split.

`/` opens the metadata finder, distinct from `Ctrl-b /` terminal-content search.
It supports fuzzy terms and `workspace:`, `harness:`, `status:`, `cwd:`, `project:`
and `branch:` filters, including quoted values. PR/CI/ticket filters explicitly
report unavailable facts. Narrow windows use Tab to cycle input, results and
details; PageUp/PageDown scroll the preview. Selection activates the actual pane
identity, not a row number.

Creation has name and directory fields; Tab changes fields and a blank directory
keeps the exact inherited directory. A workspace name is currently required.
Name fields support cursor movement, Home/End, Backspace/Delete, Ctrl-u and paste.
Enter submits, Esc cancels, and close confirmations also accept `y`/`n`. Failed
submissions retain their fields and diagnostics; an unknown outcome blocks blind
resubmission. Modal targets stay bound to their original identities through
background focus or registry changes.

Hovering or keyboard-selecting sidebar items opens passive, scrollable metadata.
It uses supplied registry facts and already-held snapshots, never hidden view
attachments or provider/filesystem work on the render path. Project data retains
loading/error/stale labels, and contradictory foreground identities hide retired
harness facts. Contextual hints use the same typed binding table as dispatch;
the command prefix expands the guide when space permits.

Reusable text surfaces, editing, window decoration, clipping, capability probes
and SVG preparation belong to `bkpr-tui-framework`; Bkpr content, action semantics
and artwork selection belong to `bkpr-client`. Confirmed Kitty graphics support
and observed colors enable static bee/hive artwork, prepared off-thread and sent
through the existing scene renderer. Other hosts retain readable `[]`/`<>`
markers, fold/status glyphs and the same interactions. Pixel recessed/raised
frames and decorative animation remain separate work; this is not general
terminal-graphics acceptance.

### Workspace and pane controls

Press `Ctrl-b`, then a command:

| Command | Action |
| --- | --- |
| `b` | Enter the all-workspace sidebar |
| `c` | Create workspace and first pane |
| `n` or `%` | Spawn a pane, split left/right |
| `"` | Spawn a pane, split top/bottom |
| `e` | Enter keyboard split-resize mode |
| `R` (uppercase) | Confirm a new configured shell in the focused unavailable pane's layout position |
| `r` / `p` | Rename workspace / pane |
| `w` / `o` | Choose workspace / show an existing pane |
| `i` | Inspect focused pane's local project/Git snapshot (arrows scroll) |
| `s` | Enter focused pane's scrollback and page upward |
| `y` | Copy the focused view's terminal selection to the host clipboard |
| `/` | Search the focused pane's terminal contents and retained history |
| `[` / `]` | Previous / next workspace |
| Arrows / Tab | Focus direction / next pane |
| Shift+Arrows | Exchange focused pane with nearest pane in that direction |
| `x` | Remove a layout leaf; keep its process running |
| `!` | Terminate a pane, after confirmation |
| `X` | Remove a workspace, after confirmation; keep its processes running |
| `d` | Detach client |
| `Ctrl-b` | Send literal Ctrl-b to the application (live mode only) |

`Ctrl-]` also detaches. Focus is client-local; showing a pane in another workspace
does not change its backend membership. Layout removal, client detach and process
termination are distinct.

`Ctrl-b Shift+Arrow` rearranges the selected workspace by exchanging two pane
positions; focus follows the same execution. It uses the same nearest-center
directional choice as focus navigation and does not wrap at an edge. Split axes,
ratios, other panes and backend workspace membership stay unchanged. Other
clients viewing that workspace see the saved arrangement, but keep their own
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

Replacement updates only the selected workspace, with a captured layout revision.
The old pane record and any projections in other workspaces remain unchanged.
Failure leaves the old layout intact; an ambiguous result is never automatically
retried. Changing selection while work is pending does not pull focus back.
Read-only clients cannot create or replace executions. Stopped records have no
restorable terminal scene; exited scenes are inspectable only while retained by
the same daemon. This is safe fresh execution, not process/harness resumption or
durable terminal-history recovery.
Compact sidebar `[S]` / `[E]` badges mean stopped / exited and preserve space for
the user-assigned pane title; headers and the chooser show the full state.

When a pane's root process exits normally, including shell `exit` or Ctrl-D,
the backend removes its references from every workspace layout and collapses
only the affected split paths. Explicit termination performs the same closure.
The pane's exit record and final in-memory scene remain available by ID; faults
remain visible for diagnosis. Completed, unprojected records are omitted from the
live sidebar; explicitly showing a retained scene still works. Closing one shell
never terminates neighboring panes.

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

### Requests and explicit shutdown

Requests use protocol version 2. Registry operations target workspaces/panes;
terminal/view operations are explicitly scoped to a pane:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" context --socket "$BKPR_RUN/control.sock"
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$BKPR_RUN/control.sock" \
  '{"version":2,"op":"inspect"}'
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$BKPR_RUN/control.sock" \
  '{"version":2,"op":"shutdown"}'
```

Check `ok` and `error` before treating an operation as successful. Shutdown closes
the listener even if cleanup/durability reports an error; an error is not a
successful persistence acknowledgement. Never target an unrelated live daemon.

Layouts and pane metadata survive restart, but PTYs and terminal contents do
not. Restored panes without a live runtime are Stopped; known exit codes remain.
The server never silently restarts their commands.

Retired metadata can be explicitly forgotten with
`{"version":2,"op":"pane_forget","pane_id":"RETURNED_PANE_ID"}` after the process
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
