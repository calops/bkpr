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

The [private Unix transport](../../crates/bkpr-core/src/transport.rs) uses protocol-v2 typed registry operations and pane-scoped requests. It requires private current-user-owned directories, creates a mode-0600 socket, and does not replace a live listener. Request/response and admission limits are bounded, with control capacity reserved independently of long polls. These remain same-user local boundaries, not authenticated remote enrollment or production network framing.

Clients receive compact lossless scene runs and whole-row deltas on snapshot workers. Image receipts include ID and generation, avoiding stale pixels or needless retransmission. Workspace/pane metadata and revisioned layouts persist; terminal contents and processes do not. Restored records without runtimes are explicitly Stopped. A full scene-delta protocol and sustained-animation CPU reduction remain separate work.

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

Use the current [README build and native CLI instructions](../../README.md#native-cli), which build both `bkpr` and `bkprd`. They describe default normal-profile execution and an explicit private fixture environment. Protect canonical live executable outputs when sharing Cargo caches. The historical measurements below identify their own binaries and must not be read as acceptance of later code.

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

The CPU-efficiency gap to Zellij remains explicitly deferred in TODO. The new UI has measurable larger-window presentation and two-process memory costs; there is no claim that the architecture cutover solved sustained-animation efficiency. Future profiling should separate native extraction, encoding, composition and physical drawing rather than silently throttle animation.

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
