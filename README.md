# Bkpr

Bkpr is being rebuilt as a pane-first native terminal multiplexer on
`feat/pane-first-mux`. This branch replaces the previous host runtime rather than
maintaining a compatibility track. The executable is `bkpr`, built from the
`bkpr-mux` Cargo package; `bkpr-terminal` adapts the pinned libghostty-vt engine.
A daemon owns a real PTY and terminal state independently of attached TUI views.

This is development code, not a production release. The native CLI below
is the implemented surface. Workspace/project UI, agent and harness integration,
broker messaging, Git/CI presentation and recovery are not currently exposed by
this executable. Earlier release notes describe the former implementation, not
features available on this branch.

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
is needed. `just build` prepares the engine and builds
`cargo build --locked -p bkpr-mux --bin bkpr`. `just run --help` builds and runs
that same executable. All recipes respect an existing `CARGO_TARGET_DIR`.

**Shared-artifact safety:** a normal build writes `debug/bkpr` in the selected
Cargo target directory. If that pathname is a currently used application, do not
run `just build`, `just run` or the equivalent Cargo build against it until you
have arranged artifact isolation. Do not replace an installed or live program
as part of trying this branch.

Without Nix, provide the pinned Rust toolchain from `rust-toolchain.toml`, Cargo,
a C compiler, libclang for bindgen, Bash, curl, patch, tar, xz and the usual Unix
core/find utilities. `just` is optional when using the direct commands below.
The preparation helper supports x86-64 Linux, ARM64 Linux and ARM64 macOS;
this is a prerequisite-platform list, not a claim of runtime acceptance on every
platform.

```sh
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
mkdir -p "$CARGO_TARGET_DIR"
bash scripts/prepare-ghostty-vt.sh
export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
cargo build --locked -p bkpr-mux --bin bkpr
```

The helper downloads hash-checked Ghostty and Zig archives, applies the checked
local clipboard-offer patch and builds the pinned static engine. Initial
preparation can require network access, including Zig build dependencies. The
Cargo build checks engine provenance and prepared-file hashes; an arbitrary
system Ghostty installation is not a substitute.

## Native CLI

Use a dedicated private runtime directory and an isolated, absolute home path.
The daemon runs in the foreground and defaults to `/bin/sh -i` if no command is
provided. It does not attach to an existing Bkpr session.

In the first terminal, after building:

```sh
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
umask 077
export BKPR_RUN="$(mktemp -d "${TMPDIR:-/tmp}/bkpr.XXXXXX")"
mkdir "$BKPR_RUN/home"
printf 'Runtime directory: %s\n' "$BKPR_RUN"
"$CARGO_TARGET_DIR/debug/bkpr" daemon \
  --socket "$BKPR_RUN/control.sock" \
  --home "$BKPR_RUN/home" \
  -- /bin/sh -i
```

The socket parent must be owned by the current user and private (mode `0700`).
The socket is private (`0600`); an existing socket is never replaced. Use a short
path to stay within Unix-domain socket path limits. An isolated home avoids
loading your normal shell configuration, but is not a filesystem or OS sandbox
for the child command.

In another terminal, set `BKPR_RUN` to the directory printed above and use the
same Cargo target directory:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" tui --socket "$BKPR_RUN/control.sock"
```

`Ctrl-]` detaches the TUI without terminating the daemon's shell. Additional
views can attach to the same daemon. A read-only view does not request input or
clipboard authority:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" tui \
  --socket "$BKPR_RUN/control.sock" --read-only
```

The TUI accepts `--clipboard ask|allow|deny` (default `ask`). Keep `ask` or `deny`
unless the child command is trusted to interact with the host clipboard.

Daemon geometry options are `--cols`, `--rows`, `--cell-width` and
`--cell-height`; defaults are `80`, `24`, `8` and `16`, respectively. Interactive
views can update terminal geometry through the native size-lease mechanism;
input and clipboard permissions are tracked separately.

The TUI receives scene changes independently of keyboard input. Typing repaints
changed rows without clearing the viewport or redrawing unchanged chrome; cursor
updates don't repaint stationary text. Unchanged image resources and placements
are retained. Initial attachment, resize and scene-baseline recovery still use
complete state, and palette changes invalidate affected presentation.

Scene polling supports bounded change waits and optional whole-row deltas.
Waiting views don't occupy the connection capacity reserved for input and
control operations. These mechanisms reduce repeated transfers and repaint work;
they don't lower terminal fidelity or skip input.

### Requests and explicit shutdown

`request` sends one JSON operation and prints the versioned JSON response:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" request \
  --socket "$BKPR_RUN/control.sock" '{"op":"inspect"}'
"$CARGO_TARGET_DIR/debug/bkpr" request \
  --socket "$BKPR_RUN/control.sock" '{"op":"shutdown"}'
```

Inspect the response's `ok` field and `error` before treating an operation as
successful. After shutdown has completed and all views have detached, the
private runtime directory can be removed. Do not use these commands against an
unrelated daemon or a live installed application.

The current request surface covers view attach/update/detach, scene polling,
size pinning, input/paste/key/mouse/focus/scroll, clipboard responses, colors,
termination and shutdown. These are development interfaces, not a compatibility
promise. The authoritative request shapes are in
[`crates/bkpr-mux/src/lib.rs`](crates/bkpr-mux/src/lib.rs) and view permission
fields in [`authority.rs`](crates/bkpr-mux/src/authority.rs).

For an already attached view, the keyboard client routes a physical byte array
through the same ordered input decoder and native encoder as the TUI:

```text
bkpr keyboard --socket PATH --view VIEW_ID '[PHYSICAL_INPUT_BYTES]'
```

Use the daemon's returned opaque view ID. Acceptance means queued input, not
proof that the child has processed it.

## Tests

Prepare the pinned engine, then run the native workspace tests:

```sh
just test
```

Or, with `CARGO_TARGET_DIR` and `BKPR_GHOSTTY_VT_DIR` set as above:

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
