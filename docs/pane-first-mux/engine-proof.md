# Initial pane-first engine experiment

## Decision

**Initial native evidence at `e9ee402`, updated for the native-only branch—not production acceptance.** The initial experiment ran independently of Zellij. This branch now removes the old runtime and makes the native application the only path; no old regression suite is required. Retained executable snapshots identify historical observations; current native verification is recorded below. Current usage is in [README](../../README.md), and the [implementation plan](implementation-plan.md) governs continued development.

The blocking observation is initial visibility of a four-row RGBA Kitty image below text. Both physical consumers remain blank after five seconds despite a valid normalized scene and host parsing. A read-only debugger trace on both fresh owned Kitty 0.49.2 processes establishes the cause: the host selects non-layered rendering from zero image counts, then rebuilds four negative placements without updating that decision or scheduling another render. A host-only empty `clear_selection` causes a layered draw with the same four placements and makes both images visible. This is a host compositor defect in the exercised fixture, not a graphics acceptance pass. Image/text reorder, synchronized-output wrapping, resource-ACK gating and tracing/environment hooks were removed rather than retained as redraw, sleep or layer workarounds.

Review also found two independent renderer defects: ordering within `BelowBackground`, and exact subcell/cropped/scaled destination geometry. Both repairs now have independent terminal-consumer and allocation-bound regressions, plus composed-scene physical geometry evidence. They don't fix or waive the initial-presentation gate.

## Implemented boundary

[`bkpr-terminal-engine`](../../crates/bkpr-terminal-engine/src/lib.rs) adapts the maintained safe Rust SDK over official libghostty-vt. The native engine parses child output, owns terminal state and encodes keys, mouse, focus and paste. Bkpr extracts typed cells, colors, cursor, modes, hyperlinks, resolved image resources and placements into its [protocol scene model](../../crates/bkpr-protocol/src/model.rs). OSC 7501 supplies semantic status; titles are not status. Held native resources do not escape into clients, and child escape strings are not replayed as trusted host commands.

[`bkpr-core`](../../crates/bkpr-core/src/lib.rs) owns independent `portable-pty` runtimes and a durable workspace/layout registry, assembled by `bkprd`. Normal execution preserves user HOME/profiles; explicit fixture homes scrub inherited credentials. [`bkpr-client`](../../crates/bkpr-client/src/lib.rs) consumes protocol scenes through the [unified composition framework](../../crates/bkpr-tui-framework/src/lib.rs). Views and layouts do not own process lifetime. Queue acceptance is distinct from child processing; ambiguous input is not blindly replayed.

The [authority model](../../crates/bkpr-core/src/authority.rs) separates view identity, visibility, interactivity, input and clipboard permission. Latest eligible visible interactive view controls PTY size unless pinned. Read-only input is independent of sizing eligibility. Clipboard authority and each view's scrollback are separate. View leases expire after ten seconds; clipboard requests time out after five seconds. Stale grants and detached views fail closed.

Linux [process cleanup](../../crates/bkpr-core/src/process_tree.rs) is scoped to a pane's birth-verified PTY session and observed descendants, not every orphan adopted by the shared daemon. It observes the session before reaping its leader. Deliberately detached services are not an OS-container containment promise. The earlier one-pane experiment used process-wide adoption; that mechanism is not appropriate for a shared multi-pane server.

### IPC and resources

The [private Unix transport](../../crates/bkpr-core/src/transport.rs) uses Planus binary wire family 6 for typed workspace/window registry operations, atomic pane creation/placement, one-shot client source publication and pane-scoped requests. It requires private current-user-owned directories, creates a mode-0600 socket, and does not replace a live listener. A fixed admission header checks family, message kind, required capabilities and the 8 MiB request / 96 MiB response bounds before allocating the payload. Control capacity remains reserved independently of long polls. These remain same-user local boundaries, not authenticated remote enrollment.

Clients receive compact lossless binary scene runs and whole-row deltas on snapshot workers. Image receipts include ID and generation, avoiding stale pixels or needless retransmission. The received allocation owns borrowed scene slices; complete validation precedes expanded native cells/resources and failure-atomic application. CLI/MCP/provider and persistence JSON are not the native wire format. Workspace/window/pane metadata and revisioned layouts persist; terminal contents and processes do not. Restored records without runtimes are explicitly Stopped. Sustained-animation CPU work remains measured separately below.

### Experiment limits

| Boundary | Bound |
| --- | --- |
| One input, paste or supplied clipboard payload | 1 MiB of data; larger payloads are rejected before dispatch |
| PTY writer | 2 MiB user budget, 4 MiB total queue |
| Local IPC | 8 MiB request, 96 MiB response; separate pane-wait, registry-wait and control admission budgets |
| Canonical image resources accepted by the TUI | 8192 pixels per dimension, 64 MiB aggregate |
| TUI projection working set | 128 MiB preflight budget for canonical images, visible derived tiles and planning metadata |

Derived images sample only visible pixels, retaining the full placement's sampling phase. Uploads are tiled at 10,000 pixels per dimension; offscreen target extents don't cause full-size raster allocation. These are experiment bounds, not final product limits.

## Native provenance and dependency ownership

The reproducible prerequisite is [`scripts/prepare-ghostty-vt.sh`](../../scripts/prepare-ghostty-vt.sh):

- Ghostty source: [`93967b7f9478e94dcbb42125c8a90f34feb40d75`](https://github.com/ghostty-org/ghostty/tree/93967b7f9478e94dcbb42125c8a90f34feb40d75).
- Source archive SHA-256: `af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70`.
- Zig: `0.17.0`, with platform archive hashes in the script.
- Build profile: `ReleaseSafe-simd-libc-baseline-offer-v1`, using `-Demit-lib-vt=true -Doptimize=ReleaseSafe -Dsimd=true -Dcpu=baseline`.
- Exact-source [clipboard patch](../../vendor/libghostty-rs/crates/libghostty-vt-sys/native/clipboard-offer.patch) SHA-256: `196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c`.
- Exercised native prefix: `/home/calops/projects/bkpr/target/ghostty-vt-93967b7`. Its provenance stamps and `bkpr-files.sha256` bind the headers and static library to the source/profile/patch.

The supported SIMD build links libc. The tested `-Dsimd=false` build brought weak allocator stubs that preempted Rust's libc allocation calls when statically linked; it is not the recipe used here. The script restores archive-owned source/toolchain files and applies the native patch with zero fuzz. The [engine build check](../../crates/bkpr-terminal-engine/build.rs) verifies the exact prepared installation; generic FFI ownership and checked declarations belong to the upstream SDK.

The patch exposes an optional paste `offer_id` and a read's `paste_offer_id` after native one-time-password validation. A boolean saying that a read was granted isn't enough: Bkpr must bind the read to the exact physical paste offer and its originating live view, not whichever clipboard contents happen to exist later. The retained SSH PNG fixture changes the host clipboard before the child reads, yet returns the original offered image.

The safe/sys SDK is pinned at [`575ed6b8ceb79ffaf247ecf7e34333876b0d5be4`](https://github.com/Uzaaft/libghostty-rs/tree/575ed6b8ceb79ffaf247ecf7e34333876b0d5be4), with a reproducible separately reviewable patch. Native stays at the tested `93967b7` rather than downgrading to the SDK's older `521cf2c`; parser-boundary declarations are updated together. Safe paste-offer correlation, exact prebuilt-native linking, optional image-data handling and pre-allocation PNG output budgets are documented in [UPSTREAM.toml](../../vendor/libghostty-rs/UPSTREAM.toml). Bkpr no longer owns a parallel generic FFI wrapper or bindgen step. The C API remains unstable, and these local dependency patches are not claimed upstream.

## Build and try it

Use the current [README build and native CLI instructions](../../README.md#native-cli), which build both `bkpr` and `bkprd`. They describe default normal-profile execution and an explicit private fixture environment. Current work uses this feature worktree's own `target/` and bounded eight-job builds, not main's Cargo cache. The historical measurements below identify their own binaries and must not be read as acceptance of later code.

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

## Historical native-only cutover verification

At the initial cutover, the workspace contained `bkpr-mux` and `bkpr-terminal` and produced one `bkpr` executable. The seven-crate workspace has since replaced that structure. The following results describe the retained initial-cutover binaries, not the current workspace build.

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

## Full-screen editor redraw correction

Sparse typing wasn't sufficient acceptance. The subsequent user report concerned actual Neovim jumps between the start and end of a large file, which changes the entire viewport. The row-delta path still expanded every changed cell into repeated JSON objects and rebuilt them on the UI thread.

A 50-sample synthetic colored-grid profile separated terminal work from transport. At 240×79, native feed/extraction took about 1.14 ms; JSON expansion, serialization, parsing and reconstruction accounted for about 77 ms as a sum of individual stage medians. A fresh same-process comparison of the complete old/new scene-processing paths measured:

| Grid | Old processing median | Compact processing median | Old payload | Compact payload |
| --- | --- | --- | --- | --- |
| 120×39 | 18.18 ms | 0.67 ms | 1,716,884 B | 16,649 B |
| 240×79 | 94.76 ms | 2.36 ms | 6,943,844 B | 49,059 B |

These processing measurements exclude IPC and physical rendering. Both use the same dev profile and assert exact decoded `Frame` equality. The earlier 77 ms and fresh 94.76 ms aren't interchangeable measurements; the direct old/new comparison is the latter run.

The TUI now uses compact JSON text runs with response-local style/link tables and explicit RLE cell boundaries. Unicode, empty/zero-width cells, color provenance, hyperlinks, image receipts and metadata remain lossless. Decode/expansion happens on the bounded snapshot worker; row patches apply in place on the UI. Readable JSON remains available for inspection. Maintained `crossbeam-channel` selects ready PTY output, controls and writer progress; PTY readiness replaces the reader sleep. Timeouts serve cancellation, maintenance and actual terminal deadlines rather than gating output.

The first compact candidate still emitted redundant SGR resets/backgrounds and no-change cursor transactions. At 240×80 styled redraws, it sent about 153 KB per jump versus Zellij's 101 KB. Differential styles, unchanged-presentation suppression and cursor-color tracking brought this to about 98 KB. SGR 22 resets both bold and faint; the target pair is reapplied explicitly. That property is also recorded in the retained fork source ledger. No color, style or layout fidelity was removed.

Final end-to-end fixtures use real Neovim `0.13.0-nightly+c049b8f`, a 100,000-line file with 303-column lines, `nowrap`, twelve alternating `G`/`gg` jumps and a five-jump burst. Styled fixtures use RGB groups changing every 6/6/6/8 characters with bold, italic and underline. Kitty `0.49.2` renders into private Xvfb; an independent process timestamps complete-body framebuffer matches. These are sampled presentation upper bounds, not hardware display latency. Full visible text and immutable full-body RGB references must match before a frame is accepted. Bkpr before/after builds both use dev `opt-level=1`; the installed Zellij `0.45.1` executable is a runtime comparator, not an identically configured build.

| Workload | Previous Bkpr emitted / presented median | Final Bkpr emitted / presented median | Zellij emitted / presented median |
| --- | --- | --- | --- |
| Plain 120×40 | 64.07 / 77.70 ms | 9.89 / 24.25 ms | 12.24 / 26.72 ms |
| Plain 240×80 | 259.17 / 291.59 ms | 9.34 / 44.00 ms | 13.19 / 54.37 ms |
| Styled 120×40 | 64.73 / 95.31 ms | 10.86 / 30.87 ms | 12.71 / 39.12 ms |
| Styled 240×80 | 280.05 / 385.70 ms | 24.86 / 94.32 ms | 28.04 / 98.19 ms |

The final styled-240 Zellij comparison ran adjacent to the final candidate. Its five-jump burst completed in 139.47 ms versus Bkpr's 136.55 ms. Final emitted maxima were 31.53 ms for Zellij and 37.39 ms for Bkpr; complete-presentation maxima were 107.04 and 106.66 ms. These observations establish comparable behavior in this fixture, not universal parity or a product-wide latency guarantee.

The final sparse/graphics regression retained exact wide/combining text, colors, resize, detach/reattach and positive-z image retention/replacement/deletion. It emitted no idle bytes or full repaints; median/p95 echo was 2.61/11.48 ms and sampled presentation 13.23/21.08 ms. A failed intermediate styled reference was a stable black startup capture; the actual timeout frame matched the immutable baseline pixel-for-pixel. Reference acquisition was corrected to require expected complete content before publication, and the failed artifacts remain diagnostic evidence rather than being counted as passes.

Evidence is under `/home/calops/projects/bkpr/target/smoke/editor-redraw/`: stage profiles, `stage-profiler.zip`, comparison summaries, `final-style-*` editor/sparse runs, controller archive, raw timestamped VT, framebuffer observations, pixel-diff and cleanup records. The final binary is `checks-style/bkpr`, SHA-256 `fb0fe0754c2452b128fd4707ffb7f8dba00c0a496d21c02c5d1907a26cb109f2`. All 72 native tests, strict all-target Clippy, formatting and build passed; canonical live-executable checksum stayed unchanged. This doesn't change the earlier graphics adoption gates or add production workspace/harness services.

## Compatibility and remaining adoption gates

Only Linux x86-64 has actual build/runtime evidence here. The preparation recipe also names Linux aarch64 and macOS arm64 archives; those platforms and their packaging aren't verified by this report.

Sixel is unsupported and advertised as such. Physical OSC-52-only host clipboard **reads** are also unsupported: safe child OSC 52 reads go through correlated host OSC 5522 requests. Define the new product's supported host/capability matrix explicitly rather than inheriting old implementation parity. OSC 52 writes and the physical enhanced-paste path are separate supported observations.

TUI production acceptance still needs a host rendering fix and fresh initial-presentation evidence, closure of the unattributed positive-after-clear and startup observations, compatible upstream Rust/native adoption, supported-platform/package closure, and the plan's remaining metadata, harness and execution-recovery work. Authenticated remote endpoints and independently implemented frontends have their own deferred acceptance gates; they are not prerequisites for restoring TUI parity with `main`. The local registry, layout documents, typed control and TUI are implemented portions of that plan, not those remaining services. The compositor trace establishes a cause, not a shipped fix. Removing the old implementation does not make these observations pass, and none requires preserving the old runtime on this branch.

## Seven-crate SDK and local workspace cutover

The active implementation now builds `bkpr` and `bkprd` from the seven packages in the implementation plan. Generic terminal ownership, encoders, iterators and callbacks use the pinned safe/sys SDK pair; Bkpr retains canonical scenes, bounds, caches and authority. The reproducible SDK patch and native revision are recorded in [UPSTREAM.toml](../../vendor/libghostty-rs/UPSTREAM.toml). The local PNG contribution rejects oversized RGBA output before allocating the output buffer; grayscale and alpha remain supported.

The backend owns independent PTYs, workspace membership and revisioned layout documents. Removing a layout reference, moving a projection or disconnecting a client does not terminate a process. Metadata persists atomically under an exclusive state-root lease. Restarted records are explicitly stopped, not recovered executions or restored terminal contents. Failed storage commits poison subsequent commit paths; explicit shutdown still cleans up.

Native callback isolation required more than multiple backend panes. A real pending clipboard callback showed that the original single client input worker serialized Alpha's focus-out ahead of Beta's input. The client now uses bounded pane-local FIFO lanes, independently admitted clipboard responses and policy changes, and asynchronous attachment/management jobs. Hidden views revoke sizing/device eligibility without waiting behind native input, then drain accepted input and complete ordered retirement. Unknown transport/native outcomes stop only the affected lane and are never replayed. Registry and selection completions cannot replace newer state.

Physical mouse testing also reproduced a routing bug: Alpha received `ESC[<0;20;2M`, but dragging over Beta delivered `ESC[<32;20;2M` to Beta. Pointer capture now binds the complete lifecycle to its original pane/view, clips to actual pane geometry and completes accepted gestures before retiring that projection. Bounded release escrow preserves ordering when normal admission or an enhanced-paste barrier is occupied.

### Verification

| Check | Observed result |
| --- | --- |
| Locked native workspace tests | 158 passed; one ignored entry is the subprocess fixture invoked by its passing parent regression |
| Independently reacquired patched SDK tests | 111 safe-crate and 3 sys-crate tests passed, including ABI, offer provenance and pre-allocation PNG bounds |
| Workspace debug/release builds, all-target strict Clippy, Rust formatting | Passed |
| Nix formatter and preparation-helper shell syntax | Passed; actual development-shell execution is Linux x86-64 evidence, not cross-platform packaging |
| Real daemon API | Independent membership/projection, stale CAS rejection, actual shell output, detach survival, termination and honest metadata-only restart passed |
| Actual Kitty clients | Splits, workspace creation, rename cancellation/focus restoration, existing-pane projection, layout-only removal, pointer navigation, independent resize and default-client daemon bootstrap passed |
| Native callback with physical unrelated input | Beta's physical shell command completed while Alpha's original clipboard request remained pending; the original request then delivered exact bytes including NUL, newline and `ff` to the actual raw PTY consumer |
| Physical gestures | Cross-pane press/drag/release stayed in Alpha, Beta received no unmatched events, and hiding the pressed projection completed its original release before detach |
| Physical graphics | Two panes reused native image ID 41 with independent red/blue pixels; deletion cleared both. This is AboveText evidence, not closure of the historical BelowText gate |

Evidence root: `/home/calops/projects/bkpr/target/smoke/workspace-cutover/`. Final coordinated records are `checks/result.json` and `release/result.json`; API evidence is in `api-release/`. Complete optimized-release terminal acceptance is `ui-1024809207520082/`, including screenshots, raw VT, origin-bound clipboard bytes, gesture logs and empty-survivor cleanup. The failing old mouse reproduction is retained separately in `ui-1020666531141315/`. The earlier global-input stall and its syscall trace are in `ui-1010976333603051/`.

One intermediate bootstrap fixture typed as soon as the daemon socket appeared, before the new client's attachment was ready. The UI reported that input was not dispatched; the resulting command lost its prefix. The final fixture waits for the client-consumed attachment's visible promotion before typing. Socket creation alone is not input readiness.

The verified release client is SHA-256 `bc750e1fcf1d87f7de045c76d4c4a44ef5353c746c2d2f2c98214ad7d3091770`; server is `9f0e152b5b3c2c3a3cfdb9fe17ad248fce40405e1125e2f8558deffc24168e8f`. Builds used eight Cargo jobs and an explicit CPU affinity of 0–7, with the existing native prefix and shared Cargo caches.

### Artifact-isolation incident

An initial verification helper used automatic recursive temporary-directory cleanup around a bind alias. After an `EBUSY` unmount failure, that cleanup traversed the still-mounted alias and deleted the shared `target/debug` cache. The original public `debug/bkpr` was recovered from its still-running executable image, verified at SHA-256 `14c1176e0718da7d3e5c72b0641317e504d08ec26bb4dc6d03f87f21536b2970`, and restored exactly. Source and release artifacts were unaffected; running executable images were not changed. The debug cache was lost and only subsequently repopulated as needed—not preserved unchanged.

The repaired helper never recursively deletes alias/output directories. Failed unmounts leave owned directories for inspection; successful cleanup only unlinks owned files/symlinks and removes empty directories. Forced-unmount-failure proof is retained in `guard-fault-result.json`. Final debug and release records compare the public executables found immediately before and after each namespace build, preserving them as found rather than restoring an older checksum over later independent changes.

### Release performance with the actual sidebar

The comparison keeps the real Neovim pane at 120×39 or 240×79 cells. The old standalone client uses a 120×40 or 240×80 window; the new client retains its real 24-column sidebar and header, using 144×41 or 264×81. Both are release builds against the same native engine. These are private Kitty/Xvfb software-framebuffer measurements, not hardware-display latency or a universal parity claim.

The first full-UI candidate repainted static sidebar cells whenever a terminal row changed. One allocation-free physical-glyph damage span per changed row now skips unchanged prefixes/suffixes, preserves complete old/new wide glyphs and clears only the damaged remainder. Geometry/palette invalidation still redraws fully. No graphics, style, hyperlink, Unicode or input fidelity was removed.

| Workload | Old emitted median | New emitted median | Old presented median | New presented median | Old / new presented p95 |
| --- | --- | --- | --- | --- | --- |
| Plain 120×39 pane | 9.20 ms | 10.52 ms | 23.51 ms | 24.62 ms | 25.02 / 27.05 ms |
| Styled 120×39 pane | 11.66 ms | 12.08 ms | 32.92 ms | 33.21 ms | 40.24 / 44.28 ms |
| Plain 240×79 pane | 8.66 ms | 10.24 ms | 43.10 ms | 47.46 ms | 49.70 / 52.45 ms |
| Styled 240×79 pane | 23.01 ms | 22.42 ms | 89.91 ms | 94.69 ms | 106.19 / 102.65 ms |

All 184 complete-body checks across old/new runs passed. Twelve cross-run full-body reference comparisons had zero different pixels. At styled 240, the first full-UI candidate emitted 102,408.5 median bytes per jump; cell-span damage reduces that to 94,474, below the old standalone client's 97,958. Its emitted/presented medians improve from 25.99/101.02 ms before spans to 22.42/94.69 ms. Small-window emission varies by roughly a millisecond and large-window sampled presentation remains slower than the smaller old window; this is not reported as strict parity.

Actual `cmatrix 2.0 -u 1` uses a 240×79 pane, three-second warmup and ten-second capture, twice per mode. Table values are medians across those two runs; CPU sums only mux processes, excluding cmatrix, Kitty, Xvfb and observer processes. Summed RSS double-counts shared pages and is not PSS. Distinct sampled body pixel states are not exact completed/dropped frames or physical FPS.

| Runtime | Outer window | Mux CPU, cores | Summed RSS | Sampled body changes/s |
| --- | --- | --- | --- | --- |
| Old standalone Bkpr | 240×80 | 1.47 | 46.24 MiB | 60.33 |
| New Bkpr + bkprd, real sidebar | 264×81 | 1.52 | 54.92 MiB | 52.44 |
| Installed Zellij | 240×81 | 0.17 | 112.51 MiB | 61.88 |
| Old Bkpr, pinned pane in larger window | 264×81 | 1.57 | 43.45 MiB | 39.79 |

The equal-outer-window control genuinely pins the old pane at 240×79 before enlarging its window, keeps its no-input/no-clipboard sizing lease alive and confirms actual consumer geometry. It also introduces blank padding and the old renderer's associated clear/reset behavior, so it is not a pure isolation of host raster size. It establishes that outer-window/projection geometry materially affects this software fixture; it does not prove that every cadence difference is caused by Kitty. Source burst rates stay near 89/s. Final new-client maximum sampled gaps were 35.38 and 35.20 ms; no supported capability or update fidelity was disabled to obtain them.

At this checkpoint the CPU-efficiency gap to Zellij remained deferred. The architecture cutover did not solve sustained-animation efficiency; the later CPU-efficiency section records the subsequent diagnostic and optimization work without silently throttling animation.

Evidence: `performance/editor/comparison-final.json`, content-addressed `*-candidate-bc750e1fcf1d/` runs, `performance/animation/comparison.json`, `performance/animation-equal-window/`, immutable old/full-UI binaries and `comparison-before-cell-spans.json` in both workload directories. Baselines remain SHA-256 `e75528a281d9e127e8a39dfaff96c9c3f04c7e6a2b3f2849cc6b507b92c97fb4`; earlier measured candidates are retained separately. No compilation or other owned heavy workload ran during performance captures.

Reproducible verification/controller sources are retained in `controllers-final.zip` (22 source files, archive CRC checked). Owned HOME/XDG/socket fixtures and exact-born runtime processes were cleaned; all six final performance cleanup records and the final physical UI record have empty survivor lists. The controllers are evidence tooling, not installed product commands.

## Shell execution closure correction

A user run exposed dead shell projections remaining in the layout and an input error on exit. The isolated old-release trace in `shell-exit-1044867768567856/` shows both causes: an exited shell remains referenced by two workspace layouts, and a valid post-exit focus request returns `pane command queue full or closed`. Physical Ctrl-D also leaves an `INPUT ERROR` key-lane banner because its release can race worker completion.

Normal root-process completion now atomically persists status/exit code and removes that pane from every affected layout, collapsing only affected split paths and incrementing each affected workspace revision once. Final in-memory scenes and independent execution records remain readable. Faults remain visible, and normal completion cannot silently turn a failed durable write into success. Explicit termination is one backend operation; the obsolete frontend terminate-then-layout-CAS sequence is removed.

Command submission and worker finalization share a short gate, never held while awaiting native work. Already-ended key-release, pointer-release and focus cleanup returns explicit `accepted:false`, no write ID and `execution_completed` semantics; it does not claim input delivery. Actual key presses, raw input and paste remain rejected after completion. Faults, capacity failures and unknown outcomes remain errors. The client also stops known-completed held releases and pending focus-out locally while preserving Detach and unrelated-pane input.

Completed records without a layout projection are no longer presented as live sidebar panes. Explicitly projecting a retained final scene remains possible; fault records remain discoverable. Physical acceptance checks both the terminal area and sidebar, not just backend metadata.

Verification: 163 native workspace tests and 114 independently reacquired SDK tests passed; strict all-target Clippy, Rust formatting and debug/release builds passed. Real Kitty clients exercised Ctrl-D, confirmed termination, a nonzero shell exit, one pane projected in multiple workspaces, automatic peer focus/input, repeated completed termination, creation after the last old shell closes, sidebar removal, and retained scene/exit metadata. Full multi-client clipboard/pointer/resize/graphics acceptance and the daemon API smoke also passed. This lifecycle-only change did not rerun the unrelated editor/animation benchmark matrix.

Evidence remains under `target/smoke/workspace-cutover/`: `shell-exit-checks/result.json`, `shell-exit-release/result.json`, final physical scenario `shell-exit-1045639412059341/`, broader terminal scenario `ui-1045643374959965/`, API scenario `api-shell-exit-release/`, and `shell-exit-controllers.zip`. Final runtime fixtures have empty survivor lists. Build jobs and CPU affinity remain limited to eight, and the existing canonical/public preview executables were not replaced.

The new release is staged separately in `shell-exit-release/`: client SHA-256 `62873a0bb796af868eeee0322b981170ca1730d047fe666c577e27980f90e766`, server `3b459ba41c94b1862a948e6aa990f387a9782556baa5513d1f2a27a614a2177e`. Start a fresh daemon to exercise the changed backend lifecycle; attaching a new client does not upgrade an already-running daemon.

## Native harness capture

The native implementation now separates foreground process/CWD recognition, authenticated managed launch identity, native conversation binding and OSC 7501 semantic status. Core owns authority, lifecycle and status selection; the client displays typed facts. User pane names remain unchanged. Ordinary private-PATH wrappers and `bkpr run` share the same adapters; explicit Antigravity installation is the only persistent harness-configuration mutation.

Capture authentication uses the socket peer's OS identity and birth, selected Bkpr executable and verified pane ancestry. Linux requires `SO_PEERPIDFD`; unsupported kernels fail managed capture closed rather than trusting an environment PID. Native conversation sources must match the launch kind; replacement generations and retirement prevent delayed callbacks from reviving exited launches. Background jobs retain private live authority but do not masquerade as foreground harnesses.

One shared process sampler performs off-path observations at a 200 ms foreground deadline. Reaper observations use its cache; final Linux root-exit handling obtains a fresh ownership barrier before reaping, including stopped jobs in other process groups. Status-only wakes coalesce and select changed metadata without process scans, hidden-view admission or full-scene/registry copies. Transition invalidation prevents stale OSC facts from reappearing after shell return; a managed waiter's first verified native child preserves genuine startup OSC instead of discarding it.

`PaneInfo` carries foreground CWD, harness identity/binding and the selected OSC summary. `PaneSnapshot.program_identity` lets clients reject mismatched facts across independent stream pumps. UTF-8 paths retain their JSON string representation; non-UTF-8 Unix paths round-trip as `{"unix_bytes":[...]}`. Live capture grants, recognition, native associations and OSC summaries are excluded from durable layout records. Historical associations and execution recovery are still future work.

### Exercised acceptance and native prerequisites

Evidence root: `/home/calops/projects/bkpr/target/smoke/harness-capture/`.

| Check | Observed result |
| --- | --- |
| Locked native workspace tests | 225 passed; five ignored entries are subprocess helpers exercised by passing parent regressions |
| Strict workspace/all-target Clippy and Rust formatting | Passed; final record `checks/result.json` |
| Optimized release build | Passed with eight jobs and affinity 0–7; `release/result.json`; canonical public executables unchanged |
| Real Omp 18.13.2 | Ordinary and explicit launch bind exact native extension IDs before prompts; concurrent launches, native resume/fork, replacement/exit and stale actual capabilities passed; one loopback fake-provider request |
| Real Codex 0.165.0 | Before-prompt IDs corroborated through the actual owned app-server; native resume/fork/picker, concurrent launches and replacement/exit passed; native children have no capture token; two loopback requests |
| Real Claude 2.1.312 | Native first-run connectivity preflight exits before hooks in the offline namespace, despite a local gateway and disabled nonessential traffic; native hook/ID acceptance remains blocked |
| Real Antigravity 1.4.8 | Actual `bkpr install agy` and native CLI preserve unrelated settings and unknown fields; native Terms/Data Use remains unapproved, so genuine root-MCP binding is unverified; two loopback requests |
| Actual Kitty TUI | Concurrent independent panes, ordinary/explicit launch, exact native-ID details, genuine OSC markers, generic-program exclusion, hidden status with zero views, non-UTF-8 CWD, suspend/foreground, shell return, matching utility/socket, user names and eight-row chooser passed |

The real-harness matrix is `native/native-acceptance-summary-20261031.json`, with native executable and candidate hashes alongside it. Omp's private fixture uses source-supported `startup.setupWizard:false` and a custom local provider, not seeded authentication/approval state. Codex's intentional local HTTP rejections create native resumable state. Its proxy keeps capabilities private: native stale-launch checks did not recover an actual retired token; actual-token rejection is separately exercised by the Omp/backend fixtures. Claude and Antigravity limits are native prerequisites, not silently passing cases. No native Terms, trust, API-key or tool approvals were granted, and no external provider was reached. [Claude's environment reference](https://code.claude.com/docs/en/env-vars.md) and [upstream preflight report](https://github.com/anthropics/claude-code/issues/81611) did not supply a supported offline preflight opt-out.

Physical release evidence is `ui-1076638967104205/`, including actual screenshots, raw VT, observations and empty-survivor cleanup. Its named synthetic executable exercises the real Bkpr authentication/PTY/OSC/control path; it is not substituted for genuine harness-ID evidence. Boundary regressions also cover wrong peers/sources, nested or indeterminate runtimes, stale births, callback/transport bounds, correlation/reconnect ordering and failure closure.

The verified release client is SHA-256 `49ee03e62a3da8f2c46e11030fb9799e1672b32dff3cef45effe8957ed9d2b27`; server is `5fc1eb48b865b6ff8501f859817098659b1e258363dbe8c300af5e20f871cbb9`. The `checks/` executable snapshots are an earlier debug build, not the release candidate; its final record verifies the current tests/lints, not those snapshots' runtime acceptance. Native engine revision and SDK are unchanged. macOS capture has source-level implementation but no runtime evidence here.

### Responsiveness and remaining efficiency work

The comparison uses the previous shell-closure release and this capture release, both with the real sidebar. Styled Neovim has the same 240×79 pane in a 264×81 window, twelve settled jumps and six five-jump bursts per build. Complete body pixels match the immutable cross-run oracles; emitted median bytes remain 94,474. Independent framebuffer sampling observes software presentation, not hardware-display latency. No compilation or other owned heavy fixture ran during measurement.

| Metric | Previous release | Capture release |
| --- | --- | --- |
| Settled jump emitted median / maximum | 27.01 / 33.29 ms | 26.62 / 33.34 ms |
| Settled jump sampled presentation median / maximum | 123.04 / 140.05 ms | 115.56 / 146.19 ms |
| Five-jump burst emitted median / maximum | 70.63 / 82.40 ms | 67.28 / 90.65 ms |
| Five-jump burst sampled presentation median / maximum | 151.09 / 172.43 ms | 162.89 / 177.85 ms |
| Eight idle panes, daemon CPU cores | 0.502 | 0.052 |
| Eight idle panes, daemon RSS | 29,376 KiB | 30,140 KiB |

Idle CPU is one five-second procfs user+system sample after a two-second warmup, without clients/providers; it demonstrates the shared sampler's reduction, not a solved sustained-animation CPU gap. Settled emission is similar, while sampled presentation and burst tails vary; no universal speedup or strict latency-parity claim follows from this pair. Raw records, geometry, hashes and empty-survivor cleanup are retained in `performance/comparison.json`, `performance/editor-{before,capture}/` and `idle-1076880544092944/`. No fidelity or update-rate reduction was introduced.

### Fixture corrections and safety

An initial one-millisecond XTest key burst produced wrong case/quotes. The direct control `direct-keys-1076569173726844/` requests the same Kitty CSI-u flags without any Bkpr process: the host wire itself reports Shift and uppercase associated text for `printf`. At ten milliseconds it reports the intended text. The physical controller now uses that demonstrated driver interval; no product keyboard behavior was changed or suppressed. This does not retrospectively attribute earlier unrelated startup observations.

Early Omp version/noninteractive probes selected the user's profile wrapper before inspection. It attempted 1Password credential lookups and returned only failure diagnostics; no credential values were returned or retained. All subsequent runs use inspected package entrypoints/direct native executables. Isolated HOME/network namespaces do not sandbox absolute credential paths or local sockets. This incident and exact scope are retained in the native manifest.

The initial Bash fixture also sourced host-global `/etc/bashrc` and `/etc/profile`, whose generated environment overwrites PATH and discovers an SSH-agent socket. Source-supported `NOSYSBASHRC=1` now excludes that host fixture configuration, without reinjecting PATH or modifying user profiles. Ordinary product interception still depends on the private PATH entry surviving shell/direnv changes; the explicit launch path remains available and wrapper-free integration stays in TODO.

Owned runtime roots/processes were cleaned with exact-birth checks; all final native, physical and performance records report no survivors. Controller sources are retained in `controllers.zip`, with CRC verification. This capture work does not supply broker delivery, Git/CI, recovery selection, remote endpoints or closure of historical graphics adoption gates.

## Native agent communication

The daemon now owns launch-scoped context, discovery, titles and durable peer messages. Seven tools expose identity, discovery, send, inbox read, processed acknowledgement and body-free outbox receipts. Workspace scope uses execution membership; repository scope uses canonical common Git identity, never a guessed CWD association. Session/all currently mean this daemon, not remote endpoints.

Authority combines private launch capabilities with verified process birth and live ancestry. Codex relay requests additionally verify the original kernel peer rather than trusting copied relay credentials. Private atomic storage preserves exact sender-scoped idempotency and receipts, retains unacknowledged bodies for seven days after persisted recipient retirement, and suspends all commits/callbacks after durable-store failure. Restart does not let a new launch adopt old authority or inboxes. Retained historical records do not imply a shipped historical inspection endpoint.

Native binding, live receiver identity, subscription readiness, wake acceptance, reading and processed acknowledgement are separate. Notifications carry opaque pointers, never peer bodies or PTY input. Receiver replacement/rebinding retires old generations without blindly replaying ambiguous effects. Omp's void `sendMessage` cannot prove native acceptance: its receipt stays `unknown` even when a particular observed turn subsequently reads and acknowledges the message. Claude idle re-wake offers have the same truthful uncertainty.

### Runtime evidence and limits

Evidence root: `target/smoke/native-agent/`. Final optimized binaries: `release/bkpr` SHA-256 `552e919af9bf6e02a3f41f8b15364053e686d24a3cceca913ca2e77b96d141d9`; `release/bkprd` SHA-256 `3ed4d8d38875c57d4199072efcbbebb74256b69986aee1458cda5b5f3534794a`. Canonical installed/build-output runtime artifacts were preserved through private mount-namespace builds sharing the existing Cargo caches.

| Path exercised | Observed result | Evidence |
|---|---|---|
| Two genuine Omp 18.13.2 conversations | Native discovery/send; opaque aside in recipient model context during an active turn; actual inbox read and processed acknowledgement; sender outbox independently observes both timestamps. Eight loopback provider requests, no external provider. | `omp-active-1081655947137793/active-proof.json` |
| Genuine Codex 0.165.0 | Before-prompt exact-thread binding; send while first turn held; native future-turn queue accepted; next turn starts automatically and its actual provider input contains only the opaque pointer. Message remains unread/unacknowledged. | `codex-queue-1081677301400578/queue-proof.json` |
| AGY receiver and metadata MCP | Actual daemon/OS-authenticated synthetic root, descendant receiver and loopback `SendAgentMessage`; wrong conversation rejects; distinct acceptance/read/ack; complete 8,524,467-byte escaped page. This is helper proof, not genuine AGY app acceptance. | `agy-helper-1081677305686033/helper-proof.json` |
| Claude hooks/watcher | Actual authenticated synthetic parent and native helpers exercise tool-boundary context, idle re-wake, historical outcomes after read, new-arrival races, retirement and UTF-8 limits. Runtime caught and fixed missing `0700` on the coordination directory. Not genuine Claude host consumption. | `claude-hook-1081294190426037/result.json` |
| Actual MCP wire | Ten maximum-size escaped bodies round-trip in an 8,524,359-byte frame; initialization, typed cancellation, duplicate IDs, eight-worker limit, invalid/foreign OS peers, 32 subscriptions and control-capacity recovery. | `mcp-1081825385820797/result.json` |
| Actual Kitty TUI | Completed ordinary shell command and management form remain interactive while an authenticated peer subscription is pending. Screenshot inspected, not merely a command-echo assertion. | `physical-1081696705217542/` |
| Durable API | Exact retries, conflicting idempotency rejection, inbox read/ack and separate body-free sender receipts. | `api-1081655887146689/result.json` |

Genuine Codex MCP function execution reaches a native approval prompt, left unapproved (`codex-delivery-1081489677577433/`). Genuine AGY Terms/native receiver approval remain unaccepted. Claude's offline startup limitation was independently reproduced without Bkpr in the preceding capture investigation. No persistent approval settings were changed, real model services contacted or fixture peer notices typed into PTYs.

The first release physical run incorrectly accepted the marker in echoed input; visual inspection found malformed case/quotes. That run (`physical-1081655913756111/`) is invalidated, not evidence of completed shell input. The corrected oracle requires a standalone output line in both backend and physical terminal; the final run uses 25 ms XTest spacing. This changes only fixture driving, not product keyboard behavior.

### Efficiency and remaining bounds

The backend reserves subscriptions independently from general controls, uses cancellable long waits, and avoids per-second process/store scans for idle waiters. No empty-read/duplicate-ack persistence rewrites. Bodies are shared in memory rather than cloned for each transactional snapshot.

| Five-second idle sample | Panes | Pending authenticated subscriptions | Daemon CPU seconds | CPU cores | RSS KiB |
|---|---:|---:|---:|---:|---:|
| Previous capture release | 8 | 0 | 0.27 | 0.054 | 30,012 |
| Communication release | 8 | 0 | 0.26 | 0.052 | 30,932 |
| Communication release, subscription capacity | 17 | 32 | 0.32 | 0.064 | — |

These short procfs samples (`idle-1081762707738047/`, final MCP record above) show no observed idle regression, not a sustained-animation or redraw performance claim. The known sustained-output CPU-efficiency work remains open. Explicit repository-scope Git lookup is outside UI/global locks but currently has no subprocess duration/output budget. Durable JSON transactions rewrite the bounded store (128 MiB); receipt growth and large-store throughput are not benchmarked. These limits are not disguised as universal bounded-latency guarantees.

Final coordinated checks: locked workspace build/tests, all-target Clippy with warnings denied and rustfmt passed; 261 tests passed, six platform/fixture tests remain explicitly ignored. Final smoke roots report no owned process survivors. Fifteen owned controller/fixture sources are retained in CRC-verified `controllers.zip` and `controllers-manifest.json`; temporary source files were removed after archival. Builds use eight jobs on CPUs 0–7, runtime fixtures CPUs 8–15; idle measurements ran without overlapping builds or native workload fixtures.

### Omp shared MCP cutover

Omp now loads the same MCP server/tool implementation as the other harnesses instead of registering tools through its extension. The private launch package contains a unique `bkpr_<launch>` server configuration and `bkpr-control` communication skill. Credentials are explicit server-child environment entries, not model arguments; this also preserves the extension's credential scrubbing before other extensions/children start. Native skill-selection/disable flags remain authoritative. No persistent harness settings change.

The extension retains only native binding, receiver lifecycle and generation-guarded step-boundary asides. The Omp-specific schema conversion, tool registration and `tools.json` asset are removed. Shared MCP still exposes the seven implemented communication tools, not main's full workspace/pane-control surface; the skill explicitly documents that limit. No comparative MCP/native-tool latency improvement is claimed.

Final optimized client SHA-256: `6390c2a7db7200bcbca7334ce3a28b867661c044c9f9cf496ce7185bcf4ad4e4`; server unchanged. `target/smoke/omp-mcp/acceptance-summary.json` and `native-1099030751027925/active-mcp-proof.json` record genuine Omp 18.13.2 launches through both ordinary managed shell and explicit `bkpr run omp`. Both advertise unique MCP namespaces and all seven tools; the obsolete direct routes are absent. Actual provider context reads the communication skill and shared send schema, then executes MCP discovery/send/whoami, observes an active-turn opaque aside, and executes MCP inbox read/processed acknowledgement/outbox. Wake remains truthfully unknown.

Private preexisting MCP/skill/config/model fixtures remain byte-identical. Unrelated MCP subprocesses observe no capture credentials. Fourteen loopback provider requests include two native auxiliary skill-description compression calls; zero external providers, approvals bypassed or owned process survivors. Fixture corrections distinguish those auxiliary calls and the implemented seven-tool surface from unimplemented workspace controls. Coordinated workspace tests (261 passing, six ignored), strict all-target Clippy and rustfmt pass. Canonical debug/release executables remain unchanged; controllers and check runner are archived under `target/smoke/omp-mcp/`.

## Local project metadata

The backend owns transient foreground-CWD project snapshots: canonical common Git identity, independent worktree root/Git directory/branch/HEAD, staged and unstaged file/line/binary summaries, untracked file counts and whole-tree committed HEAD-versus-merge-base totals. An unavailable explicit base remains unknown, with no fallback to guessed main; unborn and detached HEADs remain distinct. These snapshots are neither forge/CI results nor persisted recovery facts.

One off-path coordinator shares cached results across panes in the same canonical worktree. Maintained `notify` and `ignore` supply native filesystem events and ignore-aware watch planning, including tracked parents, new directories, Git metadata, configuration/excludes files and repository initialization. Bounded/coalesced invalidations publish loading/error state; covered results are never represented as ready after known watch loss. Input/render/control paths do not spawn Git. Shared authenticated `whoami` and detailed discovery consume these same facts; repository scope now requires ready cached common-Git identity instead of request-time Git discovery.

TUI headers/sidebar annotate project/worktree/branch without changing execution membership or names. **Ctrl-b, then i** opens focused project details; arrows scroll wrapped content, Enter/Esc return focus. Details distinguish current values, stale values, unavailable bases and errors.

Git commands have three-second child and ten-second ingestion deadlines, bounded input and 16 MiB per output stream, process-group kill/reap on timeout/cancellation and sanitized `GIT_*` overrides. Fsmonitor, external diff and textconv are disabled. Preflight rejects tracked executable clean/process filters rather than calculating semantically different raw counts; even clean filtered worktrees can report this limitation. This is not a sandbox against hostile concurrent configuration/attribute changes. Watch planning admits at most 32,768 resources per project and 65,536 native watches daemon-wide; failures are explicit errors with bounded repair backoff, not a normal recurring Git poll. Filesystem syscalls themselves are not preemptible by the cooperative walk deadline. One serial worker means slow repositories can delay other metadata collection, not application input.

### Corrections found during integration

The discovery/watch-install gap now rebuilds coverage as well as statistics when new watches are installed; current counts alone cannot validate stale watch coverage. Public registry ownership is tracked separately from temporary worker references so dropping the last public handle cancels/joins the project worker before releasing ownership. Existing restart acceptance remains strict.

Git test fixtures originally used the repository as HOME; isolated pane initialization legitimately created `.bkpr-native-home`, contaminating untracked counts. Separate fixture HOME/repository directories preserve the original exact assertions. The physical slow-Git fixture initially lost Nix coreutils multicall dispatch by resolving the `sleep` symlink to `coreutils`; it now preserves the executable's basename. Its output oracle also crops the application pane before checking a standalone output line, rather than including adjacent sidebar text. These fixture corrections do not change product input or Git counts.

### Final release acceptance

Evidence root: `target/smoke/projects/`. Optimized client SHA-256 `d49e02135b739c1bdf8aff9883502873debc2031c13af8a1444ab4840f533bba`; server `cad146d8908b16966e931a3e9dfba28a8d495800f5852e2b5b46fee85bcedbf2`. Coordinated locked workspace build/tests, strict all-target Clippy and formatting pass: **275 tests passed, six ignored**. Canonical public debug/release executables remain unchanged.

| Exercised behavior | Final evidence |
|---|---|
| Real main/feature/linked worktrees, shared common identity and independent HEADs; staged/unstaged/untracked watch updates; explicit unavailable base; commits, detach, init/unborn; rapid foreground CWD convergence; executable filter rejected without running its marker | `git-1102339436168798/result.json` |
| Actual authenticated synthetic native launches: identity/detailed discovery match backend project snapshots, compact discovery excludes details, canonical repository scope | `agent-project-1102339482083534/result.json` |
| Actual Kitty focused project details and completed physical shell output while a real Git subprocess remains blocked; screenshots visually inspected | `physical-1102339575495339/` |
| Eight idle panes sharing one real repository, matched previous/new release without overlapping compilation or workload fixtures | `idle-1102369977535817/result.json` |

| Five-second idle sample | Previous release | Project release |
|---|---:|---:|
| Daemon CPU seconds | 0.31 | 0.30 |
| Reaped child CPU seconds | 0.00 | 0.00 |
| Daemon CPU cores | 0.062 | 0.060 |
| Daemon RSS KiB | 30,024 | 33,484 |

These are single short procfs samples after warmup, not a universal speedup or sustained-redraw claim. The physical controller's output timing includes deliberate per-key driver spacing and is not application input latency. Final runtime cleanup records have no owned survivors; controller sources are archived with retained evidence. No external providers, live user sessions or persistent user configuration participated. Forge/PR/CI and recovery remain separate pending work.

## Usable TUI startup and pane lifecycle

Main-branch parity remains the goal, ordered by daily TUI usability rather than a metadata checklist. This slice addresses normal launch, unavailable executions, explicit shell replacement and restart; it does not implement full harness recovery or move remote endpoints ahead of local TUI workflows.

A new daemon with explicit current bootstrap argv creates a uniquely named fresh workspace and usable execution when no live running executions remain, even with retained metadata. Historical layouts/records are preserved, not replayed or overwritten. Headless startup without argv does not execute anything. Attaching to an existing daemon does not bootstrap another shell; initial client selection prefers a live projected execution while explicit historical selection remains possible.

**Ctrl-b, then uppercase R** confirms opening a new configured shell in an unavailable pane's selected workspace slot. The confirmation makes directory choice explicit; it never silently falls back or resumes the old program. `pane_reopen` validates the old execution, selected leaf and workspace revision, reserves the target workspace, spawns outside registry locks, and atomically commits a fresh pane ID through the existing spawn pipeline. Failure retains the old layout and cleans up the newly owned child. Other workspace projections and the old historical record remain unchanged. Live execution and read-only client requests are rejected.

Graceful shutdown now closes admission before teardown and preserves acknowledged layout intent while stopping owned executions. Natural pane exit and explicit termination still remove their projections. Integration runtime found that the previous normal completion path erased layouts during shutdown itself; this was fixed in production, not hidden by changing the fixture to crash-only shutdown.

Final optimized client SHA-256 `d1ede47ff26ca1be01ff67ea3d38ae194747998868d60c17455a5a964e2654d6`; server `f9eaecd22c6b1b52f7d01b1f5ded9e7dfcdc517d5c9952714fc470d7e31a37fd`. Locked coordinated workspace build/tests, strict all-target Clippy and formatting pass: **295 tests passed, six ignored**. Four real-daemon startup integration tests include graceful layout preservation and fresh-ID reopening. Canonical public binaries remain unchanged.

Evidence under `target/smoke/lifecycle/`:

- `api-1104214537157914/`: actual headless fresh/restart does not spawn; live/stale/invalid-directory requests reject without changing old layouts; successful replacement changes only the selected workspace, retains history and uses a fresh execution ID.
- `physical-1104214530794830/`: actual Kitty startup selects a usable shell alongside retained history; replacement confirmation cancels without mutation, explicit confirmation accepts shell input, read-only reopening is denied, detach/reattach preserves executions, natural shell exit removes its layout slot, and the TUI itself automatically starts a usable daemon from retained state. A persisted marker proves the old startup command was never replayed. Screenshots were visually inspected.

All final owned-runtime cleanup records report no survivors. Controller sources are archived with retained evidence; no user sessions, persistent settings, real providers or native approvals participated. No new latency/CPU improvement is claimed: the changed lifecycle path was exercised through actual input/output, not inferred from tests or queue acceptance. Full harness resume, retained terminal contents and remaining core TUI workflows remain separate work.

## Per-view TUI scrollback

This bounded step adds history navigation only, not selection, copying, search or persisted transcripts. Shift+PageUp and Ctrl-b s enter history; arrows move by row, PageUp/PageDown by page, Home reaches oldest retained history, and Esc/Enter/End return live. Explicit history mode blocks new application typing and paste while preserving already-accepted releases and transfer FIFO. Read-only views can inspect history.

That initial blocking policy is superseded by
[typing and manual-bottom history exit](#typing-and-manual-bottom-history-exit)
below; the original smoke remains historical evidence, not the current input contract.

Native tracked-grid references anchor each inspecting view across new output, pruning and resize/reflow without copying the terminal's retained buffer. Other views stay live and independent. The header/footer expose the authoritative offset and native retained-row total (including the visible grid); historical cursors are hidden. View retirement/detach/workspace switching ends that inspection, so a new attachment starts live. Native scrollback remains bounded by the configured 10,000-line/16 MiB page-granular retention, not an exact durable line-count guarantee.

Ordinary vertical wheel events navigate primary-screen history only with confirmed native mouse-mode capability and reporting disabled. Mouse-aware applications retain native wheel input; Shift+wheel is the explicit history override. Unknown mode capability leaves ordinary application routing unchanged. Alternate-screen history is explicitly unavailable instead of exposing the primary buffer behind the application.

History capture reuses native rendering and weak image receipts shared by actual scene-owned `Arc` resources. Cache entries do not extend decoded-pixel lifetime beyond real scenes; expired weak entries prune each capture. This avoids retaining an extra copy of the native 64 MiB graphics store. Native regressions require identical image Arcs and placements across repeated live/history viewports and verify final scene-drop release.

Integration caught a zero-offset history shortcut incorrectly using the immutable published hold snapshot instead of current raw native inspection. It was fixed without changing the existing held-render regression. The physical fixture also needed to raise the intended X11 window: keyboard focus alone left an overlapping second client receiving the wheel. Early occluded black screenshots are not presentation evidence; final captures expose the window and check populated pane rows before visual inspection.

Final release client SHA-256 `54c89495f30b8612748138d47b223eac9d22ccad56b9f19141f7ece3e3c09c83`; server `8f1a4161f0887ad7833d7b730a7f583e451443be167ae570e11813d98b67ed2a`. Coordinated locked build/tests, strict all-target Clippy and formatting pass: **310 tests passed, six ignored**. Canonical public executables remain unchanged.

`target/smoke/scrollback/physical-1106457003783737/` records real Kitty clients against a raw-terminal fixture that logs every application-input byte. One client remains anchored on older numbered rows while another displays new output. History typing/navigation emits no application bytes; native mouse-aware wheel input is exactly `ESC [ <64;21;16M`, while Shift+wheel emits no application input. Read-only oldest-history navigation and explicit alternate-screen refusal pass. Exposed screenshots of full history/live bodies and the alternate-screen boundary were visually inspected.

`images-1106481753134238/` compares the previous and new release with five 256×256 RGB images, two views and five live/history targets. All five complete viewport hashes (rows, ordered placements and resource-reference sets) match across releases and remain immutable across cycles. There are 65 measured samples after two warm cycles:

| Matched image-history control workload | Previous release | Scrollback release |
|---|---:|---:|
| Median control/full-frame round trip | 91.09 ms | 87.73 ms |
| Maximum control/full-frame round trip | 100.99 ms | 98.47 ms |
| Daemon RSS before samples | 16,156 KiB | 16,940 KiB |
| Daemon RSS after samples | 17,192 KiB | 17,928 KiB |

This deliberately serializes full image-bearing frames after control requests; it is not the compact TUI transport, physical input latency, allocation count or a general speedup claim. It shows no observed regression in this bounded comparison, with identical content. Compilation and other owned workload fixtures were stopped during measurement. Final owned runtimes report no survivors; controller sources and useful evidence are retained in the smoke archive. No live sessions, real providers or persistent user configuration were used.

## Per-view terminal selection and copy

Selection/copy reuses the pinned SDK's native gestures, word/line expansion, tracked endpoints and explicit formatter. No second terminal parser, generic selection algorithm or additional runtime dependency was introduced. The existing framework applies borrowed row spans at composition time; canonical frames, compact/delta baselines and image resources are not mutated to highlight a selection.

Ordinary left selection respects confirmed native application mouse ownership; Shift explicitly overrides only when the host forwards it. Alt selects rectangles. Completed selections and `Ctrl-b y` use existing plain-text clipboard framing, with client deny, host permissions, bounded extraction and origin/generation/focus guards. Read-only inspection may select/copy without acquiring PTY input or application clipboard grants. Native edge ticks run only for an active captured gesture and move that view's history, not another client's viewport.

### Corrections found during integration

A physical click exposed a real authority race: focusing a pane refreshes its sizing lease, so a queued selection stamped with sizing activity could be rejected and stop the input lane. A separate visibility epoch now survives same-visible sizing/capability updates while hide/show/detach still invalidate stale work. Known inspection rejection reports a non-stopping notice; unknown transport retains the existing stop/no-replay boundary. Actual-worker and client transport regressions cover both paths.

The new in-process core fixture initially enabled dedicated single-pane ownership. Its cleanup adopted other parallel tests' children, causing premature pane exits, missing project state and `ECHILD`. The fixture now uses the shared-process mode, matching existing lifecycle tests; production reaping was not weakened. The full parallel workspace suite passes with that correction.

Fixture corrections also kept native semantics intact. The retained fork's cell selection is half-open, and the SDK's exact half-cell boundary similarly excludes the endpoint cell; an end-to-end native coordinate probe established the correct mouse endpoint instead of replacing library behavior. The direct-terminal pixel oracle continues to require exactly the desired selected characters. X11 keyboard focus alone did not expose an occluded window; raising the intended window fixed event delivery after the third client opened. No product workaround or host-setting rewrite was added for either fixture issue.

### Exercised release acceptance

Evidence is worktree-local `target/smoke/selection/`. The previous release fails to produce selection after a physical drag in `physical-1121273027795832/`, after both complete unselected TUI bodies match the direct-terminal reference. Final release evidence is `physical-1123471623181119/`: the complete 684×612 pane body matches an independently rendered direct Kitty reference before and during selection, while the other client remains unselected. Actual isolated X11 clipboard bytes verify drag/copy-again, word/line/Unicode/rectangle selection, read-only copy, the complete native application mouse press/motion/release versus forwarded Shift override, selected history across output, active edge autoscroll, deny policy, cross-pane capture and retirement without stale export or redirected release.

Default Kitty reserves Shift+left for its own selection. Only the owned fixture processes remove those Shift mouse mappings to exercise forwarded override events; ordinary selection, clipboard permissions and persistent host configuration are unchanged. This does not claim that Bkpr can receive a host-consumed gesture.

`api-1122767938040172/result.json` records actual native extraction, view isolation, output/resize anchors, hide/detach denial, per-view autoscroll and metadata-only dragging. The bounded 60×20 native-pane control workload uses 57 selection-motion samples with no compilation or other owned workload running:

| Measure | Observed |
| --- | ---: |
| Median Select RPC round trip | 0.034907 ms |
| P95 Select RPC round trip | 0.054375 ms |
| Largest unchanged-scene poll response | 1,354 bytes |
| Motion samples replacing/advancing the canonical frame | 0 |

These are local control timings, not physical presentation latency, a large-editor benchmark or a sustained-animation CPU improvement. The separately tracked CPU-efficiency work remains open.

Final optimized client SHA-256: `0ed8fdf3434030bad304f9e0eba7d561f4e6da072813b94a4e3fa5e50072b0e9`; server: `9d4fdfdff8ddee2c8f8501815d52c012c0349f2b585f942afebea64a761cfdbd`. Locked workspace tests pass: 337 tests, six explicitly ignored subprocess fixtures. Strict all-target Clippy passes. Native SDK/revision and dependency patches are unchanged. Useful controllers and provenance are archived alongside the evidence; owned process cleanup reports no survivors. No live sessions, provider calls, user profiles or persistent third-party settings participated.

## Tiled-pane resizing

Keyboard mode (`Ctrl-b e`, arrows, Shift-arrows, Enter/Esc) and physical left-drag dividers now resize the existing backend layout. One shared ratio partition drives pane geometry, real one-cell gutters, hit testing and pointer/keyboard conversion. Ratatui's `kasuari` constraint layout and Taffy's CSS layout were inspected before implementation; neither removes the view/capture/revision work without adding another layout model. Main `9ad93bd` delegates native resizing; fork `d7b40a37`'s native grid/stack solver isn't copied. The existing termwiz decoder, retained framework, bounded jobs and `LayoutUpdate` supply the infrastructure. No dependency, SDK/native patch or server change was required.

Only one exact-revision update may be in flight. Raw motion retains a single latest coordinate pair; the next layout is materialized after admission or acknowledgement, not cloned per superseded motion. Release retains its final point. Own publication before reply is valid; another revision/layout in the same workspace retires the gesture. Unrelated workspaces don't cancel it. Rejection, inconsistent success and lost response never rebase or replay pending intent. Focus, workspace, modal and terminal-geometry changes consume the gesture's tail without redirecting it into application input. Stopping doesn't roll back an accepted update.

### Actual release acceptance

Worktree-local evidence is `target/smoke/resizing/`. Baseline `physical-1219360391273187/` shows the prior optimized release leaves ratios unchanged and routes the would-be resize arrow to the native application. Final `physical-1219971745143659/` runs actual Kitty/Xvfb windows with XTest keyboard/pointer events and raw native C fixtures. Fixtures enable application mouse reporting, log exact received bytes and record their actual PTY dimensions after `SIGWINCH`.

The nested three-pane layout exercises horizontal and vertical keyboard movement, physical column/row dragging, read-only observation, minimum-column clamps, no-op boundary movement, focus/modal/terminal-size retirement, foreign-layout rejection, unrelated-workspace progress and ordinary typing after completion. Every pane body at five checkpoints matches an independent direct-Kitty fixture's complete pixel reference, not a blank/first-glyph readiness check. All control/drag stages preserve application input bytes; typing resumes afterward.

| Burst/checkpoint measure | Observed |
| --- | ---: |
| Injected motion samples in one terminal batch | 80 |
| Committed workspace revisions for that burst | 2 |
| Complete pane bodies matching direct-terminal references | 15 |

The burst uses owned Kitty's byte-transparent `send-text` to exercise one input batch; physical XTest dragging is verified separately. Counts establish bounded/coalesced mutation behavior, not input-to-presentation latency, a full-screen editor comparison or improvement to the deferred sustained-animation CPU problem. Tiny windows can collapse a second subtree; projection never rewrites the stored ratio. Beyond 1,000 usable cells on an axis, thousandths ratios can quantize movement to larger cell steps.

Locked workspace tests pass: 355 tests, six explicitly ignored subprocess fixtures. Strict all-target Clippy and formatting pass. Optimized client SHA-256: `3ef4ea69bf4b9ffca58b8f6c71f7a431c3b0e2aaf7ae8b325c6c38d57ecc1e66`; server: `9d4fdfdff8ddee2c8f8501815d52c012c0349f2b585f942afebea64a761cfdbd` (unchanged). Builds use the local target with eight jobs on CPUs 0–7; the actual-terminal fixture uses CPUs 8–15, with no compilation in flight. Controllers/provenance are archived beside the evidence. Owned cleanup reports no survivors; no live sessions, model/provider services, real clipboard, user profiles or persistent host settings participated.

## Per-view native terminal search

`Ctrl-b /` now opens a bottom-row literal-search editor; Enter/Shift-Enter navigate older/newer matches and Esc clears without application input. Read-only views are supported. The pinned SDK supplies native incremental search, counts, match selection, reflow/retention and per-screen state. Bkpr binds it to visible inspection epochs, bounded pane-worker scheduling, query acknowledgements/generations and borrowed highlight spans. No full-history strings, competing parser, blocking SDK `run`, new runtime dependency or native/SDK patch is introduced.

The dependency/source audit is in `target/smoke/search/source-review.json` and the implementation plan. Native navigation was checked in retained source at revision `93967b7`: it feeds and selects, not an all-history run loop. Safe `Search::snapshot` feeds before borrowing matches; the native active rescan preserves prior progress/completion state. Snapshotting can add an extra active-area scan in a progress turn; no universal search-cost or presentation-latency claim is made.

### Corrections and acceptance

The initial engine test assumed the newest history marker was outside the live grid. It wasn't; native `IfNeeded` correctly did not scroll. Nonmatching tail rows now make the intended history case real. Previous-match navigation need not recenter an already-visible match. These fixture corrections preserve native semantics and keep assertions for real history navigation, view isolation and unchanged canonical scenes.

Actual resizing exposed a product issue: native reflow discarded the chosen match while the query/count survived. Bounded progress now selects an actual newest available match when native selection is missing, without inventing an index or scanning every result in Rust. A regression exercises repeated resize/reflow, selected spans and independent views.

Baseline `physical-1224533976403086/` shows the previous release sends would-be query text to the raw fixture. Final optimized evidence is `physical-1226829354243781/`: real Kitty/Xvfb keyboard input, independent read-only/client views, Unicode, ASCII case folding, no-match, 400 retained history markers, next/previous, explicit live return and exact application-input logs. The full 684×612 body matches an independent direct-Kitty reference before search and with the chosen match's native bold/underline/inverse styling; the other client stays plain. Resize away/back restores the same complete chosen body. New output updates the actual count from 400 to 440 while old anchored rows retain exact pixels and the complete visible body shows the newly written rows.

The offline text oracle strips SGR before pyte because its colon-SGR handling otherwise fabricates literal glyphs. Unmodified bytes still go to the actual Kitty, and pixel checks remain independent. Anchoring preserves logical top rows, not a frozen snapshot: writes to still-visible live rows legitimately appear. End is only a local live-return key while inspecting history; sending it in live mode correctly belongs to the application.

### Dense-result resource observation

`api-1226835421162477/result.json` uses 1,000 rows of 76 repeated characters, three independent read-only views and 20 replacement queries, with no compilation or other owned workload running. Native search finds exactly 76,006 matches per view including fixture live text.

| Native searches | Daemon RSS (bytes) |
| --- | ---: |
| None, before query | 13,996,032 |
| One dense search | 46,469,120 |
| Three dense searches | 109,912,064 |
| All cleared, after replacements | 81,395,712 |

The SDK retains flattened result endpoints for all matches and has no configurable result-memory cap. Bkpr materializes only viewport-page matches for clipped spans; it doesn't copy all matches into another search engine. Existing history/view admission bounds are not a separate byte-hard search allocation cap. Dense multi-view memory and post-clear RSS need allocation/lifetime profiling; this observation alone does not diagnose a leak. Exact results/counts are retained rather than silently capped. `TODO.md` tracks the optimization and required discussion before changing supported semantics.

Raw control-completion timings are retained in the JSON, including polling/serialization overhead and an 80 ms sampling interval; they are not physical presentation latency. No editor/animation CPU improvement is claimed.

Final locked workspace tests: 372 passed, six explicitly ignored subprocess fixtures. Strict all-target Clippy and formatting pass. Optimized client SHA-256: `a4e6c16c7b603406d993789043594376e89d88c18e5d0726a670baaeebb3fe18`; server: `806f56e7e3ba3b855339ba38db4e99fd649cd03e2dd13de14688403ecc548f82`. Builds use local Cargo/native artifacts, eight jobs on CPUs 0–7; runtime fixtures use CPUs 8–15. Controllers and provenance are archived beside the evidence. Owned cleanup reports no survivors. No user sessions, persistent host/profile settings, real clipboard or model/provider services participate.

## Directional pane rearrangement

`Ctrl-b Shift+Arrow` now exchanges the focused leaf with its nearest-center directional neighbor using the existing off-path, revision-checked layout API. The same execution remains focused; split axes/ratios and unrelated leaves are unchanged. No backend/protocol change or runtime dependency is required. Read-only clients are denied, missing neighbors are explicit no-ops, and another move during pending workspace management is rejected rather than queued or rebased.

Evidence is under `target/smoke/rearrangement/`:

* `physical-1234036873629974/` demonstrates the old optimized binary only changes focus: Shift+Right leaves the layout unchanged and subsequent `q` reaches GAMMA instead of ALPHA.
* `physical-1234179917013905/` exercises the new binary through real Kitty/Xvfb and physical XTest keys: all four directions, nested splits, inverse moves, boundary/no revision change, read-only denial, shared layout publication, peer detach and continued typing into the original focused execution.
* Twenty-two exchanges include ten repeated moves at 100×36 and eight at 200×60. Three raw native applications repaint their entire cell grids with row-dependent SGR colors after each WINCH. The controller verifies every visible body cell and saves actual screenshots; the original application PIDs and first client's view IDs remain unchanged, and no move/denial key reaches an application.
* `result.json` retains controller-key-through-complete-surface samples, including XTest delays, RPC/text analysis and screenshot work. They are not physical presentation latency or an editor/sustained-animation performance comparison. The existing deferred sustained-output CPU work remains unresolved.

The first baseline controller expected focused-pane identity in the ordinary footer; it is shown in pane chrome instead. The corrected baseline checks actual application delivery. No product change was made to satisfy that mistaken oracle.

Verification: 120 client tests and 373 workspace tests pass, with six explicitly ignored subprocess fixtures; locked release workspace build, strict all-target Clippy and workspace formatting check pass. The new regression verifies nested exchanges preserve exact projected rectangles and unrelated panes. Optimized client SHA-256: `d0e95a14d05db160067f4a65d43f28d028cc80519cc2f97dc83fcdd909c1d05d`; unchanged server: `806f56e7e3ba3b855339ba38db4e99fd649cd03e2dd13de14688403ecc548f82`. Known-good before/after binaries and archived controllers are retained beside the evidence. Cargo uses this worktree's native prefix/target, eight jobs on CPUs 0–7; Kitty fixtures use CPUs 8–15. Cleanup records contain no survivors. No live sessions, user profiles, provider services, persistent settings or real clipboard participate.

## Named sessions and client lifetimes

The launcher restores `--session/-s` (default `bkpr`), one-shot `--detach-others/-d`, read-only `list` and targeted `stop`. Exact names select independent daemon/state roots; persistent logical UUIDs are distinct from fresh daemon incarnations. Native pane exec receives trusted session ID/name snapshots. Bootstrap belongs to the launcher, not the client library, and a live identity/protocol error never triggers an automatic restart.

Managed frontends register a process-bound ephemeral capability on a dedicated lifetime stream, prepare their terminal/runtime state, then activate once before creating views. Normal requests carry that authority; headless admin requests cannot adopt managed views implicitly. Takeover/EOF/owner exit revoke the capability and its views, queued callbacks, device grants and sizing leases without terminating programs or waiting for an old client acknowledgement. Authentication retains kernel owner identity instead of scanning processes on each key.

### Runtime acceptance

Evidence is under `target/smoke/sessions/`:

* `api-1238916549271360/` uses real daemons and native raw PTY applications: independent exact session identities/environment, registration without activation, failed identity handshake, exact takeover, repeat-activation idempotence, stale/bare/wrong-process credential rejection, forbidden managed shutdown/nesting, EOF view retirement and original-owner exit despite a fork-inherited lifetime socket. Surviving clients and programs remain usable; stopping one daemon does not stop another.
* `physical-1238916580875331/` uses actual isolated Kitty/Xvfb, physical XTest keys and the real CLI bootstrap/list/stop paths. Unicode/slash names and default `bkpr` ignore an unrelated inherited socket for normal attachment. Ordinary attachments share the same program without spawning another shell. Read-only empty discovery creates no state.
* With A1's exact owned client process SIGSTOPped, A3 takes over via `-d`, becomes visible and types into the original pane while A2 exits normally. A1 requires no acknowledgement; after SIGCONT it exits and restores the real host TTY. Both native termios equality and the returned host screen are checked. Original pane/application PID and actual PTY `TIOCGWINSZ` dimensions are retained/verified, and session B continues accepting independent input.
* A later ordinary A4 attachment survives the earlier takeover and A3's normal detach. Targeted stop affects A only. Restart keeps A's logical UUID, changes its daemon UUID and starts the explicit fresh A5 command rather than replaying historical argv.

The single observed A3 launch-through-visible-marker interval is 196.5 ms, including Kitty/Python/client startup, X11 actions and controller sampling. It is not backend takeover latency, physical presentation latency or a sustained-rendering benchmark.

### Shutdown correction

The actual terminal fixture exposed `abcdef` becoming `abcdef\n\x04` during stop. Failing-before evidence is `physical-1237515932644782/fixture-inputs.json`. Maintained `portable-pty` 0.9.0's `UnixMasterWriter::drop` explicitly writes newline and the configured VEOF byte. The explicit termination branch stopped its writer before killing the child, allowing those bytes into a live raw application or pending shell command.

Explicit termination now signals/reaps the owned process tree before stopping the writer, matching the common cleanup order. The Linux regression `explicit_termination_does_not_type_eof_into_a_live_raw_child` uses a real raw `cat` that ignores HUP; native grace/forced-kill behavior must complete without adding input. The final physical fixture preserves exactly `abcdef` through stop/restart. No input filtering, special-case byte suppression or dependency fork is introduced.

The first smoke decoder incorrectly capped full verbose inspection JSON at 1 MiB; its corrected bounded, buffered short-request reader leaves the lifetime handshake/event reader exact and unbuffered. Initial launcher/core identity fixtures also assumed `tempfile` directories were private; explicit 0700 roots and bounded fake-listener deadlines correct the fixtures, not production permission checks.

Verified checks for this fixed backend: 131 core tests pass with five explicitly ignored subprocess fixtures; all seven launcher session tests pass. The immutable exercised pair is `after-stop-fix/bkpr` SHA-256 `89001401eebe2f616b7fd9f31b1f00aa6424e1d5355fc168a4bd7909b58f9328` and `after-stop-fix/bkprd` `3f2eb693f32e7bc08ce5557286b94e300559ba6ec9c1334af1e6e21c0048018c`. It deliberately isolates this backend fix from the separately progressing frontend chrome batch.

Compilation uses local incremental artifacts, eight jobs on CPUs 0–7; owned runtime fixtures use CPUs 8–15. Both cleanup reports contain no survivors. No live user sessions, profiles, real clipboard or model/provider services participate. Runtime proof is Linux-only; macOS owner-observer/package acceptance is not claimed. Cross-daemon sidebar/agent aggregation, separate terminal windows within a workspace, owned-pane cascading workspace close and optional creation naming remain distinct tracked work, not implemented by this session slice.

### Review corrections and final integrated acceptance

Independent backend review found a second sizing path: view expiration captured a bare fallback size and bypassed lease-aware reconciliation. Expiry now uses the same guarded resize path. Captured authority generation is checked as well as client liveness, so retiring a view without ending its client cannot restore that view's stale size. A native-actor barrier regression covers both takeover and view detach during expiration-driven fallback. The earlier resize regression now acknowledges a native turn without attaching a fresh sizing view that could hide a transient stale resize.

Launcher review also found that forwarding `--session -work` made the daemon parse a valid name as an option. Failing-before `physical-1242344435395133/daemon-logs.json` records `unexpected argument '-w'`. Bootstrap now forwards one `--session=<value>` argument. The final real-CLI acceptance creates, lists and stops both `-work` and `--`, alongside Unicode/slash names and default `bkpr`. The default runtime fallback resolves the trusted system `/tmp` base before adding owned components, handling macOS's system alias without relaxing owned-descendant symlink checks. A real filesystem alias regression exercises that boundary; no macOS runtime claim is added.

Final integrated evidence is `api-1242785272769591/` and `physical-1242785238906312/`. It repeats the process/view/grant isolation, five named-session roots, suspended-client takeover, native PTY dimensions, actual host-screen/termios restoration, later normal attachment, targeted stop and fresh-command restart against the coordinated frontend/framework build. The observed launch-through-visible-marker sample is 196.8 ms with the same non-latency limits described above. Both owned cleanup reports have no survivors.

The exercised immutable pair is `target/smoke/sidebar/final-framework/`: client SHA-256 `9fb7f1108ea615584b2494e0fd65b7b602506e9d5bdc11b2df1b7275f6bbda68`, server `a1f4d56296e84f26f3f58502d8936cbbd45344eb17bdd5e1372decc736ff2e5b`. Coordinated locked workspace verification passes 453 tests with eight explicitly ignored subprocess fixtures; strict all-target Clippy, workspace formatting and the release workspace build pass. The final core count is 132 passing tests. Earlier isolated-backend binaries/evidence remain retained for the failing-before comparison; final controllers and provenance are archived under `target/smoke/sessions/`.

## Bkpr product frontend

This slice restores Bkpr's own main-branch product layer, not general Zellij parity. Reusable retained surfaces, editing, windows, capability probes and generic SVG preparation live in `bkpr-tui-framework`; Bkpr navigation, metadata semantics, bindings and artwork selection live in `bkpr-client`. Source revisions, maintained-dependency comparisons and asset notices are recorded in the [implementation plan](implementation-plan.md#bkpr-frontend-ownership-and-reuse).

### Actual terminal acceptance

Evidence is under `target/smoke/sidebar/`, using isolated HOME/XDG, private daemon sockets, native raw-grid applications and owned Kitty/Xvfb or xterm:

* `physical-1242531997296624/`: all-workspace keyboard navigation, fold/unfold, scrolling, activation and cancellation preserve exact native input ownership. Browsing metadata admits no hidden views and changes no layouts.
* `physical-1242541175061145/`: actual rename cursor edits/cancel/commit, editable submission errors, explicit workspace name/directory creation and native cwd/input; fuzzy/field-filter finder, compact Tab previews and actual pane-ID activation; contextual guides, narrow/wide clipping and passive popup pointer safety. Private clipboard paste through actual Kitty `Ctrl-Shift-v` reaches rename, creation and finder exactly once, without native input leakage. Host paste framing follows the owning UI/native context without interrupting admitted transfers.
* The same run physically double-clicks workspace folding and unfolding. It projects pane 10 into workspace 11 without changing membership, waits for both native grids and the real ratio-400 divider, then drags through the popup covering that divider. Layout/revision, view IDs, input logs and borrowed-pane ownership remain unchanged; closing UI ownership restores typing into pane 11.
* `physical-1242571341218707/`: actual xterm enters the sidebar with `Ctrl-Space`, preserves native input, shows readable `[]`/`<>` markers and presents contextual hints. Unsupported-host behavior is not inferred from a fabricated capability reply.
* Kitty full screenshots and image-slot crops show static hive/bee artwork, with 19/23 sampled colors. Explicit APC query evidence and observed client colors gate these decorations; the existing Scene renderer owns resources, clipping and upload deduplication. No extra output loop or decorative refresh timer is added.

The original xterm failure is retained at `physical-1240737337658698/`: advertised `Ctrl-Space` did not enter the sidebar and subsequent `j` reached the native program. The failing decoder-to-App regression observed Termwiz's legacy NUL as `Unidentified` with no modifiers. Framework legacy-event normalization now maps NUL to Ctrl-Space without changing Kitty functional-key placeholders. The final xterm run confirms the actual changed path.

Other failing-before/passing-after regressions cover modal targets being redirected by registry-driven focus/workspace replacement, and cached popovers retaining old harness facts when a held snapshot changes identity without a registry revision. Popovers and finder now share the existing foreground-identity guard and invalidate metadata on relevant facts, not every terminal-output revision. Project details reuse the inspector's loading/error/stale presentation; retained finder project/branch values are labeled stale.

### Reproduction and limits

The final immutable pair is `final-framework/bkpr` SHA-256 `9fb7f1108ea615584b2494e0fd65b7b602506e9d5bdc11b2df1b7275f6bbda68` and `final-framework/bkprd` `a1f4d56296e84f26f3f58502d8936cbbd45344eb17bdd5e1372decc736ff2e5b`. Rust 1.96.1, Kitty 0.49.2 and xterm 411 are exercised on Linux; builds use this worktree's target/native prefix, eight jobs on CPUs 0–7, and fixtures run on CPUs 8–15. Workspace tests pass: 453 tests plus eight explicitly ignored subprocess fixtures. Locked release workspace build, all-target strict Clippy and formatting check pass. `checks.json` retains the toolchain/counts/hashes; `controllers.zip` retains the throwaway controller sources after cleanup.

All final fixture cleanup reports contain no survivors. No live user sessions, profiles, real desktop clipboard or model/provider services participate. The workload uses 13 initial workspaces, styled full-grid native redraws, repeated navigation, narrow/wide resizes and a real shared-pane split. It is not a sustained-editor/animation CPU comparison, cold/warm startup benchmark or physical-presentation latency measurement; those existing performance gates remain unresolved.

Static icons and text windows do not establish general terminal-image initial-presentation acceptance. Pixel recessed/raised atlases, animation, remote aggregation and forge/CI facts remain separate work. Separate terminal windows, owned-pane cascading workspace close (including shared projections), and atomic blank-name creation still require backend contracts; no split, layout-only removal or racy frontend naming substitutes for them.


## Layout-aware control keys

The Kitty decoder incorrectly selected the alternate PC-101 key instead of the primary active-layout key. Ordinary text followed its separate text field; Ctrl events with an explicitly empty associated-text field exposed the wrong identity. The decoder-to-native-encoder regression reproduced Ctrl-W as byte 26 instead of 23 before the fix. Primary layout identity now supplies both shortcut identity and the unshifted codepoint; the alternate field remains parsed but cannot replace it. Functional keys, shifted text and releases retain their existing paths. This follows Kitty's [alternate-key semantics](https://sw.kovidgoyal.net/kitty/keyboard-protocol/#key-codes).

`target/smoke/azerty/physical-1248510252070805/` records an actual isolated Kitty/Xvfb with French XKB layout, physical XTest keys and a native raw-PTY application. Ordinary w/z/a/q and Ctrl-W/Z/A/Q produce exactly `77 17 7a 1a 61 01 71 11`; screenshots were observed and exact-birth cleanup has no survivors. The permanent regression covers decoder-to-SDK control-byte conversion and unchanged text. Locked workspace tests, strict all-target Clippy, formatting and release-client build pass. No live client keyboard map, user session, profile or model/provider service participated.

## Terminal-native theme

Reusable `Theme`/`Role` resolution, retained styling and capability evidence live in `bkpr-tui-framework`; Bkpr assigns component/activity roles in `bkpr-client`. The [source ledger](implementation-plan.md#terminal-native-theme) records main/fork provenance and the dependency decision. Canonical application frames bypass theme application.

### Actual terminal acceptance

Evidence is under `target/smoke/theme/`, with private HOME/XDG, sockets, native row-dependent colored grids, physical XTest input and owned Kitty/Xvfb or xterm:

* `physical-1251134612967108/` and `physical-1251134594159980/`: dark/light sidebar selection, passive metadata, focused forms, validation errors, finder and command guide. Actual host palette changes re-query colors and present the new derived sidebar background while `THEME_PRESERVED` remains in the focused field.
* `physical-1251297214532531/`: the same flow with a chromatic background. `physical-1251297159107045/` repeats it at 240×72 with 13 workspaces and a 32-action navigation burst. Every final color case emits zero terminal bytes during its 1.1-second idle measurement.
* `physical-1251377019186238/`: xterm supplies foreground/background/palette replies while `allowTcapOps: false` leaves truecolor unconfirmed. `physical-1251377789576472/` also sets `allowColorOps: false`. Both retain readable native/ANSI chrome, text artwork markers and exact input ownership, without RGB or Kitty image output. Unrestricted xterm 411 does report truecolor; terminal names are not capability evidence.
* `matched-native-colors.json` compares immutable full native-body references against the untinted release, excluding headers, footer and real terminal borders. Dark/light/chromatic and large Kitty bodies match pixel-for-pixel. Xterm comparisons retain a one-pixel difference, so exact pixel identity is not claimed there. Native text is unchanged; metadata browsing admits no hidden views and changes no layouts.
* Real DECRQM probes in `physical-1251722591986582/` preserve an originally enabled mode 2031 and native termios through normal detach, with exit status zero. `physical-1251722657870054/` also restores an originally disabled mode and termios, but returns status one with `update lane stopped: stale view`. That snapshot does not count as a successful exit. The protocol-3 frontend acceptance below records the ordered-retirement fix and successful exits; active-generation errors are not suppressed. One untinted comparison at `physical-1251933895245134/` exits normally; that sample does not establish causality.

The first live-change smoke failed because the matcher accepted bare `CSI 997` rather than actual private `CSI ?997`. `physical-1250650494084028/real-host-input.vt` records the real `CSI ?2031;2$y` and `CSI ?997;2n` reports; the focused field remained present but no refresh followed. The corrected decoder-to-probe and App batch regressions exercise private packets at every fragmentation boundary, preserve adjacent native typing and literal paste, reject undefined modes and coalesce refresh requests. Final physical runs confirm the new derived background, not merely query submission.

### Reproduction and limits

The exercised release pair is `target/smoke/theme/final/`: client SHA-256 `f54f3b53a375a2d230cd553aa768615c8980fcf3ce0ce2b22b903c7e85be4371`, server `a1f4d56296e84f26f3f58502d8936cbbd45344eb17bdd5e1372decc736ff2e5b`. Rust 1.96.1, Kitty 0.49.2, xterm 411 and the worktree-local pinned native engine are used on Linux. Builds/tests use eight jobs/threads on CPUs 0–7; fixtures use CPUs 8–15. Locked workspace tests pass 466 tests with eight explicitly ignored subprocess fixtures; strict all-target Clippy, formatting and release workspace build pass. `native-final-checks.log`, `checks.json` and `controllers.zip` retain commands, provenance and reusable fixture sources.

All owned cleanup reports contain no survivors. No live sessions, profiles, desktop clipboard, provider services or installed programs participate. This is color/interaction/idle-output acceptance, not startup latency, sustained-editor/animation throughput, remote transport or general graphics-initial-presentation acceptance.

These checks describe the immutable protocol-2 color snapshot. The obsolete uncalled `native_colors` helper was removed after that snapshot. Protocol-3 integration and its exercised binaries are recorded separately below; this snapshot alone is not protocol-3 acceptance.

## Owned windows and backend session closure

Protocol 3 replaces the single workspace layout with owned windows and paired terminal workspace/window membership. Active-window selection is client-local. The backend rejects foreign-window/workspace layout references; closing documents acts on ownership, including unprojected terminals, rather than the client's current display.

### Native backend acceptance

Evidence is worktree-local `target/smoke/workspace-windows/`. The exercised matching release pair is `final/bkpr` SHA-256 `1f34f58059c6bc7e35b860a371312ffcbdf03ad1a8025d25e5162de872eac414` and `final/bkprd` `f38544990c8fbee1dba1eedee1f5ba92783b7a11c9afaf6cf56301f87d1b31f1`.

`api-1257835228013522/result.json` runs real private daemons, raw native PTY programs and a normal shell. It proves independent full-window creation, same-user mutation from another process through the owning endpoint, foreign layout rejection, bound terminal ownership, nonterminating layout removal, termination of unprojected owned executions and preservation of the other window's PID/input. Final-terminal closure removes only its workspace; explicit final-workspace closure and passive normal shell exit end that session without an Inspect/Wait followup. An unrelated session continues consuming input. Cleanup reports zero surviving owned processes.

Dedicated regressions reproduced and fixed three failures: a sibling normal exit deferred behind a window-close reservation could leave an empty session alive; legacy unreferenced exited receipts could become owned unavailable terminals; repeated progress could extend a blocking response write beyond its intended total deadline. Close-reservation release now drains deferred completions, migration de-owns positively identified retired receipts, and writes use per-send nonblocking I/O with an absolute readiness deadline. Final-close response draining excludes idle sockets and lifetime watches.

Locked workspace tests pass **495 tests**, with eight explicitly ignored subprocess fixtures; strict all-target Clippy, formatting and the release workspace build pass. Native permission-sensitive tests run as the actual user: remapping into a rootless user namespace correctly makes host root-owned ancestors untrusted, so production checks were not weakened. Only release building/linking uses private mount aliases, preserving the live public release pair byte-for-byte while reusing the local Cargo cache. `protected-1257773718816173/result.json`, `final/build.json` and `runtime-paths-before-host-checks.json` retain provenance and the observed helper routes. Builds use eight jobs on CPUs 0–7; isolated runtime fixtures use CPUs 8–15.

Durable schema 2 upgrades unambiguous schema-1 metadata once without replaying argv and rejects ambiguous foreign layouts without replacing the source record. Cross-session/SSH routing is not implemented by these checks; references remain a normal-use-through-owner design contract, not read-only sharing. Actual frontend interaction and terminal-restoration evidence is recorded separately.


### Actual frontend and theme acceptance

`target/smoke/workspace-windows/frontend/` exercises the matching protocol-3 release pair above on private Kitty 0.49.2/xterm 411 and Xvfb, with physical keyboard/pointer input, private HOME/XDG and credential-free native programs. `acceptance.json` indexes the commands, exact binary hashes and individual results; `controllers.zip` retains the reusable Python/C fixtures.

* `physical-1258412394882131/` exercises natural final-shell EOF; `physical-1258412395572958/` exercises explicit final-workspace close. Both use two actual clients: separate full-window creation, window chooser and pointer activation, independent client selection, a split confined to one window, stable rename/close targets, and fallback after the peer's displayed window closes. Blank-name creation preserves an inherited directory containing byte `ff`, verified by the native process rather than its lossy display label. Whole-workspace close terminates a hidden owned window without typing into it. Exact input logs preserve the surviving execution and exclude chrome input.
* Both clients move to the remaining workspace, then restore their real host TTYs when that final session ends: mode 2031 remains disabled (`2` before and after), native termios match, and both client exit statuses are zero. The natural case first returns from a child program to a still-live shell without closing its terminal, then sends shell EOF. The listener disappears and an unrelated named daemon continues consuming exact input. Screenshots show the restored host surfaces; all owned cleanup reports have zero survivors.
* `physical-1257993059229014/` repeats dark theme, passive metadata, forms/errors, finder, guide and actual live host-palette changes through the real device stream. `physical-1258099269792598/` repeats light theme at 240×72 with 13 workspaces and a 32-action navigation burst. Both retain the focused field through palette refresh, unchanged complete native-body text/pixels, no hidden views or layout mutations, and zero terminal bytes over the 1.1-second idle observation.
* Normal `Ctrl-]` detach now exits zero. The dark run records actual mode `2` before/after through a maintained `script` relay; the large light run checks the direct Bkpr TTY with mode originally enabled (`1` before/after) and matching termios. Shutdown settles the independent hide-policy receipt before admitting Detach; retiring scene pumps discard canceled generations without clearing genuine active input failures. The delayed-copy regression also verifies that export is rejected while a view is still present but retiring.
* `physical-1258304833718434/` supplies xterm colors with truecolor unconfirmed; `physical-1258304811147753/` withholds color replies too. Both use native/ANSI fallback without RGB or Kitty image output, preserve exact complete native text and input ownership, and emit zero idle bytes. Native-body screenshots retain a two-pixel difference in each xterm run; exact pixel identity is not claimed.

The private host wrapper reconstructs the native directory from ASCII hex after Kitty's UTF-8-only command-line parser, without changing bytes passed to Bkpr. Intermediate controller failures retain their output/screenshots; no product behavior or acceptance criterion was weakened to bypass them. These checks establish local interaction, color, ownership and terminal-restoration correctness, not startup/editor throughput, remote transport, decorative animation or general graphics performance.


## Daemon-owned SSH-agent forwarding

The first immutable protocol-4 release pair is under `target/smoke/ssh-forwarding/final/`: `bkpr` SHA-256 `8f49c0c90fbd2556102c1719ef6a57c47dc3874f1ed50702ae6e9886e1cbb3df`, `bkprd` `f0aec685e28e5148f0ed1c6d61e27ca9ee73dafb26354841e0c57b57932646b0`. This snapshot predates the concurrently developed frontend overlay/static-chrome changes; its acceptance is for forwarding, not those frontend additions.

`api-1276022941224706/` runs real private daemons, native PTY programs, two disposable OpenSSH agents and ephemeral Ed25519 keys. Fifteen agent-only `ssh-keygen -Y sign -U` attempts include nine cryptographically verified signatures and six expected refusals. The running pane keeps the same PID and socket environment across registration-only, A/B source changes, explicit absence, fallback, unselected detach, takeover, source-agent death/replacement and zero attachments. A separate owner process exits while its child retains the lifetime descriptor; the backend retires it and returns to the prior source without waiting for that inherited descriptor to close.

`tui-1276022954044389/` uses actual isolated Kitty clients and physical X11 keys. Six signing attempts verify relative-source capture, latest-source selection, refusal of the previous key, unselected detach, unset-source masking and one-shot takeover. The same native pane remains alive throughout. Screenshots show actual application output; both normal detach and takeover exit zero. Public allowed-signers files and signatures are retained, private keys are removed, and both runs report zero surviving owned processes.

The relay regressions exercise opaque extension bytes, independent connection state, large simultaneous bidirectional transfers, observed backpressure with unrelated traffic, half-close draining, stale epochs, owner liveness, self aliases, bounded admission, non-UTF-8 paths and inode/lease cleanup. A controlled stuck-lookup stand-in demonstrates independent traffic, revocation, replacement routing and shutdown before releasing the blocked admission worker; this is fault injection, not an actual FUSE/NFS fixture. Missing/non-socket sources fail closed without preventing attachment or modifying their paths. Isolated panes remain credential-free after a client publishes an agent.

The source/dependency decision and explicit resource bounds are in the [forwarding ledger](implementation-plan.md#ssh-agent-forwarding-boundary). No real user keys, SSH/Git configuration, profiles, live sessions or model/provider services were used by these fixtures. These checks do not implement remote endpoints, replay pending signing requests or impose an established approval timeout.

### Integrated debug verification

After the user clarified build policy, functional iteration uses incremental debug only. The integrated snapshot, including the separately owned frontend overlay/static-chrome work, is `target/smoke/ssh-forwarding/integrated-debug/`: `bkpr` SHA-256 `15cff63c7d6eaaeddb3f036979fea3d299949742d6a9b63a149172186f4c67a6`, `bkprd` `b8f31dd3157bce4c8914e733390d1e64d69622e06c66f303276c477e7b1408f7`.

`api-1277660219032212/` and `tui-1277660246777593/` repeat the complete real-agent API and physical-client cases above against that debug pair, with the same expected signing/refusal counts and zero cleanup survivors. This is functional verification, not a release/debug performance comparison. `acceptance.json`, retained public verification material and `controllers.zip` index the evidence.

The coordinated working tree passes **536 top-level tests**, with eight explicitly ignored subprocess fixtures, strict all-target Clippy and formatting checks. Counts include the parallel frontend changes. Native checks run as the host user, with eight jobs on CPUs 0–7; only the protected debug build uses private mount aliases. `protected-1277578714339700/result.json` proves every existing public debug/release executable stayed byte-identical. Cargo checks and protected builds are now serialized.

Verification also repaired two native test-fixture races without changing production lifecycle behavior: pending-client closure now matches the actual client's explicit socket shutdown, and the deferred-completion test blocks request admission rather than holding publication while inspecting registry state. The original stalled test had no remaining descendants and was retired by exact identity; its cleanup receipt is retained. Intermediate failures remain evidence, not successful acceptance.

## Generic overlays and static window chrome

The generic framework now owns overlay placement/layers, opaque return-focus tokens, modal/passive input routing, whole-gesture dismissal and clipping. Bkpr owns overlay content/actions and its `appearance` style recipe/materials. Main's focused-overlay faint is view-local presentation, not canonical terminal mutation. Static recessed/raised frames use eight perimeter fragments from a small atlas prepared by the existing graphics worker. Rich/plain modes reserve the same pane content rectangle; no graphics cover native cells.

This initial pass established mechanics, not visual parity. The user's side-by-side screenshot review rejected its glossy bevels, boxed sidebar and verbose frame titles. The source-backed correction below supersedes that appearance; the historical executable/check results here remain tied to their original hashes.

Evidence is under `target/smoke/frontend-overlays/`, with `acceptance.json` and CRC-checked `controllers.zip`. These are **debug functional checks**, using protocol 4, client SHA-256 `15cff63c7d6eaaeddb3f036979fea3d299949742d6a9b63a149172186f4c67a6` and daemon `b8f31dd3157bce4c8914e733390d1e64d69622e06c66f303276c477e7b1408f7`; no new release build or debug/release speed comparison is involved.

* `physical-1277080183843105/` records failing-before behavior: an outside press leaves the unsubmitted rename overlay open. `physical-1277647910820949/` and `physical-1277796281725523/` exercise the corrected lifecycle on actual Kitty: wheel/resize preserve a draft, Escape restores focus, an outside press cancels and consumes its release, and all four pane borders withhold native input. A click in the final native cell delivers exactly `ESC[<0;94;33M` then `ESC[<0;94;33m`.
* The dark and large light runs use a private real Git repository with a branch and unstaged change, no remotes/providers. Native body text and pixels are unchanged across metadata/forms/finder/guide interactions; live host palette changes preserve the field. The 240x72 run includes a 32-action navigation burst and narrow/large resize bursts. Complete popup references wait for Kitty's own resize indicator to withdraw, rather than treating its transient overlay as Bkpr pixels.
* Both modes of real notification ownership survive normal detach: originally disabled mode 2031 remains `2`, originally enabled remains `1`, native termios match and client exit status is zero. Each color/fallback run emits zero terminal bytes during its 1.1-second idle observation.
* `physical-1277911800512708/` supplies xterm colors without truecolor confirmation; `physical-1278130327065373/` withholds color replies too. Both are 120x36 and expose the same 94x33 native body as rich Kitty. No RGB/Kitty images are emitted. Complete native text is identical; native-body screenshots retain respectively one- and two-pixel differences, so exact pixel identity is not claimed for xterm.
* `physical-1278013915950276/` repeats two-client window creation/selection/splitting, rename/close, hidden owned-terminal closure, inherited native byte-`ff` directories and final natural shell EOF against the framed debug client. Both host TTYs restore with exit zero, and an unrelated session remains usable.

Every canonical case's exact-birth cleanup reports zero survivors. No live user session, profile, provider or desktop clipboard participates. Coordinated workspace tests pass 536 tests with eight ignored subprocess fixtures; strict all-target Clippy, formatting and protected incremental debug linking pass, as recorded above. Framework regressions cover sparse resource sharing, atlas invalidation, native content/cursor preservation, backdrop restoration and gesture tombstones. This establishes local functional/static presentation acceptance, not editor/animation throughput, startup latency, remote transport or general graphics initial-presentation performance.

Main's provider-dependent forge/PR/issues/CI card content and click-to-refresh cannot be supplied by the current backend protocol. Those facts remain explicitly unavailable and are not counted as full provider parity; no frontend network calls or fake/no-op substitutes were added. Decorative animation and a command palette remain out of scope.


## Main-aligned chrome correction

The reference is the user's `2026-11-02T23:23:56.png` screenshot: main on the left, the rejected candidate on the right. Main `9ad93bd` supplies the 38-cell sidebar, compact connected project/pane rows and terminal-derived palette. Fork `d7b40a37` explicitly excludes raised bevel/outline styling and clears recessed-frame text because labels live in the sidebar. The rich bottom status line in the reference belongs to the terminal application; Bkpr does not fabricate those metrics.

The corrected client uses an unboxed sidebar, elides sole-window rows without changing backend identities, omits empty project/detail hit rows, and keeps conventional top titles only in plain fallback. The framework ports the fork's rounded-mask convolution with its MIT notice, while the client supplies all material values. Multi-cell sparse atlases retain physical blur/radius support. Opaque-cell and transparent-graphics layers preserve native glyphs/cursor/hits; later overlays clip lower images, and scene-local depth ranking prevents an extreme native image z from covering later effects.

Evidence: `target/smoke/visual-parity/acceptance.json` and `controllers.zip`. The immutable **debug** pair is `bin/bkpr` SHA-256 `f5816109a15834acc1f052cf07d6898b7eb9a0efac8a20d3c55f7663367e1692`, `bin/bkprd` `3d1eded764d11f8b5844e14102775ef808dbe2cd00bbfb7d59e2b660a80b9e0c`. Protected incremental linking preserves existing public debug/release executables; no release build or performance comparison is involved.

| Physical run | Host cells | Native body | Native pixels changed across overlays |
| --- | --- | --- | --- |
| `physical-1282813032765718` — dark Kitty, live theme | 120×36 | 80×34 | 0 |
| `physical-1282911828352305` — light Kitty | 240×72 | 200×70 | 0 |
| `physical-1282911799805983` — ANSI-only xterm | 120×36 | 80×34 | 0 |
| `physical-1283006349533222` — color replies withheld | 120×36 | 80×34 | 1 |

These use private HOME/XDG/socket roots, actual native PTYs and row-dependent styled full-grid output. References require every expected native cell and rich-ready label-free chrome, not a first glyph. The dark/light runs exercise actual popovers, rename/error/finder/guide surfaces, 32-action navigation bursts, draft-preserving wheel/resize, outside-press cancellation and exact final-cell mouse coordinates. Live host color change retains the draft. All four emit zero terminal bytes in their 1.1-second idle interval; unconfirmed hosts emit neither RGB nor Kitty graphics.

`physical-1283054008011672` repeats independent two-client window selection, separate creation, splitting, rename/close, hidden owned-terminal closure, raw byte-`ff` inherited directories and natural final shell EOF. Both terminal hosts restore successfully; an unrelated named session survives. Every case's exact-birth cleanup reports zero survivors.

Coordinated workspace tests pass **541 tests**, with eight ignored subprocess fixtures; strict all-target Clippy and formatting pass. Regressions include transparent-layer cursor/hit preservation, opaque popup occlusion, mode-change invalidation and extreme pane-local image depth. Screenshots were inspected on rich dark/light and plain surfaces. This is not exact cross-runtime pixel parity: a fresh full-main fixture was not exercised, since the retained controller requires plugin permission grants that were not automated or seeded. The user screenshot and inspected source remain the visual references. Startup latency, editor throughput, remote transport and general graphics initial-presentation gates are unchanged.

## Main-aligned key guide

The immediate next frontend slice restores main's right-aligned alias column, wrapped descriptions and adaptive card width. Product policy stays in `bkpr-client`; `textwrap` 0.16.4 supplies maintained first-fit wrapping, and the existing framework supplies windows, roles and scene output. One capability-filtered layout emits exact key spans, including literal slash keys and wrapped aliases. An action's complete explanation fits or the whole action is omitted with overflow feedback; descriptions are not chopped to fit a row.

The retained regression first reproduced `x remove from layout (keep proc` at a narrow width. It now preserves the complete current explanation. Additional boundary coverage checks intact alias keycaps, description-continuation styling and all small viewport dimensions. Coordinated workspace checks pass **543 tests**, with eight ignored subprocess fixtures; strict all-target Clippy and formatting pass.

Evidence is `target/smoke/keyguide/acceptance.json` and CRC-checked `controllers.zip`. Its immutable debug pair is `bin/bkpr` SHA-256 `88474d3664a6b1cd053315af757746fe2e417be5b47451a44be571dbf7f6ad53`, `bin/bkprd` `3d1eded764d11f8b5844e14102775ef808dbe2cd00bbfb7d59e2b660a80b9e0c`; public debug/release outputs remain unchanged.

`physical-1284590094349844` (240×72 light Kitty), `physical-1284590113416306` (120×36 dark Kitty) and `physical-1284590094342651` (120×36 ANSI xterm) inspect actual guide columns, complete aliases, modal/finder interactions and native content restoration. The large guide visibly includes the complete layout-removal lifetime explanation. Rich resize cases cover 80×24, 40×18, 120×36 and restoration to the original size, observing the host resize indicator's withdrawal before guide screenshots. The guide remains passive; resize/form handling adds no application input. The displayed send-prefix action delivers exactly one byte `02` to its original native pane and returns application focus, with no input to other panes.

All canonical cleanup receipts have zero survivors. Native-body before/after image differences are zero for both Kitty runs and one pixel for xterm; complete native text remains identical. Each baseline idle interval emits zero terminal bytes. Three noncanonical runs exposed only a fixture predicate that mistook the persistent footer shortcut for an open guide; screenshots showed the guide correctly closed. The fixed observation excludes the footer, and all canonical cases pass without a production change. These are local debug correctness/presentation checks, not throughput or full-main cross-runtime pixel parity.

## Shared split gutters and chrome resource reuse

Adjacent pane frames now share their one-cell perimeter instead of allocating two borders plus a divider. Both native-body gaps are one cell, for side-by-side and stacked splits. Ready graphics perimeters contribute no text; plain captions survive shared rows, resizing and capability loss. Unrepresentable splits project their first subtree locally, preserving the authoritative layout and processes.

Main/fork reuse and the distinction between independent pane apertures and joined raised surfaces are recorded in the [implementation plan](implementation-plan.md#overlay-lifecycle-and-static-graphics). The existing worker already prerendered dimension-independent sparse atlases and whole straight edges. The additional framework correction preserves constant-axis resources through representable viewport/opaque-layer clipping rather than rasterizing derived strips. Regression coverage exercises real cache hits/eviction and the physical writer: one cold atlas upload, then no additional upload for unchanged, resized and moving-occlusion variants. Exact phase-preserving rasterization remains necessary for some arbitrary native-image scaling; placement changes and projection planning are not free.

Evidence: `target/smoke/split-chrome/{acceptance.json,source-freeze.json,visual-review.json,controllers.zip}`. The immutable **debug** pair is `bin/bkpr` SHA-256 `4a947edf191eabdd03a6be5b7e9cb0deea56e446ae6d6415d8ea7d13d7b957cd` and `bin/bkprd` `8286e340bc7b0b5c725d72948bc2183f74b2abef53b7caf16e1023098a1c05d0`. The protected build receipt is `target/smoke/ssh-forwarding/protected-1288375356112256/result.json`; public debug/release executables were preserved. This pair predates subsequent concurrent backend recovery changes, not the frontend source recorded by the manifest.

| Physical run under `target/smoke/split-chrome/` | Scenario captures | Independent native-body comparisons | Pixel observation |
| --- | ---: | ---: | --- |
| `kitty-dark-default-1289156629624401` | 23 | 82 | Unshaded-interior differences have maximum channel delta 1 |
| `kitty-light-default-1289097337091829` | 23 | 82 | Unshaded-interior differences have maximum channel delta 1 |
| `xterm-dark-default-1289160701967639` | 23 | 82 | At most 54 changed whole-body pixels, maximum channel delta 13, all at cell-left edges |
| `xterm-dark-bitmap-1289093983519618` | 23 | 82 | Every whole-body comparison is pixel-exact |
| Total | 92 | 328 | Default-font pixel identity is not claimed |

Every functional matrix passes side-by-side, stacked, four-way cross, nested-T and asymmetric layouts at 120×36 and 240×72. Physical keyboard focus/resize, both divider-drag axes, exact native click coordinates and modal press/release capture pass. Resize bursts cover 80×24, 38×14, 160×48, 60×20, 240×72 and 120×36; narrow input and 6×3 collapse/restoration retain the original processes and backend layouts. Tiny projection is checked through actual native text/input/identity rather than an image potentially covered by the host resize indicator. Complete expected text, target layout/ratio and indicator withdrawal precede normal screenshots.

Rich material bands intentionally shade native edge pixels and have no independently composed material-band oracle here. Default-font differences are reported without tolerance or an observed-difference mask; whole native crops and full diff images remain available. Rich startup samples changed no palette/background endpoints, but unshaded interiors can differ at many antialiased pixels by one channel unit. Glyph coverage/run-boundary quantization is **[INFERENCE]**, not a proven cause. The additional bitmap control does not replace default-font screenshots. Parent inspection includes canonical dark cross, light nested-T and default-font plain cross captures, recorded in `visual-review.json`.

All cases emit zero additional terminal bytes or image-payload bytes during their 0.35-second unchanged observation. Exact-birth/pidfd cleanup has zero survivors. Private HOME/XDG/sockets, verified native C programs and isolated terminal hosts avoid user profiles, models, providers, approval caches, live sessions and desktop clipboard contents. Original fixture failures are retained: Kitty cell-unit sizing included one extra pixel, xterm print output included history rows after resize, and nested adjacency/layout-readiness predicates required correction.

Final coordinated workspace checks after the backend owner's frozen handoff pass **569 tests**, with nine ignored subprocess fixtures, plus global formatting and strict workspace all-target Clippy. Counts use top-level Cargo suite summaries, excluding the nested `foreign_client_fixture` subprocess summary. Raw output is retained in `target/smoke/split-chrome/workspace-checks.log`. This supersedes the earlier attempt stopped by the backend's incomplete `control_resume_barrier` test hook; no backend source was modified here. The frontend source and tested client executable remain unchanged through that handoff.

Debug RSS/byte observations are not release throughput, startup, remote latency or hardware-presentation measurements. The 16-entry/8-MiB material-cache budget excludes consumer-held shared references.

## Explicit native recovery

The backend now persists authenticated harness kind/native conversation ID/raw
CWD descriptors in private registry version 3, separately from live capture and
agent authority. Headless `bkpr resume` lists without execution or daemon
bootstrap; exact `--pane` selection and shared scoped MCP `control` use the same
reserved native-resume path. The approved policy is current native configuration:
no saved argv/model/provider/profile/approval/sandbox replay, provider switching,
implicit fork or fresh-session fallback. A spawn result remains pending native
binding, not proof of native restoration.

Evidence is `target/smoke/harness-recovery/acceptance.json` and CRC-checked
`controllers.zip`. The immutable **debug** pair is `bin/bkpr` SHA-256
`4a947edf191eabdd03a6be5b7e9cb0deea56e446ae6d6415d8ea7d13d7b957cd`,
`bin/bkprd` `6ad651c085a411f1df9cd7a6712b20c12c8c3b34e010fa9d74174175f5cd44ec`.
Protected incremental linking preserves public outputs during linking; no
release build or performance comparison is involved. Source hashes and the
build receipt identify the backend cutover independently of concurrent frontend
work, which is not part of the backend commit.

* `omp-1291324099352526` runs actual Omp 18.13.2 against an owned loopback provider,
  persists a real user/assistant turn, restarts the daemon and resumes the same
  native conversation. Saved user and assistant content is visible again, while
  launch identity changes. Withholding only the fixture's native session file
  first causes genuine native refusal; the original descriptor remains
  recoverable, and restoring the file permits a successful explicit retry.
* `codex-1291151827080890` runs actual Codex 0.165.0 with its genuine app-server
  and TUI. The exact response-correlated thread binds before the first prompt;
  an intentional local provider rejection still records the user turn. Daemon
  restart and native resume preserve that thread and visible user history with
  fresh launch authority. No MCP function approval is bypassed.
* Both cases verify read-only listing without revision changes, cleared live
  facts after restart, duplicate refusal, and no argv/tokens/launch IDs in the
  registry. Their exact-birth cleanup receipts contain real tracked descendants
  and zero survivors. Private HOME/XDG/native stores and loopback-only network
  namespaces exclude user conversation stores, real providers and live sessions.

The preserved Codex failing-before screen,
`codex-1288498602909478/failure.txt`, reports ambiguous native CWD: the old
selection treated its same-depth app-server and interactive TUI equally. Role
classification now excludes the native app-server. Deterministic regressions
also first failed for exited auxiliary-process inspection and silently
substituting a supervisor's directory; verified exit is now non-native, while
missing/ambiguous live native processes fail binding rather than fabricate CWD.
Additional regressions cover retirement after scope sampling, client detach
during termination, unaffected sibling-window callbacks, durable-store failure,
stale/aliased resume reservations, raw directories and unrelated live input.

Final coordinated formatting, workspace tests and strict all-target Clippy pass:
**569 top-level tests**, nine ignored subprocess fixtures. Native runtime
acceptance is Linux Omp/Codex only. Claude's offline first-run preflight and
Antigravity's unapproved Terms/native receiver remain the existing genuine
acceptance gates. This does not implement a frontend recovery picker, process
resurrection, persisted terminal contents, remote endpoints or sustained-animation
CPU-efficiency improvements.

## Native pane context

The prior native executable reproduced both gaps in
`target/smoke/pane-context/before-1293292917934274/result.json`: session/pane
snapshots were present but both workspace variables were empty, while an outside
`context` call returned the entire registry rather than a verified live pane.

The replacement shares one canonical pre-exec context across bootstrap,
workspace/window creation, append, reopen and native recovery. Pending automatic
workspace names are reserved before exec and also excluded from metadata-only
automatic naming; explicit duplicate labels remain allowed. Session/pane and
optional workspace environment values are immutable process snapshots, not
authority. Typed live context instead verifies the OS peer's birth, ancestry and
controlling PTY, revalidates the runtime and reads current canonical labels.
Bounded startup readiness uses existing spawning state/Condvar, not polling or
an extra context service. The launcher now owns presentation; the obsolete
whole-registry frontend helper is removed.

Evidence is `target/smoke/pane-context/{acceptance.json,workspace-checks.log,controllers.zip}`.
The immutable **debug** client/utility is `bin/bkpr` SHA-256
`087f18a38081f158ac29687fe606377d7c9f0ee50aeb47c063c101113493a489`;
the daemon is `bin/bkprd`
`c83bc667e895f5f70155911f62dd6c757f308eb9e669bf4dc09b8b03a5dea4f0`.
The protected incremental link receipt is
`protected-1293659472242395/result.json`; public outputs are preserved during
that link. No release build, renderer comparison or throughput claim is made.

`native-1293758818268033` executes actual native shells and `bkpr context --json`
against private daemon/HOME/XDG roots. Immediate child context succeeds before
spawn publication; snapshots match canonical IDs and Unicode labels. Renaming
workspace/window/pane changes live query labels while the running shell's
original environment stays unchanged. Spoofed inherited pane IDs are ignored.
Hidden layouts and a second client's borrowed projection retain execution
membership. A nested PTY and an outside automatic caller fail closed; explicit
standalone context is unassigned, and terminated context is unavailable.
Concurrent native and metadata-only auto-named creation yields distinct
canonical names that match the actual children. Exact-birth cleanup tracks real
descendants and reports zero survivors.

Coordinated checks pass **581 top-level tests**, with twelve ignored subprocess
fixtures, global formatting and strict workspace all-target Clippy. Static
reviews of name-reservation lifecycle and peer/context authority found no
concrete defects. Native proof is Linux-only; supported-platform acceptance
elsewhere remains unverified. This does not add cross-workspace execution moves,
remote endpoints, agent permissions, a recovery picker or CPU-efficiency work.

## Automatic named-session restoration

Cold named startup now restores owned terminals in the saved workspace/window
identities, nested split positions and hidden membership. Ordinary terminals use
the current supplied shell in their latest verified directories, not their saved
arbitrary argv. Verified native conversations still active at shutdown use fixed
resume adapters with current native configuration and fresh launch/inbox authority.
Historical descriptors alone do not authorize automatic native resumption.

Private registry schema 4 separates directory observations, active automatic intent
and historical explicit native recovery. Older records migrate without inventing
prior native activity. Returning to a shell clears automatic native intent while
retaining the explicit descriptor. Changed observations checkpoint in the existing
off-path observer cycle, without an extra debounce delay; graceful shutdown takes
a fresh observation before teardown. Abrupt loss can miss an unobserved/uncommitted
change. Terminal contents, arbitrary argv and native settings remain unpersisted.

The server admits authenticated native lifecycle/receiver callbacks during the
restore batch, but gates ordinary requests and shared-MCP Control. Explicit
shutdown remains available and cancels/reaps owned startup work. Missing directories
and native refusal preserve stopped layout/intent without fallback while independent
terminals restore. No-argv headless startup executes nothing; live attachment never
repeats restoration.

Evidence is `target/smoke/automatic-restore/{acceptance.json,workspace-checks.log,controllers.zip}`.
The immutable **debug** pair is `bin/bkpr`
`85d8df4a0b28504ef5b002416f3a8e7dd77a956fd513c5859e32e93106284522`
and `bin/bkprd`
`c06a2ca6c35a45fe7291534975ec1a5314b508c9c3c2af2c776bb84aad357198`.
The protected incremental link receipt is
`protected-1308542911030236/result.json`; public outputs were preserved during
that link. Earlier host server-integration checks can link the public daemon;
this is not a claim that all Cargo commands isolate public outputs.

| Actual runtime scenario | Observed result | Evidence directory |
| --- | --- | --- |
| Named `bkpr -s foo`, graceful restart and second live TUI attachment | Stable session/workspace/window/pane identities, ratios 370/620, latest shell directories, hidden terminal restored, retired unowned execution not replayed; both clients detach normally without another restoration | `shell-1308590023363784` |
| Abrupt daemon exit plus a removed saved directory | Remaining terminals restore; missing terminal stays stopped in its original layout with its original directory intent; headless inspection does not execute anything | `shell-1308589997490335` |
| Genuine Omp 18.13.2 | Cold automatic resume loads original conversation text in the same pane with a new launch identity; ordinary anchor shell restores its latest directory | `omp-1308647576792869` |
| Genuine Codex 0.165.0 | Cold automatic resume loads original thread text in the same pane with a new launch identity; ordinary anchor shell restores its latest directory | `codex-1308647565882097` |
| Genuine Omp with its private native session file withheld | Native refusal retains recovery intent, does not fall back to a fresh conversation, and exact explicit retry succeeds after restoring that file | `omp-1308647557144912` |

All five final owned-process receipts report zero survivors. Native harness
children use private HOME/XDG stores, isolated loopback providers and network
namespaces; no real provider, live session, credentials or native approvals
participate. These are actual launcher/native output and state checks, not a new
physical graphics, throughput or latency comparison.

Review regressions failed before their fixes and pass afterward: an older queued
directory observation could erase a newer authenticated native binding; shared
Agent Control could bypass startup admission; and closed readiness/error output
could strand admission or the session lease. Coordinated checks pass **621
top-level tests**, with fourteen ignored subprocess fixtures, global formatting
and strict workspace all-target Clippy. Native/process proof remains Linux-only;
the genuine Claude/Antigravity prerequisites and frontend recovery picker remain
open.

At the automatic-restoration snapshot, a frontend shutdown race was retained rather than hidden: a pending RPC
can surface `session is shutting down` as an error even when lifecycle revocation
is expected. The failed owned fixtures retain that output separately. Final
automatic-restoration cases detach clients normally before administrative shutdown;
they do not claim that every concurrent TUI RPC now exits cleanly on daemon stop.
The subsequent [typed shutdown pass](#typed-session-shutdown) addresses that
expected-shutdown failure and exercises clients without detaching them first.

## Typed session shutdown

The additive protocol-4 `Response.failure` metadata separates known
`session_closing` rejection before dispatch from `unknown` after dispatch began.
Only typed backend closing errors receive it; successful replies and unrelated
errors are not reclassified from global state or text. Decoded requests hold
response-drain admission through startup cancellation and known closing rejection.
Managed snapshot checks retain their exact lease's shutdown cause even after view
removal, without converting takeover/stale-view errors into session shutdown.
Immutable `SessionInfo` remains available after persistence poison, preserving
the real CLI's target-validation prerequisite before explicit `Shutdown`.

Clients cancel observational streams but let accepted mutations drain their actual
responses under existing deadlines. Typed errors survive snapshot pumps, native
input receipts, retirement and attach rollback. Known undispatched requests and
observational interruptions backed by a typed closing reply or validated shutdown
event can exit cleanly. Bare EOF, malformed responses and genuine failed or unknown
mutations do not become successful results. Later cleanup cannot lend its safe
cancellation marker to an earlier malformed snapshot or hide another failed action.

Evidence is `target/smoke/session-shutdown/{acceptance.json,workspace-tests.log,workspace-strict.log,controllers.zip}`.
The immutable **debug** pair is `bin/bkpr`
`ddba47598ee35415a27737b197354214028a117ff0ab0886925c6e857f0104d6`
and `bin/bkprd`
`c70a3d41d17459ba1fe7b4dd8e6f5c006b6a1935d3fcd2f667a984d8f04c00f2`.
The protected incremental link receipt is
`protected-1312186005744632/result.json`; it preserves public outputs during that
link, not during every preceding host Cargo command.

Actual runtime `matrix-1312265720678523/result.json` exercises two iterations
each of explicit `bkpr stop`, natural last-shell exit through real TUI input,
last-terminal termination and last-workspace closure. Every case has two attached
TUIs and five pending observational requests. All **16 client exits and eight
daemon exits are 0**, exact original termios is restored and no TUI emits `Error:`.
Exact shell process births disappear. An independent session retains its identity
and consumes real TUI shell input after every case; normal detach leaves its daemon
running.

The same matrix verifies failures against the actual backend: an invalid-view
keyboard mutation returns `ok:false` and CLI exit 1. A controlled Unix proxy forwards
an accepted workspace rename, then deliberately drops its response. CLI exit 1
reports outcome unknown/no replay; authoritative name/revision changes and the
proxy trace prove exactly one dispatch. These failures are not suppressed to make
shutdown appear clean.

Full workspace checks pass **645 top-level tests**, with fourteen ignored subprocess
fixtures. After a mechanical renderer context/cache-key refactor, all **97 framework
tests** pass again; final strict workspace all-target Clippy and global formatting
pass. Deterministic regressions cover response admission, late lifecycle/registration,
startup cancellation, persistence failure precedence and stop identity, exact
managed Poll leases, accepted mutation receipts, attachment rollback and queued
Select/Search shutdown rejection.

Runtime is Linux host UID 1000, private HOME/XDG/socket roots, real shell PTYs at
100x32 and inherited AF_INET/AF_INET6 socket denial. Final cleanup reports zero
surviving owned process births. Build/check jobs remain bounded to eight CPUs/jobs;
no release benchmark, physical graphics comparison, provider/account/profile
change, or live session participates. Existing `serde`/`anyhow` and transport/lifetime
mechanisms suffice; no dependency or second lifecycle service is introduced.


## Contextual chrome and horizontal frame substrates

The current frontend proof is
`target/smoke/hint-popup/horizontal-background-acceptance.json`. Its immutable
dev-profile pair is client
`a4dc776f7e4ed3dd8c80a6618a531c713932514a40434dff0a2375cb54669359`
and daemon
`c70a3d41d17459ba1fe7b4dd8e6f5c006b6a1935d3fcd2f667a984d8f04c00f2`.
The protected link receipt is
`target/smoke/ssh-forwarding/protected-1317588570464055/result.json`;
its before/after public executable hashes agree. Source hashes, controller
archive, complete per-case records and uncensored PNGs accompany the proof.

Main's retained fork explicitly fills cleared frame cells with the original
pane background in `output/recessed_frames.rs::decorate_characters`. Our hidden
frame glyphs had still received sidebar-background border styling. Compact
horizontal openings extend into that character row, exposing the wrong substrate.
The client now applies its existing normal-background panel role only to
material-ready frame rectangles. Opaque atlas pixels still supply the outside
matte; translucent inside shadows now blend over the correct pane color.
No extra texture, raster operation, viewport margin or canonical pane edit is
introduced. The renderer's existing retained style/damage path owns the change.

Four actual default-font Kitty/xterm dark/light cases pass **56 contextual
captures**, including normal/sidebar/prefix/resize/history/search guides,
modal/finder suppression, live palette changes on Kitty, narrow overflow,
resize return and reversible `Ctrl-b g` rendering. All four normal detaches
exit zero, restore exact termios and host background, and leave pane processes
backend-owned until fixture cleanup. Unchanged idle windows emit no additional
terminal or image bytes. Native dimensions, cursor, view identity and input logs
remain checked; native image fields are retained, but this raw-grid fixture does
not claim physical application-image presentation.

Two more Kitty dark/light cases cover single panes, stacked panes, cross
junctions and tall **120x80** windows: **eight captures and 32 horizontal-aperture
pixel samples**. The straight-edge reference uses the original background plus
the configured Gaussian shadow, requiring a consistent image-alpha blend space
and allowing only eight-bit rounding. Shared-edge samples identify linear-light
blending on this host. Normal pane background, rather than matte, is now visible
inside both exterior and shared horizontal borders. Tall dark and stacked light
PNGs were inspected directly. All six accepted fixture receipts have zero
surviving owned process births.

The legacy keyboard path also exposed Termwiz's raw NUL/FS/GS/RS/US characters.
The adapter now supplies printable control-key semantics rather than losing them
to the native encoder's control-text fence. A real Termwiz-to-Ghostty regression
checks exact native bytes, including Alt-GS; actual xterm `Ctrl-]` detach passes.
Clipboard replies and paste framing do not enter this normalization path.

Coordinated current-source verification passes **646 top-level workspace tests**
with fourteen ignored fixtures, strict workspace all-target Clippy and formatting.
Receipts are under `target/smoke/bootstrap-test-checks/`. An earlier run exposed an
unrelated recovery test's global-revision race. Its backend owner replaced
incidental revision/whole-record pins with public no-replay PID checks before the
passing run; the failure was neither hidden nor re-pinned.

This is Linux, private HOME/XDG/socket, headless Kitty/Xvfb correctness evidence,
not release latency, GPU FPS or equal physical presentation-cadence evidence.
It does not establish project-color or sidebar-popup formatting parity.

## Main-aligned finder cards and full raised surfaces

The finder now follows main `9ad93bd`: separate query/results/preview cards,
40/60 wide or stacked geometry, single-row selected results, truthful
matched-field excerpts and grouped metadata. The query uses the terminal's cyan
accent; neutral cards use the fork's six-percent linear-light lift. Forms reuse
main's mode tints. Bkpr owns these choices; the framework supplies rounded fill,
all-edge bevels, shadows, source-local cell clipping and retained background
spans. Three cell surfaces borrow one frame without copying its contents.
The existing off-thread worker, 16-entry/8-MiB material cache and stretched
straight-edge resources remain.

Raised highlights now include the surrounding surface color, matching the
fork's active `bevel_normal` and restoring light-theme upper/left facets.
A separate retained-theme regression failed when equal local update counters
kept the old palette, then passed with process-unique resolved-style revisions.

Immutable optimized-debug native pair:

- `target/smoke/finder-overlays/bin-1322281134781673/bkpr`,
  SHA-256 `0ad78fc2ffad39b384354df75af7d27b7d83c3a830c6ed2fae849bf3da56ce5c`.
- Matching `bkprd`,
  SHA-256 `407bb0b330c4e5f25d1982a8c9022b54465b05fc7de18d2a9b72d4487fae1d8b`.
- `build-snapshot.json` in that evidence root records 26 frontend/manifest
  hashes and the protected link receipt. All four public executable hashes
  stayed unchanged. Workspace verification passed 654 top-level tests,
  fourteen ignored fixtures, strict all-target Clippy and formatting before
  the backend owner resumed unrelated Core-only work.

`target/smoke/finder-overlays/acceptance.json` records dark/light Kitty and
dark/light xterm: 64 structured finder captures, 12 form screenshots and
240 three-pixel raised-edge traces. Screenshots were inspected for wide,
stacked, light/dark, normal and tinted-form presentation. The workload covers
120×36, 88×44, 44×20, 28×10 and 160×48, private real Git metadata, Unicode
editing, preview keys/wheel, active rendering switches, reopening and cancel.
Literal `FOCUS` bytes subsequently reached exactly the selected stable pane.
UI keys did not leak, metadata views did not mutate backend layouts, all four
normal detaches returned zero and restored exact termios/host backgrounds.
Native processes remained backend-owned until exact-birth fixture cleanup;
every final cleanup reports no survivors.

Warm navigation added no image payload and static observations added no
terminal bytes. These are debug correctness/resource observations, not GPU
latency, release FPS or main-runtime performance-parity claims. `controllers.zip`
has verified CRC/member hashes and SHA-256
`7e9f5403a12b81acf0d0c066a81b30d1c63f8a711936e3f2e6d7ffbedf7eec90`.
Intermediate controller failures remain separate: saturating page navigation
is not invertible from an intermediate offset; Xvfb's last-client reset can
race terminal startup. The final controller starts scrolling at the top,
uses `-noreset`, and replayed all four contexts.

Provider forge/CI/ticket facts and stable backend project-color ordinals
remain unavailable. This snapshot does not claim sidebar hover/color parity.

## CPU-efficiency: measured inline cell storage

The sustained-output CPU gap is real. The diagnostic-only evidence is in
`target/smoke/cpu-diagnostics/findings.json`; optimization experiments, immutable
release controls and acceptance receipts are in `target/smoke/cpu-optimization/`.
The retained change replaces each `Cell.text` heap-backed `String` with the
maintained `compact_str` crate's `CompactString`. On this x86-64 target it has the
same 24-byte object size, keeps short UTF-8 text inline and spills longer grapheme
clusters to the heap. There is no truncation, Unicode substitution, JSON-wire
change, frame cap, added delay or slower process-recognition interval.

Two separately controlled comparisons isolate the server and client effects.
The server comparison uses the earlier identical frontend; the client comparison
uses one captured newer frontend source tree and a fixed optimized server.
Do not add the two reductions or interpret these as one cumulative old/new pair.
Each row averages two unprofiled release runs, with the same 240×79 native pane
and exact offered source bytes/deadlines. CPU sums only mux processes; 100% is
one logical CPU.

| Changed component | Workload | Control CPU | Inline CPU | Matched Zellij CPU |
| --- | --- | ---: | ---: | ---: |
| Server, earlier frontend | Dense redraws | 33.34% | 27.25% | 12.21% |
| Server, earlier frontend | Recorded `cmatrix` replay | 151.69% | 148.13% | 13.40% |
| Client, captured newer frontend and fixed inline server | Dense redraws | 30.62% | 27.26% | 12.05% |
| Client, captured newer frontend and fixed inline server | Recorded `cmatrix` replay | 151.49% | 144.98% | 14.30% |

Client controls are SHA-256 `4e67c4fc2d920e83469cdcf5449a1f0c010d9fded82abf582ad6ab2d4d22990a`
(heap) and `b681c4420923d7d90e93c186bd500bc23a02bcc20e1fafe9699617f033218f71`
(inline); both use server `969e57a7a4608ec6d0cfbb70ace42121d4174589574f0b0f774d3a104d8e9072`.
`client-control-builds.json` records the captured source identities and protected
link receipts. All four pre-existing public debug/release binaries remained
unchanged. The source capture predates a connected-popup-only frontend correction;
these CPU cases never open a popup, and both controls use identical captured UI.

The dense source offers 300 frames at 30 Hz; the replay preserves the original
recording's byte stream and deadlines rather than generating new random matrix
output. All twelve final client/control cases passed exact complete-source,
native geometry, final full-grid and exact-birth cleanup checks. These are CPU
measurements, not physical-terminal FPS, pixel parity for every intermediate
frame or proof of equal presentation cadence. Products use CPUs 8–15, software
rendering 16–23 and observers 24–27; CPU affinity is not exclusive reservation.
No owned compilation or other heavy fixture ran during the measurements.

The actual-PTY EOF checks cover 16,383, 16,384, 16,385 and 65,549-byte prefixes
followed by a final Unicode screen, including combining marks and a grapheme
longer than the inline capacity. All final complete frames matched before and
after the retained change. The permanent lifecycle regression checks final
Unicode output after partial-buffer termination, not storage representation.
The coordinated workspace receipt in `target/smoke/sidebar-popups/workspace-checks.log`
includes that regression, 669 top-level passing tests and 14 explicit ignored
fixtures; strict all-target Clippy and rustfmt also passed.

Rejected experiments remain explicit: moving the scene JSON subtree reduced
client work but increased backend publications and total CPU; opportunistic
already-available PTY read batching did not improve total CPU; reusable procstat
buffers with the unchanged recognition interval did not reduce idle CPU. None
of these candidates is retained. Their individual receipts remain in
`scene-move-experiment.json`, `reader-experiment.json` and
`idle-buffer-experiment.json`.

Animation still costs about ten times the matched Zellij control. Generic scene
JSON materialization/re-decoding, allocation, full-grid extraction and
connection/thread churn remain candidates; host-wide process sampling remains a
separate idle cost. This is measurable progress, not near-Zellij acceptance.

## Sidebar hover, project colors and warm image residency

Source policy is main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`,
`crates/bkpr-zellij/src/{ui,palette,native}.rs`: structured passive metadata,
project-name/pane-marker accents, neutral branches, finite ANSI-derived colors
and canonical-repository reservations. Connected raised geometry comes from
fork `18890b231bc3dd29039dd6ca141ec0af49cf332e`,
`zellij-server/src/output/recessed_frames/{joined,contour,patches,depth,strips}.rs`.
The applicable Zellij MIT notice is retained in the framework source.
Bkpr owns palette/content/material choices; the framework owns generic union
geometry, glyph-safe color spans, preparation and image lifetime.

The final debug pair is
`target/smoke/sidebar-popups/bin-1339780935436455`:

* `bkpr`: `d7f73e4ac86b91944f54c307615bafd916ef11427a0f3b062001245a34d4e27a`.
* `bkprd`: `8dd470d33df08d2f6ec9ac38b4956fa06de7d9d0b672ce7771284061be9f0c7c`.

`cache-build-snapshot.json` records all 151 source/manifest hashes, the protected
link receipt and the prior immutable color-only client
`a7cedf1a0d10d968d51dae1f750121155c4b154ff0f0c479e7eaf033f3f8ba88`.
Both clients use the same daemon. All four public debug/release executable
hashes remained unchanged. `cache-workspace-checks.log` records 689 top-level
passing tests, 14 explicit ignored subprocess fixtures, strict all-target Clippy
and rustfmt. Native-engine regressions cover exact A/B/A projection pixels and
identity reuse, source retirement, inactive count/byte eviction and a visible
projection exceeding the inactive-cache budget. Large VT output is fed in
64-KiB chunks, respecting the engine's unchanged 1-MiB admission limit.

The initial full check had one capture-fixture readiness failure, retained in
`capture-readiness-failure.log`; its cause is undetermined. The focused fixture
passed under process/write tracing, and subsequent complete suites passed
without capture/authorization changes. The private trace is retained separately.
`large-image-admission-failure.log` records the new regression helper's initial
oversized feed; only its chunking was corrected, not production quotas.

`project-colours-acceptance.json` records the color-only pair's five complete
contexts: Kitty dark/light, xterm dark/light with confirmed truecolor, and xterm
with genuinely disabled truecolor capability replies. Each uses ten private
native C-grid panes, two local repositories and a linked worktree. Native
process identity/birth is verified before explicitly signaled fixture OSC 7501;
no real harness, provider, profile or approval store is invoked.

Physical foreground checks cover linked-worktree slot/hue sharing, independent
repository distinction and agreement across sidebar markers, popup names,
finder markers and previews. Real broken `.git` metadata produces an error
without losing the slot; repair restores fresh facts with the same assignment.
Real Kitty foreground/background/ANSI changes trigger fresh theme queries and
consistent recoloring without changing backend reservations. Names remain
unique in grouped repository summaries; branches/tree guides keep their neutral
roles, and semantic status/count colors are not flattened by hover.

`cache-acceptance.json` records the final pair's dark/light Kitty and genuine
ANSI-only fallback cases:

* `kitty-dark-default-1340010493211277`.
* `kitty-light-default-1340010558894167`.
* `xterm-dark-default-1340010557376435`.

Each rich case retains 24 popup capture records and the plain case 23, plus
finder, palette, normal-rendering and restoration screenshots. The workload
exercises semantic no-button hover, keyboard/pointer precedence, popup handoff
and wheel scrolling, Codex glyph/spacer, fancy/plain toggling, 160×48/64×28/120×36
resizes, native motion-reporting ownership and exact input delivery. It verifies
unchanged native view/layout membership, zero idle output, clean detach, original
termios/background restoration, host mode-1003 reset and survival of backend
programs after client detach. All owned fixture cleanups report no survivors.
`candidate.json` records dimensions, 9×18 cell pixels, Linux/Nix environment,
terminal versions, palette, process births and executable hashes; Kitty is
0.49.2. Screenshots are acquired from the actual Xvfb terminal surface after
complete content readiness, not rendered from recorded VT.

The same controller reproduces the cache defect on the old immutable client in
`kitty-dark-default-1340010522418839`: its fully warmed zero-upload assertion
fails, and owned cleanup still completes. The first C/B/C/B cycle primes exact
post-toggle geometry; only the following identical cycle is called fully warm.
The independently recorded Kitty payload bytes are:

| Four target switches | Before cache correction | After cache correction |
|---|---:|---:|
| Initial post-toggle C/B/C/B | 1,731,072 | 63,456 |
| Fully warmed C/B/C/B | 1,731,072 | 0 |

The remaining first-cycle bytes are four newly required projection rasters,
not source retirement. Fully warm switches transmit zero image chunks in both
dark and light cases. Placement/text output still exists: the final warmed
cycles write about 272 KiB of total VT. This is exact debug byte/resource
evidence, not a release CPU/FPS, hardware-latency or zero-work claim.

Direct physical probes retain the one-texel Kitty/llvmpipe leakage and guarded
fix in `probe-kitty-dark-default-1329281147954674` and
`probe-kitty-dark-default-1329768873736176`. Three physical texels sampling the
middle logical texel preserve exact opaque colors across vertical and horizontal
stretching, without rasterizing a full-length strip. The controller also uses
xterm's native styled print mode 2, removes its `ESC # 5` row prefixes before
coordinate/style decoding, handles inverse cells and waits for complete SVG
artwork rather than treating the first updated foreground as readiness.

`controllers.zip` retains all ten exact controller/fixture/probe sources with
per-member SHA-256 and verified CRC:
`34752491186d2be1289a26a3d8aaa5bcc5c496e90393f4a7d7fa95239ac54370`.
`controller-archive.json` records the member identities. Loose sidebar/probe
scaffolds are removed after verification; raw captures and receipts remain.
Provider-owned forge/PR/CI content is still unavailable, not fabricated.
Animation, command palettes and the separately listed toast/theme/keybinding
work remain outside this pass.

## Sidebar popup anchor and shared material correction

The user's side-by-side screenshots exposed three gaps in the preceding sidebar
pass: a one-row content offset, mismatched item/card/border backgrounds, and tree
guides left over the raised item. Main remains
`9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`; the current retained fork reference is
`d7b40a37a63d4934054516f6f445488348075a62`.
`zellij-server/src/output/recessed_frames/raised.rs` lifts the background when
a surface has an attachment and clears covered guide cells; `joined.rs` gives
item and card matching physical top padding.

Client placement now accounts for the card's framing row so its first content
row aligns with the item, retaining screen-fit clamping and tiny-screen source
visibility. One client-owned raised palette supplies both cell backgrounds and
the existing generic joined material. `ChromeContext::sidebar_popup` carries
the visible target and color explicitly; unrelated/modal rows are not cleared.
RGB popup plates do not also mark their cells as inverse selections: generic
`BackgroundSpan` intentionally preserves selection backgrounds, and changing
that framework contract would regress other widgets. Without confirmed RGB,
native inverse selection remains intact.

Actual immutable debug pair:
`target/smoke/sidebar-alignment/bin-1359830084496647`.

* `bkpr`: `dbf7e859610c1ea7b241dbf507ca0c629c5ea10d7467bdf2fcd9d7f895e0dbab`.
* `bkprd`: `98339bed48ca5bec043e5e5f0857bc745e7524e107e84af0b243b50fef062f54`.

`target/smoke/sidebar-alignment/acceptance.json` records five complete actual
terminal contexts: Kitty dark/light, truecolor xterm dark/light and genuinely
ANSI-only xterm. Every capture checks source/content row alignment and blank
highlighted guide cells; RGB cases check one item/card color, plain cases check
the border-cell backgrounds, and physical guide/rim samples check the displayed
fill. Captures retain hover/keyboard handoff, scrolling, rendering toggles,
three sizes, live palette changes, input ownership and detach restoration.
All cleanups report no surviving owned births. Idle output and fully warmed
popup-switch image payload deltas remain zero.

The affected checks pass: 33 controller/sidebar tests, six popover tests and
113 framework tests; strict all-target Clippy passes for the changed frontend
packages with dependency linting excluded. The initial shared-background
regression caught inverse selection overriding the plate before the RGB-only
selection fix; a separate regression protects native inverse fallback.

Workspace-wide checks encountered the concurrent backend codec migration:
removed snapshot fields, changed typed payload/error APIs, legacy transport
fixture versions and generated-wire lint failures. These are not represented as
passing or relaxed. The immutable runtime pair includes protocol 5; the old
JSON-line smoke adapter was replaced by the real native `bkpr request` utility
using the daemon-advertised version, not by a second fixture codec. Concurrent
backend/app edits mean smoke-start source hashes are not a claim of whole-tree
compiled-source equivalence. All four pre-existing public executable hashes
were preserved by the protected build.

Exact seven-source controllers and fixtures are archived in
`target/smoke/sidebar-alignment/controllers.zip`, CRC and member hashes verified,
SHA-256 `a42b6bbecdb5c080c93a51e846869a1713957869805a162f77dcc1ba50fe1d0a`.
Raw terminal output, actual screenshots, environment/pair receipts and cleanup
records remain; loose owned scaffolds are removed after verification.

### Complete opaque-cell coverage at the sidebar/card bridge

The subsequent user screenshot exposed a narrow gap despite the matching palette
and correct union geometry. The client intersected cached opaque spans with
`window::content`, excluding the card's framing column. Sparse contour graphics
intentionally leave fully opaque interiors to cell composition; rim checks alone
therefore missed this unfilled column.

The graphical popup now projects the cached opaque spans across its full logical
envelope. Retained popup state blanks its framing glyphs in graphical mode and
invalidates painting on readiness/preference changes, restoring complete rounded
text borders without changing metadata, layout, geometry or cached raster work.
The transition regression preserves exact interior content while checking both
graphical padding and restored text corners.

Actual debug pair:
`target/smoke/sidebar-seam/bin-1362142258095532`.

* `bkpr`: `fac98c0b4cf064c47c3c8968fc636c0e83a8e63db3ffb0b6ce2ba1bf582cef29`.
* `bkprd`: `420b289c966c58d6f9ab9a86b82661da71f6f29f6d812a75937cb451ab55e1ad`.

`target/smoke/sidebar-seam/acceptance.json` records dark/light Kitty and truecolor
xterm fallback. The rich cases sample every pixel across the formerly unfilled
column at 23 popup states each, including target switching and three sizes:
2,070 physical pixels exactly match the shared material. Actual screenshots were
inspected; rendering toggles restore rounded borders, input/lifecycle checks
remain intact, warmed image-upload deltas stay zero and no owned births survive
cleanup. These are framebuffer/payload observations, not latency or CPU claims.

The coordinated workspace now passes 720 tests with 14 explicit ignored
subprocess fixtures, strict all-target Clippy and rustfmt. Full output and
protected-pair/source receipts are retained in `workspace-checks.log` and
`build-snapshot.json`. Both affected source hashes remain unchanged after smoke;
all four pre-existing public executable hashes were preserved.
Exact seven-source controllers are archived with verified member hashes/CRC at
`target/smoke/sidebar-seam/controllers.zip`, SHA-256
`2a6aed1c9c1ef422b2b41ec9c663f79dd12b950021704fd0f6b082038784fbb9`.
Loose owned controller scaffolds are removed; raw pixel samples, VT and
screenshots remain.

## Native Planus transport and matched CPU comparison

Native IPC now uses typed Planus wire family 5, including requests, responses,
binary pane scenes and client-lifetime events. `bytes::Bytes` retains the
scene's range in the received allocation without a second scene-byte copy.
Canonical mutable cells/resources still require owned storage; this isn't an
end-to-end zero-allocation claim. External JSON boundaries and private durable
schema 4 are unchanged.

Independent review found that a malformed optional union could become an
apparently absent layout. The failing regression decoded the damaged request
as `layout:null`. Nullable wrapper tables with required union contents now
reject that mutation, and equivalent result/failure corruption is covered.
Complete scene validation, revision checks and resource preflight precede
consumer changes or poll-cursor advancement.

### Native acceptance

`target/smoke/planus-migration/delivery-acceptance.json` records independent
verification of the exact transport-only staged source: 716 tests passed,
14 subprocess fixtures explicitly ignored. The integrated worktree, including
the separately owned frontend changes, passes 720 tests. Both scopes pass
strict all-target workspace Clippy, transport-owned rustfmt and workspace
doctests. Permission-sensitive suites run as the real host user against exact
protected debug artifacts, not inside the build namespace. Concurrent
rendering changes aren't formatted, reverted or included in this commit.

`delivery-tui/` launches that transport-only debug pair in actual Kitty and
verifies the complete 79-row final pane grid, sidebar and exact finite source
bytes. It is functional acceptance, not another performance measurement.

`runtime-1363643375664373/acceptance.json` exercises both incompatible wire
directions: attempted shutdown is rejected while the original session/daemon
identity stays usable through its matching client. Unknown required
capabilities fail before dispatch. The actual PTY fixture preserves NUL,
control, non-UTF-8 and UTF-8 input bytes; emitted combining/wide/ZWJ text,
truecolor, links and exact RGBA pixels survive native scene transport.
Subsequent polls use a real row delta and generation-keyed image receipts;
native resize converges, and final-pane termination reaps its workspace/session.
Cleanup leaves zero owned survivors.

### Matched release CPU

Both release pairs use the same pinned native SDK, thin LTO and frozen client
rendering/framework sources. The JSON control also has the same consumer
resource preflight, with only a read-only image-key accessor added to its old
scene adapter. No production rendering, frame cap, delay or fidelity change
is credited to Planus. Builds use eight jobs and preserve all live public
executable paths. No owned compilation or other owned GUI fixture runs during
CPU capture.

The actual native pane is 240×79 cells. Bkpr has its 280×81 sidebar window;
Zellij uses 240×81. Twelve serial Kitty/Xvfb captures balance variant order
across two repeats per workload/variant. Dense work emits 300 frames at the
same offered 30 Hz, exactly 6,085,154 post-ready bytes. Recorded `cmatrix`
replay uses the same forwarding deadlines and exactly 9,745,690 post-ready
bytes in every run. All complete final terminal grids and byte streams match.

CPU is client plus daemon, with 100% meaning one logical core. Producer,
relay, observer and emulator CPU are excluded:

| Workload | JSON control | Planus | Fresh Zellij control | Bkpr CPU reduction |
| --- | ---: | ---: | ---: | ---: |
| Dense full-screen redraws | 24.87% | 22.48% | 11.19% | 9.6% |
| Recorded `cmatrix` replay | 142.70% | 78.74% | 12.93% | 44.8% |

On replay, client CPU falls from 59.10% to 28.43% and daemon CPU from 83.60%
to 50.31%. These are repeated workload observations, not universal budgets
or physical-display FPS. Faster consumption can change observed intermediate
states; final terminal content isn't a presentation-cadence measurement.

Planus removes native JSON materialization/parsing, but not the remaining CPU
gap: about 2.0× Zellij on dense work and 6.1× on replay. Frame extraction,
comparison/expansion, rendering and per-request connections/workers still
exist; this comparison doesn't attribute their shares. Profile those paths
before selecting another optimization.

The exact source/build/binary receipts are in
`release-build-1362334787277319/result.json`; raw balanced captures are in
`drivers/stages/paired-1362541717915134/`. `findings.json` and
`measurements.json` retain individual counters, byte hashes and limitations.
Every case and the stage watchdog report zero surviving owned births.

## Frame proximity, draggable sidebar and physical resize feedback

The application guide gains one cell on its right after its content/gutter
layout, so existing text does not shift. Other guide contexts keep their
geometry. Sidebar width is an ephemeral per-client preference; dragging its
right edge changes projected view sizes without changing canonical layout
ratios, membership or process identities. Explicit widths clamp to preserve
usable sidebar/native cells and survive terminal resize/hide/restoration.
Input-read-only clients may resize this local viewport, not backend layouts.

Pane frame presses select the nearest movable horizontal and vertical edges
by pane quadrant, retaining split identities and initial grab offsets. Both
axes materialize in one existing exact-revision layout update, with one
in-flight commit and one latest pending coordinate pair. Hover and press use
the same targets; feedback shows the divider spans actually affected by the
authoritative binary layout. Modal/focus/geometry retirement consumes the
remaining pointer gesture rather than leaking its tail to a native pane.

Source ledger: main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`
and retained fork `d7b40a37a63d4934054516f6f445488348075a62`,
especially `zellij-server/src/tab/mod.rs:625–648`,
`tab/mouse_handler.rs:411–472`, `tab/resize_feedback.rs`,
`panes/tiled_panes/resize_feedback_grid.rs`,
`panes/tiled_panes/resize_feedback.rs` and
`output/recessed_frames/hover.rs`. The adapted edge-glyph rules retain the
fork's MIT notice.

Dependency/ownership decision: extend the maintained Termwiz input parser and
existing framework scene projection, not a second input parser, layout engine
or renderer. `bkpr-client` owns sidebar preference, gesture policy and terminal
ANSI-cyan/matte styling. `bkpr-tui-framework::window::paint_edge_feedback`
paints connected glyphs into retained cells; `CellsWithUnderlay` clips those
cells and demotes only intersecting earlier graphics beneath their text.
Sampling, source identities, shadows and bounded renderer caches survive.
The unchanged strip/style/size key skips repainting; no new raster worker,
texture atlas, polling loop or per-hover image generation is introduced.

Verification uses the immutable matching protocol-5 debug pair recorded in
`target/smoke/resize-feedback/build-snapshot.json`. Full workspace tests passed
(741 successful result rows including isolated subprocess tests, 14 ignored),
as did strict all-target Clippy, formatting and the protected debug build.
The build receipt preserves all four public debug/release executable hashes.

Physical evidence under `target/smoke/resize-feedback/`:

- `kitty-dark-default-1365853265807366/` and
  `kitty-light-default-1365895404740844/`: actual rich and normal rendering,
  two-axis near-corner drags, stationary no-jump presses, local sidebar sizing,
  Escape cancellation, default-guide right expansion and modal no-click-through.
- `xterm-dark-default-1365895396393122/`: the same input/geometry behavior
  through the actual rounded-text fallback.
- Full native grids and final rows establish content readiness before immutable
  screenshots. ANSI-cyan text plus physical ink pixels prove feedback on both
  affected axes and the sidebar edge. Each Kitty theme completes eight warm
  hover cycles with zero additional image payload bytes.
- Actual 120×36 → 180×48 → 120×36 resizing, exact native input preservation,
  normal detach, original palette/termios restoration and zero owned-birth
  survivors passed. Read-only behavior is covered by decoded client regressions,
  not misrepresented as an exercised read-only physical fixture.

A separate all-wire-splits regression reproduced a pending legacy Escape being
overtaken by a structured key-release report. Resolving the maintained parser
at complete framed-report boundaries fixes that ordering; it failed before and
passed after. The first physical cancellation driver also had its own defect:
`xdotool --clearmodifiers` synthesized mouse-up before Escape. Retained host
input proves that ordering; the corrected driver preserves the held button.
The diagnostic failure is not attributed to product cancellation.

`acceptance.json`, `final-checks.log`, per-case PNG/text/device/input/cleanup
records and the CRC-verified six-member `controllers.zip` retain reproduction
and source evidence. This is debug software-framebuffer functional/cache proof,
not a release CPU, hardware-presentation latency or FPS claim.

## Shared right padding for every keybind popup

The application-only exception above is superseded: every keybind context now
uses the same final right-edge expansion in `keyguide::render_hints`. The extra
cell remains outside wrapping, centering and key/description spans, and clamps
to the available viewport. This is keybind-card layout policy, not extra padding
for metadata cards or other overlays. Fancy and normal presentation consume the
same resulting geometry.

The context regression covers all fourteen guide contexts, including Ctrl-b,
sidebar, forms, finder, history/search and resize. It checks the added right
cell, screen-edge clamping and unchanged content/role coordinates. Obsolete
default-width and symmetric-frame assertions were removed rather than repinned.
Full workspace tests, strict all-target Clippy and formatting passed.

Actual immutable debug-pair acceptance is under `target/smoke/keyguide-padding/`.
`kitty-dark-default-1368510844961336/` records the default and Ctrl-b rounded
cards, then the same Ctrl-b content coordinates in fancy rendering with physical
right-padding/material samples. The outer sample is in the bevel, not the flat
plate; native terminal text hidden beneath graphics is not mistaken for visible
popup content. Input preservation, normal detach, palette/termios restoration
and zero surviving owned processes passed. The protected build receipt is
`target/smoke/ssh-forwarding/protected-1367244315972282/result.json`.

## Post-Planus scalar and UTF-8 cell extraction

The fresh post-Planus replay profile uses a ten-second, 997 Hz user-space
CPU-clock capture. Inherited counters cover future workers; decoded weights
include only the original daemon/client TGIDs. All 6,637 sampled leaf addresses
resolve across 1,106 daemon threads and two client threads. Rust/Zig
unwinding remains shallow, so inclusive stacks aren't reliable CPU budgets.
Snapshot extraction alone accounts for 23.0% of daemon leaf samples, before
native cell getters. Client composition, style comparison and scene expansion
also remain substantial. These sample weights aren't benchmark CPU percentages.

`Resources::capture` now reads ordinary non-spacer codepoints through the
existing SDK's raw-cell API. Actual clusters use `graphemes_utf8` with one
reusable string instead of a UTF-32 buffer and a second Rust encoding pass.
The previous safe `graphemes_buf` wrapper also queried the length again after
our own query. Raw content tags distinguish scalar text from clusters and are
reused for background provenance. The cluster bound stays 65,536 codepoints;
native spacer handling, Unicode ownership, widths, colors, links, authoritative
Kitty placements and snapshot cadence stay unchanged. No dependency, bindings,
native patch or wire-family change is needed.

### Matched release CPU

The final twelve actual Kitty captures alternate variant/workload order, with
three repetitions per pair and workload. Both pairs use identical frozen Planus
frontend/framework sources, the same SDK and symbolized release settings.
Only the engine extraction file differs. The native pane is 240x79 cells inside
the same 280x81 outer TUI. Dense output offers 300 frames at 30 Hz; the replay
preserves all 9,745,690 source bytes and captured deadlines.

CPU sums only the birth-matched client and daemon user/system counters.
100% means one logical core; producer, emulator and observer CPU are excluded.

| Workload | Original extraction | Scalar/UTF-8 extraction | CPU reduction |
| --- | ---: | ---: | ---: |
| Dense full-screen redraws | 23.06% | 22.16% | 3.9% |
| Recorded `cmatrix` replay | 80.76% | 76.83% | 4.9% |

Every final case passes exact finite-source, native geometry, complete final
Kitty terminal-content and exact-birth cleanup checks. There is no frame cap,
extra delay or slower polling. This is a small CPU improvement, not physical
FPS, presentation-cadence parity or a solved Zellij efficiency gap.

### Correctness and experiment limits

The final shared worktree passes 738 top-level tests, plus one isolated
subprocess success, with 14 explicit ignored fixtures. Strict all-target Clippy
and workspace formatting pass. Tests execute as the real host UID; only
compilation uses protected private output mounts. Builds use eight jobs on
CPUs 0-7, with 3/4 GiB memory pressure/limit and no swap. Existing public
executables remain byte-identical across those protected builds.

Actual private-daemon comparison preserves the entire original Unicode frame:
an 81-byte combining cluster, subsequent ASCII, native ZWJ segmentation and
widths, empty continuations and trailing cells. Initial smoke assertions wrongly
assumed a joined two-cell ZWJ emoji; the pinned parser's original and candidate
frames both split it identically. The corrected full-frame comparison is kept
alongside those failed observations, without changing parser behavior.

A style-ID lookup cache was rejected: dense CPU improved, but all three replay
samples worsened. The intermediate UTF-8-only extraction and its valid profile
are retained separately. An additional final profile failed at the perf ACK-pipe
startup handshake before releasing the workload gate; it has no surviving owned
processes and supplies no final hotspot-shift claim. The fresh initial profile
and unprofiled final CPU measurements remain independent evidence.

Evidence is under `target/smoke/post-planus-profile/`: `findings.json`,
`matched-scalar/result.json`, `final-checks-summary.json`,
`scalar-unicode-differential.json`, immutable pair manifests and archived
controller/source captures. No owned compilation or other owned GUI fixture
runs during CPU captures; live sessions, user settings and providers aren't used.
The next measured candidates are control-worker scene/run scratch allocations,
native dirty-row reuse and client composition/style comparison.

## Sidebar focus graphics continuity, hidden cursor and artwork layering

Three independent boundaries caused the reported focus behavior:

- `SvgGraphics::configure` discarded all published icons/contours when a marker
  color changed. It now retains entries matching exact RGB/dimensions and
  contours matching exact style/cell geometry while preparing missing variants.
  Matching undrained publications survive superseding requests. Genuine style,
  geometry, capability and source-generation invalidation remains enforced.
- Renderer image retirement, projection pruning and placement deletion preceded
  synchronized-update begin. All presentation-affecting deletion now occurs
  inside that atomic frame; unchanged native resources and bounded inactive
  projections retain their existing residency rules.
- Sidebar input focus did not own a cursor position, and rendering restored
  cursor position only when visible. The client now anchors its hidden cursor
  to the highlighted rendered icon row. The framework preserves valid logical
  coordinates beneath passive plates while masking visibility, and emits final
  CUP even for a valid hidden cursor. Later modal focus overrides it normally.

The hive's unchanged main geometry starts half a cell into the framing column.
Joined glyph exclusion begins one column farther right, so the later contour
covered that ink. A separate resource-only artwork frame now follows passive
materials and precedes modal contents/effects. It shares prepared images, does
not clone the cell canvas, and changes neither SVG artwork nor margins, style,
input ownership or normal fallback.

No new graphics/parser/cache dependency or competing output loop was added.
These are narrow extensions of the existing resvg preparer, retained scene and
maintained terminal input/output infrastructure. Generic publication, atomicity
and cursor mechanics remain in `bkpr-tui-framework`; sidebar focus and artwork
layer selection remain in `bkpr-client`.

Worktree-local evidence: `target/smoke/sidebar-focus/`. The frozen old debug pair
is `keyguide-padding/bin`; the matching fixed pair is `sidebar-focus/bin-final`.
The final protected receipt is
`ssh-forwarding/protected-1393559136031107/result.json`. Workspace tests,
strict all-target Clippy and formatting passed, including exact resource
identity/style invalidation, undrained publication, emitted VT ordering, hidden
cursor clipping/passive occlusion, modal/native restoration and idle silence.

| Actual dark Kitty workload | Old pair | Fixed pair |
| --- | ---: | ---: |
| Image deletions outside synchronized frames, sixteen focus changes | 2,712 | 0 |
| Selected workspace hive's opaque gutter pixels visible | 0 / 16 | 16 / 16 |

`kitty-dark-default-1393999999387411/` records the old behavior;
`kitty-dark-default-1394000013915551/` and
`kitty-light-default-1394123325809422/` record the fixed behavior. Actual XCB
framebuffer sampling covers hive/bee ink and six border regions through the
focus burst. The old sample records thirty regional opaque-ink/border losses;
the fixed dark/light samples record none. Full RGB differences are retained:
popup backing, shadow and antialias pixels can change without losing opaque
SVG ink, so those differences are not silently discarded or called disappearance.

Kitty's actual `get-text --add-cursor` reports hidden positions on workspace,
pane A and pane B at their expected rendered rows, native position on return,
and a visible cursor only for the focused form. Light-mode acceptance also
switches to normal rendering and repeats hidden workspace/pane/native checks.
The complete expected native content establishes initial readiness before
immutable references; screenshots, raw VT/device bytes, exact input logs and
zero-survivor cleanup remain retained.

These are functional debug software-framebuffer observations, not a frame-rate,
hardware-presentation latency or zero-upload claim. Popup sources still retire
when absent; this fix does not silently retain unbounded hidden resources.

## Scene encoder response-local scratch reuse

The encoder previously collected a row's `Vec<Run>` before serialization.
Each run allocated its own `String` and layout vector, including a layout
allocation that the ASCII fast path immediately discarded. The measured
control-worker allocator cost motivated removing those allocations rather than
replacing them with a cheaper allocator.

`Run::finish` now writes each completed run directly into the existing Planus
builder. One text buffer, one layout buffer and one offset vector retain their
capacity across runs and rows within that response. All capacity is released at
the end of encoding, not retained in a global or cross-client pool. Layout
entries, not text length, identify a pending run because empty-text cells still
carry meaningful widths/counts. Style/link interning, full validation, resource
receipts, delta baselines and the wire family remain unchanged.

Standard `String`/`Vec` reuse and the maintained Planus builder provide the
needed ownership guarantees. No pooling crate, new dependency, native patch,
frame cap, delay or slower polling is needed.

### Matched release CPU

Both pairs use the signed `e13b4e3` backend baseline and the same frozen
frontend/SDK. Only the encoder and its test source differ. This deliberately
excludes the separately committed frontend graphics fixes from the comparison.
Twelve actual Kitty captures alternate variant/workload order, three runs per
variant/workload, with the same 240x79 pane, 280x81 outer TUI and exact finite
source bytes/deadlines. CPU sums only client and daemon user/system counters;
100% is one logical core.

| Workload | Per-run buffers | Response-local buffers | Interpretation |
| --- | ---: | ---: | --- |
| Dense full-screen redraws | 21.80% | 21.63% | Essentially unchanged |
| Recorded `cmatrix` replay | 76.30% | 67.42% | 11.6% less total CPU |

The replay daemon accounts for nearly all the reduction; client CPU is
unchanged within variation. Every candidate replay sample is below every
control sample. All twelve cases pass exact-source, geometry, complete final
Kitty content and zero-owned-survivor checks. These aren't physical FPS,
presentation-latency or universal workload claims.

### Allocation and semantic proof

A throwaway System-backed allocator counts calls only during encoding of
prebuilt 240x79 fixtures. This isn't CPU timing or peak-memory measurement.
Both encoders produce the same encoded lengths in these fixtures.

| Fixture | Old allocations | New allocations | Old reallocations | New reallocations |
| --- | ---: | ---: | ---: | ---: |
| Dense, single style | 420 | 107 | 400 | 10 |
| Highly fragmented | 38,196 | 6,441 | 479 | 11 |

An independent archived encoder/decoder oracle passes 484 cases, including
1,936 complete-frame comparisons, 1,452 diagnostic comparisons and fourteen
validation/error cases. Coverage includes independent style/link transitions,
empty links, zero-byte/zero-width rows, row-edge identity changes, long Unicode,
all style/source/cursor fields, deltas and image generation receipts. One
permanent regression preserves the previously uncovered alternating-run and
empty-row boundary behavior.

Actual private old/new daemons produce identical complete native frames and
scene diagnostics through three phases. Exact NUL/control/UTF-8/invalid-UTF-8
PTY input, image replacement/deletion, final-session closure and cleanup pass.
The integrated worktree passes 748 top-level tests plus one isolated subprocess
success, with fourteen explicit ignored fixtures, strict all-target Clippy and
workspace formatting. Builds use eight jobs/CPUs and the existing native prefix
and Cargo cache; protected builds preserve public executables. No owned
compilation or other owned GUI fixture runs during CPU captures.

Evidence is in `target/smoke/run-buffer-reuse/`: `findings.json`,
`matched/result.json`, `verification-summary.json`, complete compressed codec
records, native phase receipts and archived reproduction sources. No live user
session, provider or persistent setting participates.

## Atomic pane creation and placement

Native wire family 6 replaces single-spec `pane_spawn` with a batch of named
process specs and an explicit final layout. Existing and new leaves occupy
distinct namespaces; the result maps local references to committed pane IDs.
Workspace/window creation, reopen and restore retain their public shapes and
share the same reservation/start/commit engine. TUI splitting uses one request,
not a visible default layout followed by a separate placement CAS.

The complete structural request, ownership/CAS, capacity and bounded durable
candidate are validated before child setup. Every HOME/CWD is prepared before
the first child executes; startup and rollback never hold global registry
locks. A workspace reservation lasts through final validation, durable write,
record/live-handle publication and callback installation. Unrelated workspace
changes merge at commit. Partial startup or failed durable commit reaps
unpublished children without changing the visible layout or consuming older
restore intent. OS command side effects and directory preparation are not
atomic or reversible. Failed persistence still poisons all commit paths; an
error after rename/fsync does not prove an unchanged durable file.

**Debug acceptance**, not a performance claim:

* `target/smoke/atomic-pane-placement/verification/compiled-1397815169830048/manifest.json`
  freezes the matching family-6 pair: client SHA-256
  `be59df2ef2b7f644783e5f94b42eeed645c487ab598c31e270fa07aec261f92c`,
  server `4b576a624034dc40d5782121b331841b82c31c2b9ca6ddf32db9b655d6ef1da3`.
  Public debug/release executables remain byte-identical. Builds use the local
  incremental target, eight jobs/CPUs, a 4-GiB memory ceiling and no release build.
* Workspace tests pass as host UID 1000: 768 top-level successes, one additional
  isolated-subprocess success and fourteen explicitly ignored fixtures.
  Formatting, strict all-target Clippy, doctests and the protected debug build
  pass. Deterministic regressions cover admission before execution, later
  filesystem prerequisite failure, ownership/ref/layout boundaries, batch
  capacity, partial exec failure/reaping, unrelated-workspace responsiveness,
  final CAS, shutdown, early exit, persistence poison and retained restore intent.
* `run-1398246679808064/acceptance.json` passes nineteen actual CLI/backend cases.
  Real Kitty TUI left/right and top/bottom splits each advance their window
  revision once. The attached client then shows an existing pane left, Codex
  top-right and Claude bottom-right from one batch request. Production `bkpr run`
  launchers authenticate both capture kinds using private credential-free native
  stand-ins; no model, conversation readiness or notification readiness is claimed.
* Wait subscribers record five horizontal, five vertical and eight multi-pane
  snapshots. Each target has exactly two observed layouts: old and final, with
  consecutive window revisions and no subscriber error. These samples are
  observation evidence, not an OS-atomicity proof; the single publication and
  barrier regressions enforce the intermediate-state invariant.
* Stale revisions, invalid ownership/references/layouts and invalid later specs
  reject before execution. A later exec failure returns error with no new
  registry panes/layout; the first command's filesystem side effect was observed,
  demonstrating why process rollback is not side-effect rollback. Failed durable
  publication returns error, rejects subsequent writes, keeps immutable identity
  readable and allows targeted cleanup while preserving its real failure exit.
  All final fixture cleanup records contain zero surviving owned process births.

The first native controller incorrectly assumed global `--pane` selected a TUI
workspace; the CLI uses that flag for headless pane tools, while TUI startup
selects its first usable workspace. Its multi-pane screen wait timed out despite
correct backend layout/capture state. The corrected controller uses ordinary
workspace navigation and keeps the actual client attached throughout creation;
no production behavior was relaxed. `final.xwd` was converted with ImageMagick
to `final.png` and visually inspected.

`verification-summary.json`, the raw check/runtime receipts, actual framebuffer,
`checked-sources.zip` and archived controller sources live under
`target/smoke/atomic-pane-placement/`. Fixtures use private HOME/XDG/socket roots,
deny Internet sockets and never touch live sessions, user profiles or providers.

## Remote-session backend and headless connection

The first remote milestone connects to **one existing named session**. There is
no local federation daemon, remote bootstrap/install/restart, TUI integration or
mixed-session workspace composition. The owning remote daemon retains process,
layout, harness, recovery and metadata authority.

`bkpr_client::remote::RemoteSession` owns one system OpenSSH child and exposes a
concurrent native `Endpoint`, pinned session information, lifecycle polling and
disconnect. `bkpr remote` provides identity-only `inspect`, persistent JSON-line
`connect` and one-shot `request`. Inspection never registers or activates a
native client; only persistent connection accepts takeover. Normal requests are
managed attachments, not an administrative fallback.

One remote `bkpr __remote-helper --stdio` opens every native Unix socket for an
attachment, preserving the native helper-PID/birth-bound authorization contract.
Creating a separate SSH command per native RPC would change that peer identity.
Native harness callbacks remain remote-pane callbacks; the remote endpoint
rejects local harness/agent authority before opening a transport channel.

System OpenSSH supplies authentication, strict host-key trust and user host/
identity/ProxyJump configuration. Maintained Yamux 0.14.1 multiplexes the binary
stdio stream; a current-thread Tokio runtime is confined to the connector/helper,
not the native daemon. Native family 6 and durable schema 4 are unchanged.
A separate bounded Planus remote-control family 1 negotiates the exact session,
daemon identity, capabilities, connection nonce and heartbeat lease.

Admission reserves 79 observation streams, 15 controls and one native lifetime
stream, alongside the mux control stream. The shared receive window is 24 MiB,
copy chunks are 16 KiB and retained SSH stderr is limited to 16 KiB. Independent
heartbeat send/read deadlines enforce a remote lease without depending on sshd's
keepalive configuration. Disconnect revokes views and drains accepted mutations
under a bounded deadline; network loss never fabricates success or triggers
reconnect/replay. SSH stderr is drained after child exit under its own deadline.

Forwarding is off even if SSH config enables it. Explicit forwarding uses the
remote OpenSSH socket and the existing native attachment-selected agent route,
not a local pathname or a second agent bridge. The SSH child cannot inherit
local `BKPR_*` authority or context. A real hostile SendEnv/AcceptEnv fixture
reproduced the leak before that scrub; evidence retains variable names only.

**Exercised debug acceptance**, not WAN or rendering-performance measurement:

* `target/smoke/remote-sessions/verification/compiled-1407002429921643/manifest.json`
  freezes client `a86d4846f260d5e03101017224fa7178d18945953a982bee17a778f4dc156de0`,
  server `9ec81984c84e15815cfc0705e6a93265f99937f1f8a516838469f6e874df7d00`
  and the standalone public-SDK probe compiled from exact Cargo JSON artifact
  paths. The 178-file source capture includes the shared frontend snapshot before
  later toast work; that later work is not covered by these checks.
* Formatting, strict workspace/all-target Clippy, doctests and protected
  incremental debug compilation pass. Host-UID1000 workspace execution passes
  805 top-level tests plus one isolated subprocess, with fourteen explicit
  ignored fixtures. Builds use eight jobs/CPUs and the existing local target/SDK.
  Protected runs preserve their current public debug/release executable hashes.
* `actual-ssh-identity-acceptance/acceptance.json` passes all 27 required cases through
  actual private OpenSSH/sshd. The daemon is the **pre-existing compatible**
  family-6 atomic-placement build, SHA-256 `4b576a624034dc40d5782121b331841b82c31c2b9ca6ddf32db9b655d6ef1da3`.
  Session ID, daemon identity/birth and original pane birth survive every
  disconnect, takeover, helper death and application partition without restart.
* Exact Unicode/slash names, absent/stopped/unsafe/symlink targets, changed host
  keys, missing/noisy/incompatible helpers and actual-PTY helper refusal pass.
  Inspection succeeds at all 32 occupied native client slots without changing
  their IDs, views or the original pane's cryptographically verified agent source.
* Canonical binary scenes, ordered key/raw/paste input, exact binary clipboard
  bytes, atomic two-pane placement and real pane-native context callbacks pass.
  With 31 registry waits and 48 pane polls held, the next observation rejects
  admission while real key/input and lifecycle control remain responsive.
* Default forwarding refuses signing despite private SSH config enabling it.
  Explicit forwarding signs from the original remote pane; the disposable public
  key verifies the signature before and after nonactivating inspection.
* Both the local SDK and its real SSH child are stopped by exact-birth pidfds.
  The remote helper exits and native views retire in about 5.05 seconds while
  both local births remain stopped and sshd remains running. Original panes
  survive; stale input is rejected without changing received PTY bytes.
* A gated reply is lost only after direct native inspection confirms one durable
  workspace creation. The caller reports unknown outcome, does not replay and
  reconnects with fresh client/view identities. The persistent CLI reuses one SSH
  child/helper for multiple commands and leaves panes alive at EOF.
* A separate owned daemon restarts at the same named socket with the same logical
  session and a new daemon identity. The old endpoint cannot create a workspace
  on that replacement; explicit fresh attachment gets a new client identity and
  succeeds. The original compatibility-test session and its pane stay running.

The SSH fixture uses a private user/mount namespace and owned chroot, separate
local/remote HOME/XDG roots, private accounts/keys/known-hosts and real PTYs.
Permission-sensitive workspace tests run separately as host UID1000, not that
namespace's UID0. Final inner/outer receipts have zero owned survivors, no
forwarded sockets and removed private credentials/rootfs. No live session,
provider, real signing identity, user shell profile or persistent setting is used.

Evidence, exact source hashes and archived reproduction controllers are under
`target/smoke/remote-sessions/`. This is loopback OpenSSH acceptance, not a second
physical host/WAN, macOS, remote TUI, or genuine native-model/harness onboarding
claim. Genuine copied harness-token proof is explicitly unexercised here;
existing authenticated capture/peer-binding regressions pass in the workspace
suite. Those limits do not gate the scoped connection milestone.


## Framework toasts and transient TUI notices

`bkpr-tui-framework::toast::ToastStack` owns bounded notification lifetime,
retained layout/text, rounded fallback cells, dismiss hitboxes and complete
pointer gestures. Bkpr-client supplies the five-second lifetime, top-center
placement, theme and existing raised-popup shadow/bevel material. Transient
clipboard, input, operation, resize and rendering-mode notices no longer occupy
the sidebar or pane title. Pending operations, live resize instructions,
correlated editor errors and consent decisions remain contextual state.

The stack retains at most three cards, evicting the oldest during bursts.
Messages, wrapping and geometry are bounded; truncation is visibly marked.
Insertion owns the deadline, not painting. The application expires cards and
adds their next deadline to its existing poll timeout; there is no animation or
periodic forced repaint. Cards never take keyboard focus. Dismissal consumes
the complete pointer gesture, including expiry, eviction and chorded buttons.

Reuse decision: retained `UiSurface`, `window` helpers and the existing scene
and decoration path already supply composition, clipping, styling and output.
Existing maintained Unicode segmentation/width libraries handle text boundaries.
No dependency, renderer, backend service or separate output loop was added.
The SGR framing boundary now preserves button/action identity alongside
Termwiz's coordinate/modifier decoding. This fixes chorded release ambiguity
without expanding the maintained decoder's supported mouse grammars.

Actual isolated debug acceptance used the immutable client
`540562566909b06a78d8b719184dfb0d80c3732c2f2d38f53c42b58036d1adce`
and daemon
`9ec81984c84e15815cfc0705e6a93265f99937f1f8a516838469f6e874df7d00`.
The final test-only cleanup rebuilt to those identical executable hashes.

* Kitty 0.49.2, private Xvfb/llvmpipe, dark and light palettes, 120×36,
  9×18-pixel cells: complete fancy and normal toast surfaces, approximately
  five-second expiry without input, body-click consumption, immediate `[x]`
  dismissal, exact native keyboard delivery while visible, and held/chorded
  releases after disappearance all passed.
* Actual xterm, private Xvfb, 120×36, 9×19-pixel cells: the same interaction
  checks passed with rounded text borders and no graphics requirement.
* Detach restored the owned terminal; all fixture cleanup receipts have zero
  surviving owned process births. No live sessions, model/provider services,
  shell profiles, approvals or persistent settings were used.

Receipts and inspected full-frame screenshots:
`target/smoke/toasts/kitty-dark-default-1410887779021660/`,
`target/smoke/toasts/kitty-light-default-1410924061676883/`, and
`target/smoke/toasts/xterm-dark-default-1410969164632241/`.
The frontend source scope, protected build manifests and reproduction controllers
are retained under `target/smoke/toasts/`. Existing uncommitted latency work is
included in the source capture and preserved, not presented as part of the toast
design or as a resolved sidebar-focus regression.

These are functional physical-surface checks, not a latency benchmark or desktop
GPU acceptance. The previous sidebar-focus investigation remains separate.

Final coordinated verification: workspace tests passed **823**, with **14
ignored**; all-target strict Clippy, formatting and doctests passed. Host-UID
runtime checks and cleanup are recorded in
`target/smoke/toasts/verification/checks-1411196131407918/result.json`.
The compiled test/runtime manifest is
`target/smoke/toasts/verification/compiled-1411112587497522/manifest.json`.
Protected builds preserved their entry debug/release executable hashes.
`target/smoke/toasts/verification-summary.json` links exact source scope,
terminal receipts and archived controllers. Loose toast controllers were removed
after archival.

## Authoritative daemon spawn defaults

The native `SessionInfo` control result now includes optional `SpawnDefaults`
beside the unchanged logical session/daemon identity. A validated `OnceLock`
holds startup command argv, raw-path cwd/home and isolation policy. This is not
a full environment export, persisted launch recipe or inference from pane argv.
No new operation, native family, remote-control schema or durable version is
needed: the existing Planus control-value representation carries the DTO.

The daemon captures its explicit current command, or its own `SHELL` for
headless startup, together with selected HOME/cwd/isolation. Incomplete headless
configuration stays unavailable without starting programs or guessing paths.
Remote clients read this before activation. Native token/peer/birth and connected
lifetime authentication still apply; only immutable `SessionInfo` bypasses the
inactive-client and durability-health gates. Normal input/poll paths gain no new
lifetime syscall.

The shared protected build at
`target/smoke/remote-tui/verification/compiled-1415410320870103/manifest.json`
passes formatting, strict all-target Clippy, doctests and debug compilation.
Host-UID1000 workspace execution passes 837 top-level tests, one additional
isolated subprocess and fourteen explicitly ignored fixtures. Native regressions
exercise preactivation reads, refused inactive mutations/views, forged authority,
lifetime retirement, raw paths, immutable/nonpersisted policy and real persistence
poisoning through framed native sockets.

`target/smoke/remote-spawn-defaults/actual-native/acceptance.json` records actual
CLI/daemon/process proof as host UID1000 with Internet socket creation denied.
Both normal and isolated headless daemons publish exact configuration without
executing the configured command. A process created from the returned defaults
records the expected cwd/HOME and observable environment-preservation/scrubbing
behavior, despite different caller HOME/SHELL. A different populated execution
does not change policy; missing daemon `SHELL` returns null defaults while the
headless daemon remains usable and empty. Cleanup has zero owned survivors.

`source-capture.json` and `checked-backend-sources.zip` freeze the seven backend
files; archived smoke sources and `verification-summary.json` retain reproduction
and shared-check provenance. This is native backend acceptance, not a claim that
remote TUI physical-surface verification has completed.

## Direct remote TUI attachment

The launcher now routes bare `bkpr -r HOST` directly into
`bkpr_client::run_remote`, before local roots, state discovery or daemon startup.
`-s/--session` is a literal remote name with default `bkpr`;
`--remote-bkpr-bin` selects an already-installed helper. `--ssh-config` and
explicit `--forward-agent` retain the shared OpenSSH endpoint policy. The old
`--ssh` and `--helper` flags are removed, including for headless commands.

Ownership decision: reuse `RemoteSession`/Yamux and the normal client attachment,
input, rendering and shutdown paths. No new transport/dependency, local gateway,
frontend emulator, federation cache or mixed-session ownership layer was added.
The local terminal owns capabilities, theme, clipboard and presentation. The
remote daemon owns execution, filesystem paths, layouts, processes and metadata.
The client requires valid immutable remote spawn defaults, read before activation
or `-d` takeover. Empty sessions stay empty until the user explicitly creates a
workspace; those first processes use the remote daemon's policy, not local
`HOME`, `SHELL` or CWD. Missing defaults fail closed.

The first integrated protected incremental debug build is
`target/smoke/remote-tui/verification/compiled-1415410320870103/manifest.json`:
client `c1e91c563d827db5e298145dd6abac02487eacb3ad2fc652013745095f2d5fcc`,
daemon `619a6071ff6e0906f21dfea9873ff688f700b1c9e86e76468d821fdd493c3423`.
Formatting, all-target strict Clippy, workspace test builds/doctests and the
host-UID test run in `verification/checks-1415518211391925/result.json` passed.
That integrated working-source build also contains the separate latency WIP;
publication verification uses a scoped source view without that WIP, recorded
in `target/smoke/remote-tui/publication-verification.json`.

The latency-excluded toast prerequisite passed all 828 top-level tests plus one
isolated subprocess test (14 ignored), strict Clippy, formatting, doctests and
actual local fancy/plain expiry/dismiss/input checks. The remote publication
pair is client `e965e60325d38eaaf77756d4a5131797ed8e43e34bf20648fb026cdf46947141`
with the same daemon above, frozen in
`verification/compiled-1417665257873434/manifest.json`. Its strict compile checks,
dark/light remote matrix and local toast regression passed:
`kitty-dark-default-1417791993433915`, `kitty-light-default-1417873519077277`
under `target/smoke/remote-tui/`, and
`target/smoke/toasts/kitty-dark-default-1417907828645694`.

Its full-suite rerun did **not** pass:
`verification/checks-1418016847865801` records the backend
`stale_observation_cannot_replace_newer_authenticated_native_binding` test
panicking at `capture/tests.rs:966` because `automatic[pane_id]` was absent,
followed by poisoned-state/destructor panics and the harness deadline. That
backend executable is byte-identical to both earlier passing workspace runs;
the cause is not established by the frontend work. All subsequently unreached
targets, including framework, client and CLI, passed separately in
`verification/checks-1418713600928277`. The failed backend target was not retried
or omitted from the reported result. Every fixture cleanup has zero owned
survivors. Signed publication was initially blocked by an unresponsive live
Bkpr SSH-agent proxy; signing was not disabled.

The later [stable authenticated-capture fixture repair](#stable-authenticated-capture-fixture)
passed the complete latency-excluded workspace: 835 top-level tests plus one
isolated subprocess, 14 ignored, strict Clippy, formatting and doctests.
`target/smoke/capture-recovery/verification/checks-1420780905092158/result.json`
records the new run after the fixture change, without erasing the earlier
failure. Its client/daemon pair is byte-identical to the physical remote TUI
proof above. Runtime foreground validation and recovery policy are unchanged.
At that verification handoff, only signing-agent connectivity blocked publication.

Actual remote GUI receipts:

* `target/smoke/remote-tui/kitty-dark-default-1416576825976312/acceptance.json`
* `target/smoke/remote-tui/kitty-light-default-1416936250987730/acceptance.json`
* Existing local TUI regression:
  `target/smoke/toasts/kitty-dark-default-1416936260897703/acceptance.json`.

These run the real system OpenSSH 10.5p1 client/server, one no-PTY helper and an
already-running matching daemon in a private owned rootless chroot. Kitty/Xvfb
and the TUI run outside that chroot as host UID1000; remote CWD/home directories
are absent from the local filesystem. Generated private keys, strict known-host
checking, separate HOME/XDG roots, controlled PTY programs, no user profiles and
no model/provider calls isolate the fixture. The surface is 120×36 cells at
9×18 pixels, with complete controlled Unicode content; dark/light themes cover
fancy and rounded plain rendering. This is functional evidence, not a
full-screen editor, WAN or latency benchmark.

The observed path covers remote session/SSH identity, exact key and bracketed
paste bytes, local clipboard writes through native Bkpr consent, split creation,
authoritative keyboard resize, separate terminal-window creation, ordinary
second-client attachment, successful third-client takeover, safe failed
initialization before takeover, detach and terminal-mode restoration. It also
checks explicit first-workspace creation in an existing empty remote session,
missing-session refusal, killed-helper failure, a stopped SSH transport reaching
remote lease expiry, no post-failure input replay, and surviving remote programs.
The final light run additionally waits for every remote view to retire.
No local daemon, state directory or socket is created despite a deliberately
inherited local `BKPR_SOCKET`. Cleanup verifies owned process identities and
removes private credentials and roots.

A clipboard-read attempt reached Kitty's own permission prompt. The fixture did
not approve it or change host permissions, so host clipboard-read completion is
not claimed. The completed clipboard proof is a remote OSC 52 write observed
byte-for-byte in the private local X11 clipboard, with no consent keystroke
leaked to the remote PTY. Explicit forwarding retains the earlier backend SSH
proof; the GUI matrix does not claim a new forwarding/signing test.

Transport failure revokes client authority and exits with the original error;
it does not manufacture an empty layout or silently reconnect/replay. Detach
and failed attachments do not own the remote daemon or its programs. Separate
latency investigation changes remain uncommitted and are not part of this
feature or its performance claims.

## Stable authenticated-capture fixture

The remote TUI workspace run in
`target/smoke/remote-tui/verification/checks-1418016847865801` remains a recorded
failure, not a successful run obtained by retry. Its first missing automatic
recovery entry was followed by an assertion while holding the registry mutex;
that poisoned subsequent shutdown and destructor cleanup.

Instrumented native RPC/process fixtures subsequently captured a concrete
eligibility failure in `target/smoke/capture-recovery/`:

* `stress-1420069003088815/core-04.log` records an accepted duplicate binding
  whose foreground predicate returned `ENOENT`, followed by automatic-intent
  removal at `registry/recovery.rs`. Later bindings restored the intent.
* `stress-1420348927882942/core-07.log` identifies the foreground-classification
  failure on a descendant whose PID, birth, parent, controlling PTY and process
  group were still present. `core-44.log` records the corresponding binding
  classifier failure and missing readiness marker.

The fixture's copied native shell repeatedly forked `sleep` every 20 ms while
the backend inspected its descendants. Replace that churn with the shell's
blocking `read` builtin and a retained private `ChildStdin` pipe. A separate
marker follows completion of the nested rejection fixture before any native
binding. Keep the real callback authentication, process inspection,
background/restore transitions and durable automatic-binding assertions.
Snapshot the asserted state under its mutex, then release it before asserting.

The original run did not record its foreground predicate, so the exact original
event cannot be proven retroactively. The observed classifier failures justify
stabilizing the fixture, not relaxing production authority checks or retrying
bindings until accepted. Production process classification, foreground
validation, automatic-resume policy, wire formats and daemon binaries are
unchanged. Temporary diagnostics are archived and removed from source.

With identical diagnostics, the stable fixture passed all 512 isolated cases
in `stress-1420505590085300`: 2,560 authenticated native bindings, zero rejected
foreground predicates or classifier errors, and zero surviving owned process
births. The run used the actual host UID1000, four concurrent fixtures on
CPUs0–7, private HOME/XDG and sockets, incremental debug artifacts and no
provider calls.

After removing all temporary production-source diagnostics, the scoped
latency-excluded feature source passed formatting, all-target strict Clippy,
workspace test compilation, debug builds and doctests:
`target/smoke/capture-recovery/verification/compiled-1420716408307149/manifest.json`.
The frozen host-user workspace run in `verification/checks-1420780905092158`
passed 835 top-level tests plus one isolated subprocess test, with 14 ignored
and zero surviving owned process births. This is a new run after a fixture
change; it does not overwrite the original failure.

The rebuilt client SHA256 remains
`e965e60325d38eaaf77756d4a5131797ed8e43e34bf20648fb026cdf46947141`,
and the daemon remains
`619a6071ff6e0906f21dfea9873ff688f700b1c9e86e76468d821fdd493c3423`.
They are byte-identical to the pair already exercised by the isolated remote
TUI matrix. Public debug/release binaries and foreign frontend/latency sources
were preserved; the frontend-owned Git index was not touched.

## Discoverable shared control

The native CLI and launch-local MCP now share one typed operation catalog in
`bkpr-protocol::control`. Schemars 1.2.3 generates argument/result schemas from
the existing Rust contracts. Help/schema resolve locally; native calls use the
existing binary transport and registry dispatcher. This adds no second spawn
path, terminal parser or agent-specific management subsystem.

`bkpr api help`, `bkpr api schema OP` and
`bkpr -s work api call OP --arguments JSON` expose existing inspection,
workspace/window/pane lifecycle, atomic pane batches with request-local layout
references, layout CAS and native recovery. Unmanaged calls require an explicit
endpoint. Managed calls retain their launch authentication and default to the
live execution workspace; explicit session/all scope still stops at one daemon.
Repository-scoped and cross-daemon control are not advertised.

The MCP has nine shared tools: the existing seven communication/identity tools,
`control` and separate explicit `terminal_input`. Control uses
`{operation,arguments,scope?}` rather than the recovery-only request envelope.
`pane.read` returns canonical visible-frame text and terminal state, not
scrollback, image contents or output subscriptions. Text, bracketed paste, raw
bytes and native key events never implicitly append Enter. Temporary private
views do not borrow a human client's dimensions, focus or clipboard grants.
PTY acceptance is not application consumption or harness readiness.

Scope checks filter registry results and validate target identities before
execution and publication. Process/foreground observation stays outside the
registry writer lock, with each batch child revalidated before spawn. Retained
launch lifetime descriptors and synchronous End revocation fence queued native
input at writer admission without procfs scans on the input path. Client
takeover also fences private views and queued input. Accepted input receipts
survive view cleanup; typed failed/unknown outcomes survive credential
redaction and relay transport without classifying arbitrary error text.

Native wire family 6, remote control 1 and durable schema 4 are unchanged.
Older running daemons may reject newly exposed operations; callers do not
restart them or replay ambiguous mutations. The MCP envelope change is a clean
cutover, not a compatibility alias.

The isolated runtime proof is
`target/smoke/control-api/actual-native-final/acceptance.json`. It uses the real
launcher, daemon, launch authentication, CLI and newline-delimited MCP adapter.
A private native executable supplies callbacks without invoking Claude or a
provider. Seccomp denies Internet sockets; HOME/XDG, sockets and process
identities are private. This proves the Bkpr transport/control path, not genuine
harness consent or model execution.

The smoke exercises local discovery without a daemon, truthful pre-dispatch
errors, nine-tool MCP discovery, default-workspace refusal and explicit
session-scope access, atomic two-pane final placement, stale layout revision,
complete viewport reads and shared CLI/MCP dispatch. Text, bracketed Unicode
paste, Enter and raw bytes including `0`/`255` reach the PTY byte-for-byte.
Repeated reads/input leave no private views, dimension changes or human-view
authority behind. Exact-birth cleanup reports zero surviving owned processes.

The final protected debug build, formatting, all-target strict Clippy, workspace
test compilation and doctests passed in
`verification/compiled-1454534904742265/manifest.json`. The frozen host-UID1000
run in `verification/checks-1454602212097961/result.json` passed 858 top-level
tests plus one isolated subprocess test, with 14 ignored and no surviving owned
process births. Runtime pair SHA256:

* `bkpr`: `15fe5120ca4cf8cb2bbae8f9659faed02880e6b58394d7b5c42ba710bed6c283`
* `bkprd`: `337fbecab885f43f33a71776689d108bf1f87be6f61913f127ac9b8512bd12fa`

The first full run remains recorded as failed in
`verification/checks-1454369035243334`: one new End-vs-resume test failed before
its reservation barrier while using a fork-heavy fake-agent loop. Only that
fixture changed to the existing blocking-shell pattern; production checks were
not relaxed. Its rebuilt runtime pair is byte-identical to the smoke pair.

Earlier compilation timeouts and an undefined-hidden-symbol linker failure
remain in their original logs. Matching the failed code-generation unit against
incremental work-product maps identified one `bkpr-core` library cache entry.
It was reversibly quarantined, not deleted; rebuilding that library took
11.2 seconds and the subsequent full compilation passed. No workspace cache
clean, target-directory split, release build or resource-limit increase was
used. The original public-output guard failure also remains recorded; no saved
binary was restored over a changed output. Final guards preserve all four
entry debug/release hashes. The concurrent frontend/theme work, paused latency
changes and its Git hunks are excluded from this feature's source and commit.


## Instance-local theme switcher

This subsection records the initial UI-only pass. Its native-pixel-preservation
assertion is historical, superseded by the
[pane propagation correction](#theme-propagation-into-terminal-panes) below.

`Ctrl-b t` opens one searchable theme card, without a preview pane. Keyboard
selection and pointer motion preview the palette across Bkpr; Enter/left click
commit, while Escape/outside press restore the prior choice. Query, selection
and preview survive resize and fancy/normal toggles. An empty result cannot
commit. A new client starts with inferred terminal colors, including when it
reattaches to the same backend with the same HOME/XDG; selection creates no
configuration file.

Framework `SearchList` owns bounded grapheme-safe query editing, cached fuzzy
ranking, navigation, scrolling, content headings and hit geometry, reusing
`TextInput`, `UiSurface` and existing `nucleo-matcher` 0.3. Client `themes` owns
the ten choices and UI-only palette projection; `appearance` still derives
muted, blended and semantic colors. Source pins and applicable palette notices
are recorded in the implementation plan and data module. Actual host evidence
remains separate from overrides and still supplies native pane colors.

Real Kitty mode-2031 notifications trigger fresh OSC queries without polling.
An explicit preset survives host palette changes; inferred mode and a return to
it use the latest observed colors. The picker reports unconfirmed RGB support
instead of pretending to apply unsupported colors.

Final evidence: `target/smoke/theme-switcher/acceptance.json`. The immutable debug
pair combines backend revision `b41d1873921f1eb116df2eec2f5b0654c54b80cb` with
the theme feature:

* `bkpr`: `22e050878ff465951530d6f172e3dbe6fc9e3019e6fc0ce6aed3304326be2991`
* `bkprd`: `337fbecab885f43f33a71776689d108bf1f87be6f61913f127ac9b8512bd12fa`

Dark/light Kitty 0.49.2 rich and rounded-cell modes each pass 62 complete-frame
palette/state checks; an actual xterm legacy-input run passes 28. Private Xvfb
surfaces use 120×36 cells, default fonts, two visible native grids and four owned
pane processes. All nine presets have physical foreground/background and
derived matte checks. Representative 11×4-cell native-body crops remain
byte-identical across palette changes; exact PTY input stays unchanged. This is
not a whole-frame native pixel-identity claim. Both detach cycles restore owned
host palette/TTY state, and every case reports zero surviving owned processes.

The initial rich run caught a missing heading: graphical mode intentionally
omits the text perimeter, so placing instructions in its caption hid them.
The generic content heading fixes both modes; a geometry regression covers
mouse selection and a one-row query viewport. A repeated-hover controller also
needed real one-cell motion after reopening at the same pointer coordinate;
the original failed evidence is retained.

Protected formatting, all-target strict Clippy, workspace test compilation,
debug build and doctests pass in 52.92 seconds:
`verification/compiled-1454897806215614/manifest.json`. The actual UID1000 run
in `verification/checks-1455177749151246/result.json` passes 874 top-level tests
plus one isolated subprocess test, with 14 ignored and zero surviving processes.
All 182 current host source guards and all four entry debug/release executable
hashes are preserved. No old-backend mounts, cache clean or release build is used.
Only the four paused latency files and one app call use their prior baseline;
two byte-identical crate-root shadows force Cargo freshness without changing
host mtimes. Paused latency work is neither reverted nor counted as verified.

These are debug/software-rendering functional checks, not latency guarantees.
Live color notifications are physically exercised on Kitty; RGB-unavailable
palette projection has deterministic boundary tests, not an ANSI-only physical
acceptance claim. Controllers and effective frontend source are retained in
`target/smoke/theme-switcher/controllers.zip`.

## Theme propagation into terminal panes

This records the pane-propagation pass. Its modal-faint preview treatment is
superseded by the [undimmed chooser correction](#papercolor-palettes-and-undimmed-previews)
below; the palette precedence and canonical-state isolation contracts remain.

The initial theme switcher excluded pane surfaces, so their native text colors
could not follow a selected palette. The corrected client attaches one borrowed
framework `PaletteOverride` to each visible pane/history surface. Default
foreground/background and ANSI16 resolve through it before annotations, inverse,
faint and damage comparison. Explicit RGB and indices beyond the borrowed slice
retain canonical colors. No scene rows, terminal contents, images or backend
palette state are cloned or mutated; clients can present one canonical frame
with different palettes independently.

UI roles already used the selected foreground and invalidated retained text
styles on changed palette evidence; no separate UI foreground workaround was
added. Overridden pane defaults use explicit RGB encoding even when host default
colors are unknown. Unoverridden surfaces preserve existing native-default and
observed-match ANSI encoding.

`target/smoke/theme-panes/acceptance.json` records the corrected debug pair:

* `bkpr`: `7e1c2f550632d57f52672ec61549303ecdd5b8e556a836eaca360064fd1ad9b7`
* `bkprd`: `337fbecab885f43f33a71776689d108bf1f87be6f61913f127ac9b8512bd12fa`

The private raw native program paints default colors, all 16 ANSI foregrounds
and backgrounds, literal RGB and extended indexed references. Solid block text
glyphs provide exact foreground pixels, while adjacent spaces provide background
pixels. Expected complete Unicode grid content fences every capture. During
modal preview the existing faint treatment remains active, so encoded foreground
and physical background are checked; committed/no-modal captures additionally
check physical foreground pixels rather than mistaking dimmed ink for palette
failure.

Dark and light Kitty rich/plain runs each pass 82 palette/state checks, including
39 undimmed foreground-pixel observations. The xterm normal/legacy-input run
passes 38 checks, including 18 undimmed foreground observations. Every named
palette is committed and inspected; cancellation restores original pane colors.
The existing resize, rendering-mode toggle, empty-result, hover/click,
outside-dismissal, mode-2031 refresh, fresh-instance reset, exact native input,
config-tree and detach-restoration checks remain exercised. All three cases
report zero surviving owned processes.

Protected formatting, strict all-target Clippy, workspace test compilation,
debug build and doctests pass in 68.25 seconds:
`target/smoke/theme-panes/verification/compiled-1396432300441/manifest.json`.
The actual UID1000 run in `verification/checks-1702452364460/result.json` passes
876 top-level tests plus one isolated subprocess test, with 14 ignored and zero
survivors. Added scene regressions cover two differently themed and one canonical
projection of the same frame, restoration, annotations, clipping, hidden focused
cursor geometry and default/ANSI/RGB/indexed precedence.

All 182 guarded host source inputs and the public entry debug/release binary
hashes are preserved. The same five paused-latency exclusions remain intact.
These are functional debug/software-rendering checks, not a latency guarantee.
They verify per-client color presentation; application RGB, image contents and
backend application color-query replies are not rewritten. Controllers,
native C source and effective frontend sources are retained in
`target/smoke/theme-panes/controllers.zip`.

## PaperColor palettes and undimmed previews

The catalog now includes PaperColor Light and PaperColor Dark from
[the original upstream `0cfe64f`](https://github.com/NLKNguyen/papercolor-theme/blob/0cfe64ffb24c21a6101b5f994ca342a74c977aef/colors/PaperColor.vim),
with the pinned MIT notice and source mapping in the catalog and implementation
plan. They use the existing UI/pane preview, commit, rollback and instance-local
lifetime; no dependency or runtime fetch is added.

Theme chooser presentation no longer sets background faint. Forms/finder keep
the existing policy, and clipboard consent retains precedence when it is the
displayed modal. The private actual-terminal fixture opens an ordinary rename
dialog and observes its foreground dimming and restoration without a background
color change. Every chooser preview now checks undimmed foreground pixels as
well as background pixels, including both fuzzy `paper light`/`paper dark`
searches. All twelve choices fit the unfiltered card; both PaperColor variants'
default foreground/background and complete ANSI16 palettes are inspected.

Final evidence is `target/smoke/papercolor/acceptance.json`. Actual Kitty rich
and normal rendering plus xterm normal/legacy input pass, preserving exact pane
input, application RGB/extended-index references, commit/cancel semantics,
mode-2031 refresh, fresh-instance reset, config-tree and detach restoration.
All owned-process survivor lists are empty. Screenshots include
`fancy-paper-light-fuzzy-undimmed.png` and
`ordinary-rename-overlay-still-dims.png` under the recorded Kitty case.

The protected debug build, formatting, strict all-target Clippy, workspace test
compilation and doctests pass in 32.91 seconds:
`verification/compiled-5688278584631/manifest.json`. Host UID1000 workspace
execution passes 876 top-level tests plus one isolated subprocess test, with
14 ignored, in `verification/checks-5792702273414/result.json`.

Runtime pair SHA256:

* `bkpr`: `d93aa472c64d4619bc9e0b672aa41e13a32c810cabcb0b597b7994c842049570`
* `bkprd`: `337fbecab885f43f33a71776689d108bf1f87be6f61913f127ac9b8512bd12fa`

All 182 source guards and public entry debug/release executable hashes are
preserved; paused latency work remains untouched and excluded. These are
private Xvfb/software-rendered functional checks, not latency guarantees.
Controllers and effective frontend source are archived in
`target/smoke/papercolor/controllers.zip`.

## Typing and manual-bottom history exit

Ordinary application keys, literal Ctrl-b forwarding and plain/enhanced paste
now return only the target view to live output and deliver input once. History
navigation and Esc/Enter/End remain local commands. Read-only grants, accepted
release origins and clipboard consent/offer ordering remain unchanged.
Published offset-zero scenes retire the local history flag; downward navigation
at the visible bottom also retires pending entry, including coalesced up/down.

Core retains view identity through its internal key/raw/paste mutations and
resumes the target within the existing pane worker after native writer admission.
Removing that view's historical scene override selects the existing immutable
canonical live frame; target annotations and revision/wake update without a
terminal recapture, another request or application output. Already-live input
skips projection. Post-admission projection/effect errors cannot turn an accepted
write ID into a false rejection. No protocol or engine dependency changes.

`target/smoke/scrollback-input/acceptance.json` records the immutable debug pair:

* `bkpr`: `ba1cb2bb5925579a588845ccd58cd9f1cd16ea08b6c37abf3aa95d29249e614f`
* `bkprd`: `fccea1abb681b3d11ec700c1694f8d6871f232429d76e4edebc0f17f72e67538`

Actual terminal cases:

* `kitty-dark-default-8609972197018/` and
  `kitty-light-default-8610993951963/`: rich and rounded normal rendering.
* `xterm-dark-default-8755389583423/`: legacy input and normal rendering,
  including a native foreground xclip owner on the private Xvfb PRIMARY selection.

Across these cases, 98 restored native bodies match every expected cell and
every pixel of their immutable live references, including the steady live cursor.
Kitty cursor queries also confirm hidden history/restored live visibility.
The raw applications log input but emit no output after typing: `x`, bracketed
paste, literal control-prefix forwarding and typing bursts still restore live
presentation and normal hints. Twelve immediate enter-history/type sequences
per rendering mode preserve each `q` exactly once. Manual wheel-bottom return,
oldest/page navigation, explicit live return, independent other-pane history and
180×48 resize/typing pass. Core tests separately exercise two historical views
of the same pane, exact raw/plain/enhanced paste and encoded release/denial edges.

Reference readiness excludes transient presentation: an intermediate normal-mode
reference accidentally contained the “Normal chrome” toast, producing a real
top-row pixel mismatch after its expiry. The final controller waits for toast
expiry and clears the host resize OSD before taking complete references; it does
not mask those pixels or change production rendering.

Final protected checks are
`verification/compiled-8610305743201/manifest.json`: formatting, strict all-target
Clippy, workspace test compilation, debug build and doctests pass. Actual UID1000
execution in `verification/checks-8694673319213/result.json` passes 881 top-level
tests plus one isolated child, with 14 ignored. All runtime/test survivor lists
are empty. All 182 guarded source inputs and entry debug/release binary hashes
remain unchanged by verification; the five paused latency exclusions remain intact.
These are debug/software-rendered functional checks, not latency guarantees or
full-screen editor benchmarks. Controllers and effective source snapshots are
retained in `target/smoke/scrollback-input/controllers.zip`.

## Independent local and remote TUI sessions

`target/smoke/multi-session/acceptance.json` records the complete working tree,
including the user-accepted graphics latency work. There are no source overrides
or latency exclusions. The 196 guarded workspace inputs include
`chrome_graphics.rs`, framework `graphics.rs`, `joined.rs`, `joined_contour.rs`
and the application's retained-decoration-source call.

The immutable incremental-debug pair is
`verification/compiled-30537826252942/manifest.json`:

* `bkpr`: `1a004c205bc5f44af73de248ff9a3308c60642d0082db55603d371227b36b6c3`
* `bkprd`: `fccea1abb681b3d11ec700c1694f8d6871f232429d76e4edebc0f17f72e67538`

Native family 6, remote control 1, durable schema 4 and engine `93967b7` are
unchanged. Actual terminals are Kitty 0.49.2 and XTerm 411 on private Xvfb;
the SSH transport is OpenSSH 10.5p1. Primary workloads use 120×36 physical cells
and complete 80×34 native bodies. Cargo work is bounded to eight jobs; native
permission-sensitive test harnesses run as the actual host UID1000.

| Case under `target/smoke/multi-session/` | Exercised result |
| --- | --- |
| `kitty-dark-default-31010262582328/` | 51 complete-native captures; mixed local/SSH owners, fancy/plain, three physical client windows, reconnect and full session-end cascade pass. |
| `kitty-light-default-31010251902966/` | Same complete workload and 51 captures under the light host palette. |
| `xterm-dark-default-30379966784756/` | 12 complete-native captures; legacy keyboard navigation, exact Unicode/newline bracketed paste, Ctrl-Space and rounded normal fallback pass. |
| `kitty-dark-default-30379952395332/` | Headless control needs no TTY/display; EOF detaches only the client, takeover retains its reason, and unexpected transport loss exits unsuccessfully without terminating the remote program. |
| `kitty-dark-default-31046267510467/`, `kitty-dark-default-31094402626854/` | Missing-helper errors remain visibly offline with no initial auto-retry; explicit R works in single/mixed mode and both local owners stay usable. |
| `kitty-dark-default-31046257232703/` | Normal last-process exit and a process exiting during bootstrap both close the sole TUI with exact TTY restoration. |
| `kitty-dark-default-31397849461987/` | Explicit fixture restart of the same logical owner reconnects to a fresh daemon; changed logical identity stays rejected/offline. Old input/images are not adopted and the instance palette survives. |

The mixed cases browse and rename a foreign owner's workspace through the one
tree without exposing hidden native views during passive navigation. Aliases
deduplicate to one session parent. Actual takeover removes only the retired
attachment; normal last-process exit removes its subtree while other local and
remote sessions keep accepting exact input. Closing the final workspace through
the real confirmation overlay exits after terminal restoration. Reconnect does
not repeat bootstrap or one-shot takeover.

Shared physical state is exercised through global theme/chrome changes and a
toast that survives owner switching and can be dismissed. Three backends use the
same native image ID777 with distinct actual pixels; resource namespaces remain
independent. Held enhanced-key release stays with its original owner. Clipboard
consent preserves exact bytes and never injects device replies into a PTY.
Stopped pre-Welcome SSH/proxy groups are cancelled before fixture cleanup,
local input continues during remote loss, and an accepted input with a hidden
reply is never replayed after force reconnect.

The spinner regression was reproduced with one already-ended SSH attempt and
idle connection workers while the UI still said connecting. Failed transport
cleanup had cancelled a clone of the coordinator's attempt token, causing its
current failure completion to be discarded. The supervisor now owns a child token;
initial errors remain inspectable and only previously active transport losses
auto-reconnect. `baseline.json` retains the failing-before source/binary provenance.
`retirement-before.txt` separately records the failing takeover-before-activation
regression; typed native retirement now reaches removal instead of the retry path.

Reference readiness includes the physical recessed aperture, not just complete
native text. An early remote-only reference preceded cold graphics readiness;
the uncensored later difference changed 30,453 pixels, with a maximum channel
delta of 60. The final controller waits for visible physical corners and both
aperture sides before recording immutable references, then compares whole native
bodies without masks. A Mocha corner whose shadow rounds to the background is
not treated as a readiness signal. XTerm's documented
[mode 2006](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Readline-Modes)
is explicitly enabled/reset in the private fixture so original pasted LF bytes
remain literal. Production paste behavior and rendering were not changed to make
these checks pass.

Formatting, all-target strict Clippy, workspace test compilation, debug builds
and doctests pass. Actual host execution in
`verification/checks-31010239622406/result.json` passes 937 top-level tests plus
one isolated child, with 14 ignored. All guarded inputs and entry public
debug/release executable hashes are preserved. Every selected runtime/test
cleanup reports zero owned survivors; private credentials and fixture roots
are removed.

`controllers.zip` retains all 196 source inputs, exact successful controller
variants verified against recorded hashes, transitive fixture dependencies and
verification metadata; CRC verification passes. `fixture-decisions.json` keeps
the observed readiness, X11 stacking, revision-guard and fixture-binary corrections.
These are functional debug/software-rendered checks, not WAN/second-host,
hardware-GPU, full-screen editor or new comparative latency guarantees.

## Session tree presentation and live native titles

Evidence: `target/smoke/session-spacing/acceptance.json`, with immutable debug
client `af6dae92` and daemon `ec672b1b`. Private loopback SSH, isolated HOME/XDG
and Xvfb exercise three sessions, a private Git repository and two native windows.
No model/provider, user session or installed executable is used.

`kitty-dark-default-43874236839502` and
`kitty-light-default-43874236856733` pass actual tree navigation, window changes,
exact native input, two observed Nerd Font connecting frames, normal-rendering
toggle, session removal, single-owner elision and TTY restoration. Fourteen
complete native-grid captures retain whole unmasked native-body comparisons
when the same pane/geometry/rendering state recurs. Physical samples distinguish
active/inactive guides and verify that bright ancestor lines stop at each
junction. Both focused item rows share the full-width background. The exact
U+E0CC honeycomb reserves two cells, and window labels remain neutral.
Session separators are nonselectable; arrows skip them. Healthy remote hosts
are right-aligned and muted without `[up]` or a literal `remote` prefix.

`xterm-dark-default-44376515957783` passes the same hierarchy/input/lifecycle
path with normal rounded chrome and actual ANSI inverse selection. Hovering its
metadata card preserves the focused item's background, measured as
RGB `[216,222,233]` before and after, including its metadata row. Available
font coverage differs: the private default XTerm font does not render all Nerd
Font icons. Names and tree geometry remain available; two-cell reservation is
not a claim that every host expands the glyph visually.

The ANSI native-body oracle found 17 changed cell-edge pixels after reconnect,
with maximum channel delta 7, in a complete 720×646 body. Original references,
full differences and coordinates remain unmasked. All native cells match.
This resembles the earlier default-font run-boundary observation, but its cause
is not proven; the ANSI run does not claim exact pixel preservation.
`kitty-dark-default-44088998258961` independently proves a visible network-off
error, no implicit initial retry and exactly one new admission on manual retry.
It is remote-only, not a mixed-owner failure-isolation claim.

Native title proof is
`kitty-dark-default-43244104092050/acceptance.json`: the same final daemon and
prior client `4493171d` (before selected-row trailing-space padding). Actual OSC
0/2 Unicode titles update visible and hidden pane metadata, and reattach retains
the same process/birth. Empty titles return to the base label. Actual frontend
rename wins over later parsed OSC; restart restores the custom stopped label,
not transient title contents or arbitrary old argv. Ten complete native-grid
captures, seven whole-native-body comparisons, exact input and both TTY restores
pass. A `codex` title alone creates no harness facts. The initial GUI failure is
retained: Inspect/context changed while the sidebar did not. Registry `Wait`
now uses the same effective title projection instead of the raw saved record.

Formatting, all-target strict Clippy, workspace test compilation, debug builds
and doctests pass. Actual host-UID execution in
`verification/checks-44345359154646/result.json` passes 948 top-level tests,
with 14 ignored. The recovery-control fixture first failed with unavailable
live context while its foreground command repeatedly forked `sleep`. Its single
test now uses a stable shell builtin `read`; no production authority
check, retry or other exit-gate fixture is weakened. The historical error alone
does not identify which descendant syscall failed.

Final protected compilation is `verification/compiled-44291313315953`;
`../ssh-forwarding/protected-44291006724544/result.json` preserves all 197
guarded inputs and entry debug/release output hashes. An earlier helper's
preservation assertion failed; its symlinked cache children have been replaced
with private bind mounts and direct/canonical-path self-checks. The original
unexpected writer is not established; this is not a whole-task public-output
preservation claim. Negative startup, fixture and oracle runs remain alongside
successful evidence rather than being rewritten as acceptance.

Selected runtime/test cleanup reports zero owned survivors and removes private
credentials/root files. `controllers.zip` retains final reproduction controllers,
transitive helpers, verification metadata and the 197 source inputs. These are
functional software-rendered checks, not WAN, hardware-GPU, full-screen-editor
or comparative latency acceptance.

## Lost keyboard releases and view retirement

Evidence: `target/smoke/lost-key-release/fix/acceptance.json` and
`runtime-acceptance.json`, based on native main `920625cd`. The immutable debug
client is `eca51989`; daemon `643d464e` is unchanged from the diagnostic pair.
The original [reported reproducer](https://gist.github.com/clemux/94aab0f358b602734d0b337196e5d6f6)
and before/control receipts remain in `target/smoke/lost-key-release/`.
Those supplied observations were reused, not rerun to confirm the reported bug:
lost Super left the original pane retiring and rejected its probe; delivered
key-up kept input working.

The client now retains original pane/view and release identity until local
release admission succeeds. Physical `focus;out` settles every attachment's
held keys, including background views. Ordinary pane/session selection still
keeps hardware key-up addressed to its original owner. Key releases use the
existing command FIFO as cleanup; they do not inject a printable press or paste.
Accepted enhanced-paste work and its suffix remain ahead of cleanup, full
admission retains the held entry, and input completion wakes background owners.
Retired generations keep routing tombstones; late hardware key-up does not
become a second release or input for another attachment.

This is Bkpr client lifecycle policy, not a second framework input decoder,
generic keyboard subsystem or backend permission change. The native SDK,
wire family, renderer and daemon are unchanged.

The candidate passes the supplied lost-Super scenario, original delivered-up
control and a lost ordinary-key variant. Each restored pane accepts the probe;
a later real release preserves readiness. Actual private Kitty/Xvfb scenarios
cover Super, ordinary `a`, and hiding the original view before focus loss.
A native fixture requests CSI-u flags 31. Transparent util-linux `script`
captures the real host's keyboard capability reply, XTest key-down, X11 focus
out/in and delayed real key-up. Its original pane receives exactly one release
before hardware key-up, and the peer pane receives no stale release.
`final-physical-{lost-super,lost-ordinary,hidden-super}/restored.png` captures the
actual restored UI after transient toasts expire.

Unit regressions cover normal in-host session switching, physical focus loss
across inactive sessions, original-view retirement, full queue admission,
accepted paste/suffix ordering, cleanup failure isolation, removed/replaced
generations and late key events. The existing stopped-command-lane test now
also proves owed key-up can drain without replaying a failed press.
The static coordinator fixtures cancel their disconnected capture receiver
instead of accidentally treating it as a live registry stream.

Final formatting, all-target strict Clippy, workspace test compilation, debug
build and doctests pass in `fix/build/compiled-4589097018587/manifest.json`.
Actual host-UID tests in `fix/build/checks-4829724532130/result.json` pass
959 top-level tests, with 14 ignored; all 344 client tests pass.
An earlier run alongside GUI fixtures hit the unchanged backend bind-storage
test's global revision assertion (5 instead of 4). That failure remains in
`fix/build/checks-4665510797504`; the quiet full suite passes, but its cause
was not established and no backend test was weakened.

Public debug/release executable bytes and inodes remain unchanged under the
private output guard (`fix/build/protected-4588811526526/result.json`).
The guard first detaches cache hardlinks without replacing live pathnames.
All 20 runtime cleanup receipts and the final host test receipt report zero
owned survivors. Controllers, exact source snapshots, binary hashes and check
metadata are retained in `fix/controllers.zip`; loose throwaway scripts are
removed after archive verification.

This proves private Kitty/XTest focus loss with software rendering, not a real
desktop compositor shortcut or a new responsiveness benchmark. Multi-session
generation/backpressure cases are behavioral unit coverage, not remote-WAN
physical-terminal acceptance. Live user sessions and installed programs were
not restarted or changed.

## Declarative grouped keybinding hints

Evidence: `target/smoke/grouped-hints/acceptance.json`, source base `be13a74a`,
immutable debug client `10cf44b2` and unchanged daemon `643d464e`.
Client presentation groups retain all concrete actions and available aliases;
the framework formats structured chords without Bkpr actions, modifier-bit
interpretation or label heuristics. Notice rows are reserved before whole-group
layout. The source/dependency decision is in the implementation ledger.

Actual private Kitty rich/plain and read-only cases pass in
`kitty-95454571623462` and `kitty-95454570680106`; natural XTerm fallback
passes in `xterm-95454570611799`. At 160×54, focus and movement each occupy
one row: `←, →, ↑, ↓` and `Shift-(←, →, ↑, ↓)`. Resize by one and five
cells remain separate rows. Sidebar selection shows `↑/k, ↓/j`, distinguishing
aliases from related actions. History retains both ordinary and shifted page
aliases. Read-only mode hides unavailable mutation and clipboard actions.

The sidebar is physically dragged narrow; a 34×30 host repeats shared Shift
scope on each wrapped movement row, preserves the complete `n/%` alias and
reports omitted groups as `… 3 more`. Final screenshots follow transient-toast
expiry; whole native surfaces, raw device/input bytes and registry snapshots
remain available without masking.

Representative dispatch stays unchanged: directional focus leaves window
revision 3 unchanged, pane exchange advances 3→4, and resize advances 4→5→6
with actual native width deltas +1 and +5. Exact application markers, no
shortcut leakage, original pane/process/view identities, clean detach exit 0
and identical termios pass. Real Kitty graphics ACK and Tc/RGB responses are
captured through transparent util-linux `script`; XTerm retains normal text
fallback without fabricated capability responses.

Formatting, all-target strict Clippy, workspace test compilation, debug builds
and doctests pass in `build/compiled-95170141555358/manifest.json`.
Actual host-UID execution in `build/checks-95244570491403/result.json` passes
977 top-level tests, with 14 ignored, including 167 framework and 351 client
tests. Generic tests cover non-directional and partial groups, mixed/shared
modifiers, aliases, Unicode widths/spans and narrow boundaries. Client tests
cover filtered membership, unchanged concrete dispatch, produced-text chords,
declared order/priority and notice-only/whole-group row budgets.

Guard `build/protected-95169850759153/result.json` preserves live public
debug/release executable bytes and inodes. Selected runtime/test cleanup
reports zero owned survivors. `controllers.zip` retains the exact physical
controllers; `verification.zip` retains source snapshots, build/check recipes
and verification metadata. Loose owned controllers are removed after archive
verification. Earlier fixture-only readiness, Escape framing and transient
toast corrections remain recorded in the aggregate evidence.

These are isolated Xvfb/software-rendered functional checks, not every
directional combination, a pixel-equivalence oracle or a new performance
benchmark. Installed programs and live user sessions remain untouched.

## Grouped-hint punctuation and sidebar refinement

Evidence: `target/smoke/hint-refinements/acceptance.json`, source base
`df89ad9`, immutable debug client `f0c39524` and unchanged daemon `643d464e`.
Bkpr styles non-keycap ranges as muted, preserving literal punctuation keys as
accented keys. Folding and sidebar exit use declared action groups; descriptions
are shorter, and `Ctrl-b x` reads “remove from layout.” The redundant
`Ctrl-b w/W/o` shortcuts, actions and dedicated forms are removed. Reopening
an unavailable pane retains its independently reachable directory-choice form
and original navigation/confirmation semantics.

Actual private Kitty rich/plain passes in `kitty-97130679640118`; read-only
passes in `kitty-97138705412739`; natural XTerm fallback passes in
`xterm-97209252230525`. Complete screenshots show `←, →, Space` on one
folding row and `Esc, Ctrl-Space` on one return-to-app row. Physical folding,
both sidebar exits, history, resize, narrow-sidebar/host wrapping and ordinary
application input pass. Removed chooser keys do not open a form, mutate the
registry or leak input to an application.

Kitty's actual ANSI screen exports retain separate foreground/faint styles for
parentheses, commas and alias slashes versus both accented keys and ordinary
descriptions, in rich and plain modes. This is a real terminal style observation,
not a framebuffer-color equivalence oracle. XTerm fallback is screenshot-checked;
unit coverage establishes exact `Role::Muted` assignment and keeps a literal
slash key accented. Group tests retain every concrete fold/exit action and its
dispatch while checking partial availability.

Formatting, all-target strict Clippy, test compilation, debug builds and
doctests pass in `build/compiled-97066563662557/manifest.json`. Host-UID
execution in `build/checks-97114850588503/result.json` passes 977 top-level
tests with 14 ignored, including 167 framework and 351 client tests. Two
remaining retired chooser test callers were migrated to project details after
the initial compiler diagnostics; their failure evidence is retained.

All selected terminal runs detach with exit 0, identical before/after termios
and zero surviving owned processes. Guard
`build/protected-97066274343010/result.json` preserves the public debug/release
executable bytes and inodes. The initial XTerm attempt used an obsolete history
label as its Escape-readiness condition; its failed exact input and cleanup
remain recorded. Waiting for actual history-guide withdrawal fixes the fixture,
without changing product decoding or suppressing the failure.

Both exact executed controller revisions are archived as `controllers.zip` and
`controllers-before-readiness-fix.zip`; `verification.zip` retains the frozen
source, check recipes and receipts. Owned loose scaffolds are removed after
archive verification. These are isolated software-rendered correctness checks,
not a responsiveness benchmark, full theme matrix or pixel-equivalence claim.
No installed programs, live sessions or user configuration are changed.

