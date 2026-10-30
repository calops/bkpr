# Initial pane-first engine experiment

## Decision

**Initial native evidence at `e9ee402`, updated for the native-only branch—not production acceptance.** The initial experiment ran independently of Zellij. This branch now removes the old runtime and makes the native application the only path; no old regression suite is required. Retained executable snapshots identify historical observations; current native verification is recorded below. Current usage is in [README](../../README.md), and the [implementation plan](implementation-plan.md) governs continued development.

The blocking observation is initial visibility of a four-row RGBA Kitty image below text. Both physical consumers remain blank after five seconds despite a valid normalized scene and host parsing. A read-only debugger trace on both fresh owned Kitty 0.49.2 processes establishes the cause: the host selects non-layered rendering from zero image counts, then rebuilds four negative placements without updating that decision or scheduling another render. A host-only empty `clear_selection` causes a layered draw with the same four placements and makes both images visible. This is a host compositor defect in the exercised fixture, not a graphics acceptance pass. Image/text reorder, synchronized-output wrapping, resource-ACK gating and tracing/environment hooks were removed rather than retained as redraw, sleep or layer workarounds.

Review also found two independent renderer defects: ordering within `BelowBackground`, and exact subcell/cropped/scaled destination geometry. Both repairs now have independent terminal-consumer and allocation-bound regressions, plus composed-scene physical geometry evidence. They don't fix or waive the initial-presentation gate.

## Implemented boundary

[`bkpr-terminal`](../../crates/bkpr-terminal/src/lib.rs) owns an experimental Rust adapter around official libghostty-vt. The native engine parses child output, owns terminal state and encodes keys, mouse, focus and paste. Bkpr extracts typed cells, colors, cursor, modes, hyperlinks, resolved image resources and placements into its [scene model](../../crates/bkpr-terminal/src/model.rs). It exposes OSC 7501 status as semantic events, not title heuristics. Held native render resources don't escape into clients. Clipboard effects cross an explicit policy boundary; child escape strings aren't replayed as trusted host commands.

[`bkpr-mux`](../../crates/bkpr-mux/src/lib.rs) starts an actual `portable-pty` child behind a separate daemon. It supplies an isolated HOME/XDG environment and owns the PTY independently of attached views. The [TUI projection](../../crates/bkpr-mux/src/render.rs) attaches over IPC, renders normalized scenes and detaches with Ctrl-]. Closing the last view doesn't terminate the child. Explicit termination and shutdown are separate operations. The bounded writer queue and ordered physical-input decoder distinguish queue acceptance from child processing; unknown input delivery isn't blindly replayed.

The [authority model](../../crates/bkpr-mux/src/authority.rs) keeps view identity, visibility, interactivity, input permission and clipboard permission separate. The latest eligible visible interactive view controls PTY size unless an eligible view pins it. A different authorized view can supply clipboard access. Read-only input is denied; hidden views aren't size owners. Each view has independent scrollback. Opaque view IDs expire after ten seconds without activity; clipboard requests time out after five seconds. Stale grants and detached views fail closed.

The Linux daemon is a **one-pane, process-isolated subreaper**. Its [process cleanup](../../crates/bkpr-mux/src/process_tree.rs) is not a generic multi-pane process registry or a safe cleanup primitive for an arbitrary shared application process. The current library shape doesn't establish multi-pane production lifetime semantics.

### IPC and resources

The [private Unix-socket transport](../../crates/bkpr-mux/src/transport.rs) uses protocol-v1 JSON responses and rejects incompatible response versions. It requires a current-user-owned private parent directory, creates a mode-0600 socket and never replaces an existing socket. Requests and responses have aggregate size limits; connection count and I/O time are bounded. These are local same-user experiment boundaries, not authenticated remote endpoint enrollment or production network framing.

Clients poll bounded full snapshots and semantic events. They report retained image receipts keyed by image ID **and generation**, so immutable image bytes can be omitted while placements and scene state still change. Generation-aware caches prevent reused IDs from referring to stale pixels. This isn't a production scene-delta protocol, persistent pane state or restart recovery. Polling, JSON/base64 resource transfer and whole-scene repaint costs still need production evaluation.

### Experiment limits

| Boundary | Bound |
| --- | --- |
| One input, paste or supplied clipboard payload | 1 MiB of data; larger payloads are rejected before dispatch |
| PTY writer | 2 MiB user budget, 4 MiB total queue |
| Local IPC | 8 MiB request, 96 MiB response, 32 concurrent connections |
| Canonical image resources accepted by the TUI | 8192 pixels per dimension, 64 MiB aggregate |
| TUI projection working set | 128 MiB preflight budget for canonical images, visible derived tiles and planning metadata |

Derived images sample only visible pixels, retaining the full placement's sampling phase. Uploads are tiled at 10,000 pixels per dimension; offscreen target extents don't cause full-size raster allocation. These are experiment bounds, not final product limits.

## Native provenance and dependency ownership

The reproducible prerequisite is [`scripts/prepare-ghostty-vt.sh`](../../scripts/prepare-ghostty-vt.sh):

- Ghostty source: [`93967b7f9478e94dcbb42125c8a90f34feb40d75`](https://github.com/ghostty-org/ghostty/tree/93967b7f9478e94dcbb42125c8a90f34feb40d75).
- Source archive SHA-256: `af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70`.
- Zig: `0.17.0`, with platform archive hashes in the script.
- Build profile: `ReleaseSafe-simd-libc-baseline-offer-v1`, using `-Demit-lib-vt=true -Doptimize=ReleaseSafe -Dsimd=true -Dcpu=baseline`.
- Exact-source [clipboard patch](../../crates/bkpr-terminal/native/clipboard-offer.patch) SHA-256: `196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c`.
- Exercised native prefix: `/home/calops/projects/bkpr/target/ghostty-vt-93967b7`. Its provenance stamps and `bkpr-files.sha256` bind the headers and static library to the source/profile/patch.

The supported SIMD build links libc. The tested `-Dsimd=false` build brought weak allocator stubs that preempted Rust's libc allocation calls when statically linked; it isn't the recipe used here. The script restores archive-owned source/toolchain files and applies the exact patch with zero fuzz. [`build.rs`](../../crates/bkpr-terminal/build.rs) consumes the prepared prefix and generates experimental bindings.

The patch exposes an optional paste `offer_id` and a read's `paste_offer_id` after native one-time-password validation. A boolean saying that a read was granted isn't enough: Bkpr must bind the read to the exact physical paste offer and its originating live view, not whichever clipboard contents happen to exist later. The retained SSH PNG fixture changes the host clipboard before the child reads, yet returns the original offered image.

This isn't a commitment to maintain a second generic Ghostty SDK. Prefer the maintained upstream safe/sys pair from [`Uzaaft/libghostty-rs`](https://github.com/Uzaaft/libghostty-rs), with compatible Rust and native source pins. Published `0.2.2` lacks some required wrappers; Git `9ede35b8ea94a55ed3608408d67e102883129f1e` already wraps clipboard read/write, OSC 7501, render holds and resolved Kitty images. Its native pin `521cf2c` is older than this experiment's `93967b7`; mixing the two ABIs isn't valid. The concrete remaining upstream contribution is validated paste-offer provenance. Keep Bkpr's scenes, quotas, authority and status policies; replace redundant generic FFI wrappers before production adoption. The C API remains unstable.

## Build and try it

The commands below use the current native executable name. They do not install harness adapters, edit profiles or make provider requests. Use the repository devshell for Rust, Clang/libclang, C build tools and download/unpack tools. Native preparation downloads pinned Ghostty and Zig archives into the shared target directory. If the shared target contains a running older `bkpr` executable, preserve it or isolate public artifact output before building.

```sh
export CARGO_TARGET_DIR=/home/calops/projects/bkpr/target
nix develop
scripts/prepare-ghostty-vt.sh
export BKPR_GHOSTTY_VT_DIR="$(scripts/prepare-ghostty-vt.sh --print-prefix)"
cargo build --locked -p bkpr-mux

# Terminal 1: choose a fresh private experiment directory.
proof_dir=$(mktemp -d /tmp/bkpr-engine.XXXXXX)
chmod 700 "$proof_dir"
printf 'Experiment directory: %s\n' "$proof_dir"
"$CARGO_TARGET_DIR/debug/bkpr" daemon \
  --socket "$proof_dir/mux.sock" --home "$proof_dir/home" -- /bin/sh -i
```

In another terminal, set `proof_dir` to the printed directory and attach:

```sh
export CARGO_TARGET_DIR=/home/calops/projects/bkpr/target
read -r -p 'Experiment directory printed by terminal 1: ' proof_dir
"$CARGO_TARGET_DIR/debug/bkpr" tui --socket "$proof_dir/mux.sock"
```

A second terminal can run the same command, or add `--read-only`. Clipboard policy defaults to `ask`; `--clipboard allow|deny` selects an explicit experiment policy. Ctrl-] detaches without killing the shell. Stop the owned daemon and child explicitly when finished:

```sh
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$proof_dir/mux.sock" '{"op":"inspect"}'
"$CARGO_TARGET_DIR/debug/bkpr" request --socket "$proof_dir/mux.sock" '{"op":"shutdown"}'
```

The CLI's `--help` describes typed attach/update/poll/pin/input/clipboard operations. Don't attach this proof to existing sessions or substitute a real-provider harness run for the isolated fixture.

## Retained observations

All paths below are under `/home/calops/projects/bkpr/target/smoke/pane-first-engine/`. These ignored artifacts are local evidence, not distributed test fixtures.

Historical baseline: `proof-clean`, SHA-256 `58ad798bb57ce4b2f8fd2a7c70ddf7b4b2790cb6739912b8bca418954567abda`. The SSH, consent, Omp and native compositor-trace runs use this snapshot.

Final integrated snapshot: `proof-final-geometry-1`, SHA-256 `355abc760e193fb7935449cb429c8c49aa8cf15af0188673b5515bab6f5f824a`. Fresh API, clipboard-boundary and composed-geometry runs use this executable. Controller sources are retained in `controllers-final.zip`; `final-checks-summary.json` records the exercised integration commands and their scope.

| Evidence | Observed result |
| --- | --- |
| `final-geometry-api-1/observations.json`, `requests.jsonl`, `fixture.jsonl`; earlier `final-clean-api-1/` | Latest-owner sizing, explicit pin, hidden-owner exclusion, read-only input denial, two-view convergence, independent scrollback, OSC 7501, exact text/binary/PNG MIME reads and writes, stale grant denial, detach survival and explicit child termination pass on both snapshots. `sixel:false` is explicit. |
| `final-clean-ssh-png-1/result.json`, `ssh-fixture.jsonl`, `ssh-clipboard.png` | Actual owned SSH terminal path carries child OSC 52 to local Kitty clipboard and physical paste back into the mux PTY. Default Kitty policy is retained. A 156-byte PNG paste returns the original offer after clipboard replacement. Original SHA-256 is `9371358677781283024f2996c1657bf355fbe0671829716320ef0a5b6a48f746`; replacement is `fd9a2174d36f72df308a805f78401d3a2fe053fc59da37717b436dad26b0d744`. This proves terminal clipboard transport, not new production remote-endpoint framing. |
| `final-clean-consent-1/result.json`, `kitty.vt`, `clipboard-after-y.bin`, `clipboard-after-n.bin` | Physical allow writes exact bytes `00 ff 01 fe 0d 0a 80`; deny returns permission failure and leaves those bytes unchanged. The writer keeps Kitty defaults; a separately authorized read-only fixture verifies bytes. Closing the physical window detaches all views while the child survives; explicit termination reaps it. |
| `final-clean-omp-1/result.json`, `native-pane.json`, `actual-omp-tui.png`, `provider-request-1.json`, `provider-response.txt` | Real Omp `18.13.2` TUI renders a response and native OSC 7501 idle/working/done events. One fake-provider request runs in a distinct user/network namespace with loopback only and no default route; zero external provider calls. This is not full harness/broker integration parity. |
| `final-clean-full-1/observations.json`, `requests.jsonl`, `fixture.jsonl`, `kitty-1.vt`, `kitty-2.vt`, `failure.png` | Real shell job control, PTY, physical input, WINCH, pointer input, normal RGB image visibility and deletion pass before the four-row below-text image visibility timeout. The run fails overall. |
| `final-clean-independent-1/observations.json`, `final-clean-independent-caches-1/observations.json` | Separate X servers, then separate HOME/cache directories, still reproduce the initial blank image. Shared-X state alone doesn't explain it. |
| `graphics-last-frame-1/result.json`, `graphics-full-replay-4/result.json`, `graphics-full-resize-replay-1/result.json`, `graphics-presented-old-text-1/result.json` | Exact VT replay controls render successfully; these controls don't erase the live failure. |
| `physical-host-selection-diagnostic-2/observations.json`, `physical-refresh-diagnostic-1/observations.json` | Host-only selection clear and no-op view refresh recover visibility without a new child image. Neither is retained as a fix. |
| `final-kitty-gl-renderer-1/result.json`, `native-kitty-context.json` | Exercised host is Kitty `0.49.2`, Mesa `26.2.5` llvmpipe, LLVM `21.1.8`, 256 bits. This is environment evidence, not blame attribution. |
| `native-layer-trace/result.json`, `events-1535579.jsonl`, `events-1535576.jsonl`, `run-2/trace-initial-before-host-clear.png`, `run-2/trace-after-host-clear.png` | Actual installed-binary trace reproduces the failure under GDB: image counts change from zero to four negative placements while `needs_layers` stays zero, the non-layered branch runs, and no follow-up render is scheduled. Host-only selection clearing produces a layered draw with unchanged placements and 64 contiguous magenta pixel rows in both consumers. No target function calls or data writes; zero instrumentation errors and no cleanup survivors. |
| `final-geometry-clipboard-boundary-1/result.json`, `cleanup.json` | A 2 MiB supplied clipboard response is rejected before dispatch while the real child remains running. The same pending request then accepts exactly 1 MiB and delivers every byte to that child; explicit termination and exact-birth cleanup leave no survivors. |
| `positive-geometry-composed-3/geometry-summary.json`, `positive-right-edge-source-roi-view-{1,2}.png` | The native 1×1 image remains exactly one pixel in each physical view. A second placement produces a 15×13 derived upload with exactly 195 solid pixels in the narrow view and full 141×13 native coverage in the wide view. Native filtering is measured by coverage and center color, not an incidental edge kernel. Explicit deletion clears both images. This proves composed-scene geometry, not initial-presentation acceptance. |
| `positive-geometry-final-1/observations.json` | A standalone positive clipped image after all images were cleared times out. Its cause remains unproven; the composed-scene success doesn't erase it. |
| `native-layer-trace/run-1/failure.png`, `observations.json` | An instrumented fixture start produces uppercase/quote-corrupted physical typing before any graphics emission. Run 2 waits for observed keyboard flags before typing. That is diagnostic coordination, not a product fix or an established input-failure cause. |

Cleanup records for the clean baseline and final API, clipboard-boundary and composed-geometry fixtures record exact process births, exact-birth-only signalling and empty survivor lists. Cleanup success doesn't turn a failed visibility assertion into a pass.

## Native-only cutover verification

The replacement workspace contains only `bkpr-mux` and `bkpr-terminal`; the native executable is `bkpr`.

| Check | Observed result |
| --- | --- |
| `cargo test --locked --workspace` | 56 native tests passed: 27 mux, 6 real-process integration, 1 offer-lifetime and 22 terminal-adapter |
| `cargo build --locked -p bkpr-mux --bin bkpr` | Passed |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed |
| Rust/Nix formatting and devshell evaluation | Passed; non-Linux evaluation is not a runtime proof |
| Native daemon API smoke | Sizing/input/clipboard authority, two-view convergence, scrollback, OSC 7501, MIME reads/writes, stale-grant denial, detach survival and termination passed |
| Two actual native TUI clients | One-pixel image, composed clipped/native geometry and explicit image deletion passed; not initial-presentation acceptance |
| Retained release-analysis policy tests | 8 passed; separate from native runtime verification |

Cutover evidence is under `/home/calops/projects/bkpr/target/smoke/native-only-reset/`: `checks/result.json`, `api/observations.json`, `tui/observations.json`, physical screenshots and exact-birth cleanup records. Both smoke fixtures have no survivors. Native verification used a private public-output view with canonical caches/locks; the unrelated currently used executable's checksum stayed unchanged. No Zellij/WASI regression checks were run for this replacement, and no native Nix application package is claimed.

## Interactive latency and repaint correction

An actual user run exposed noticeable per-key latency and flicker. The old TUI serialized keyboard handling behind synchronous snapshot requests and a 30 ms input poll, while the listener accepted connections on an 8 ms timer. Every scene transfer repeated all terminal rows/styles, and every repaint cleared the viewport and positioned the cursor separately for every cell.

The corrected path uses a bounded snapshot worker and a wake descriptor alongside stdin, change-notified bounded server waits, and readiness-based listener acceptance. `SIGWINCH` wakes the client directly. Opt-in row deltas retain one shared baseline per view with complete-state fallback on baseline/geometry mismatch. Serialization and row comparisons run outside backend locks. Renderer damage tracks rows, palette, chrome and image projections: unchanged text/chrome/placements aren't repainted, changed rows aren't blanked first, and output is bracketed with synchronized-update controls. These are equivalent-output optimizations, not reduced fidelity.

Both executables use the same Cargo dev profile (`opt-level=1`), the same Ghostty engine and an isolated Kitty 0.49.2/Xvfb fixture on Linux x86-64. Each workload types 40 sequential keys into an owned real PTY child. Presentation is the first sampled change in the expected echo cell of Kitty's actual Xvfb framebuffer, not a protocol ACK or VT replay. Sampling requests 4 ms intervals; this isn't hardware display latency.

| Terminal | Build | Emitted VT p50 / p95 | Presented pixels p50 / p95 | Terminal bytes, 40 keys | Whole-line clears |
| --- | --- | --- | --- | --- | --- |
| 120×40 | Before | 90.10 / 102.19 ms | 116.18 / 131.73 ms | 1,707,198 | 1,600 |
| 120×40 | After | 12.50 / 18.23 ms | 21.41 / 27.90 ms | 13,958 | 0 |
| 240×80 | Before | 252.04 / 268.91 ms | 391.16 / 412.97 ms | 7,243,198 | 3,200 |
| 240×80 | After | 20.36 / 24.88 ms | 39.39 / 49.15 ms | 23,558 | 0 |

Both builds emitted zero terminal bytes during the two-second idle interval. The corrected build produced zero full-viewport repaint samples while typing. Wide/combining text, colors, cursor and physical WINCH were exercised; before/after terminal-body screenshots match excluding the blinking cursor cell. A positive-z image remained visible at every sampled instant while typing, and actual pixel replacement/deletion passed.

The static header didn't blank in either build's sampled frames. The destructive repaint mechanism is removed, but this fixture did **not** reproduce the user's physical flicker. Shorter unsampled transients and other host environments aren't ruled out. None of this closes the earlier below-text Kitty compositor or standalone-after-clear adoption gates.

Evidence lives under `/home/calops/projects/bkpr/target/smoke/input-latency/`: `comparison.json`, `final-before-120x40/`, `final-before-240x80-r2/`, `final-after-120x40/`, `final-after-240x80/`, `after-120x40-graphics/`, and `checks-final/result.json`. Raw timestamped input/output, framebuffer samples, screenshots and owned-process cleanup records are retained. Earlier incomplete capture attempts are separate from these final runs.

`retained-complete-120x40.png` and its sentinel closeup preserve the complete reattached scene, including typed input and wide/combining content. Physical captures are published only after presentation checks complete, not while a polling screenshot is still being replaced. Reproducible controller sources are archived in `controllers.zip`.

The verified executable is `checks-final/bkpr`, SHA-256 `1675ee87fbd315c747896b9485ebe20c45d071de0171e22ac176e0b2f1ef7922`. All 66 native tests, strict workspace/all-target Clippy, formatting and the native build passed. The shared canonical executable stayed unchanged at SHA-256 `14c1176e0718da7d3e5c72b0641317e504d08ec26bb4dc6d03f87f21536b2970`.

## Compatibility and remaining adoption gates

Only Linux x86-64 has actual build/runtime evidence here. The preparation recipe also names Linux aarch64 and macOS arm64 archives; those platforms and their packaging aren't verified by this report.

Sixel is unsupported and advertised as such. Physical OSC-52-only host clipboard **reads** are also unsupported: safe child OSC 52 reads go through correlated host OSC 5522 requests. Define the new product's supported host/capability matrix explicitly rather than inheriting old implementation parity. OSC 52 writes and the physical enhanced-paste path are separate supported observations.

Production acceptance still needs a host rendering fix and fresh initial-presentation evidence, closure of the unattributed positive-after-clear and startup observations, compatible upstream Rust/native adoption, platform and package closure, and the plan's registry, layout, control, metadata, harness, recovery and endpoint-security work. The compositor trace establishes a cause, not a shipped fix. Removing the old implementation does not make these observations pass, and none requires preserving the old runtime on this branch.
