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
status are available in the TUI. Broker messaging, Git/CI presentation and harness
recovery selection are not implemented yet. Earlier release notes describe the
former implementation, not features available on this branch.

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
# Keep an existing shared CARGO_TARGET_DIR; otherwise use this checkout's target.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
just build
"$CARGO_TARGET_DIR/debug/bkpr" --help
```

There is only the default devshell. No separate sibling-runtime shell or build
is needed. `just build` prepares the engine and builds both executables with
`cargo build --locked --workspace`. `just run --help` builds and runs `bkpr`.
All recipes respect an existing `CARGO_TARGET_DIR`.

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
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
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

After building both executables, run `bkpr` to connect to or start the local
`bkprd` and open the terminal client:

```sh
"$CARGO_TARGET_DIR/debug/bkpr"
```

Normal launches preserve your HOME, shell profiles and environment, using the
invocation directory. Closing the client leaves the daemon and panes running.
`bkpr context` and `bkpr request` are headless and do not start a daemon. Inside a
managed pane they default to its `BKPR_SOCKET`; an explicit `--socket` overrides it.

For an isolated preview, start the daemon explicitly in one terminal:

```sh
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
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
"$CARGO_TARGET_DIR/debug/bkpr" tui --socket "$BKPR_RUN/control.sock"
```

Socket/state directories must be private and current-user-owned. The socket is
mode `0600`; a live listener is never replaced, and one daemon exclusively owns
each state root. Keep Unix socket paths short. Explicit `--home` selects a
scrubbed fixture environment, not an OS/filesystem sandbox. `--bkprd PATH`
selects the server executable; `bkprd --bkpr PATH` selects the matching utility
and harness wrappers when testing independently staged binaries.
View admission is bounded to 128 per daemon and 32 per pane. Pane waits,
registry watchers and controls have separate capacity, so admitted waiting views
do not consume the slots reserved for input or management.

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
The MCP adapter currently exposes capture/context, not peer communication.

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


### Workspace and pane controls

Press `Ctrl-b`, then a command:

| Command | Action |
| --- | --- |
| `c` | Create workspace and first pane |
| `n` or `%` | Spawn a pane, split left/right |
| `"` | Spawn a pane, split top/bottom |
| `r` / `p` | Rename workspace / pane |
| `w` / `o` | Choose workspace / show an existing pane |
| `[` / `]` | Previous / next workspace |
| Arrows / Tab | Focus direction / next pane |
| `x` | Remove a layout leaf; keep its process running |
| `!` | Terminate a pane, after confirmation |
| `X` | Remove a workspace, after confirmation; keep its processes running |
| `d` | Detach client |
| `Ctrl-b` | Send literal Ctrl-b to the application |

`Ctrl-]` also detaches. Click a sidebar workspace or pane to focus it.
Forms use Enter to apply, Esc to cancel and Ctrl-u to clear. Focus is
client-local; showing a pane in another workspace does not change its backend
membership. Layout removal, client detach and process termination are distinct.

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

`--read-only` denies application input and clipboard access without making the
visible client ineligible for sizing. The latest eligible visible interactive
view owns pane dimensions unless pinned. Daemon initial geometry options are
`--cols`, `--rows`, `--cell-width` and `--cell-height` (defaults 100, 30, 8, 16).

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
