# Pane-first mux implementation plan

## Direction and branch scope

`feat/pane-first-mux` is a replacement branch, not a compatibility track. `bkpr` is the user entrypoint and terminal frontend; the separate `bkprd` daemon owns panes and backend state. The seven crates below replace the former combined prototype. The previous launcher, Zellij/WASI plugin, runtime input and release packaging are removed. Old APIs and implementation tests are not compatibility requirements; restoring user-visible TUI and harness capabilities from `main` is the current delivery priority.

`main` and the retained Zellij fork are starting material, not specifications. Inspect relevant current source as a subsystem comes into scope; keep useful code immediately where it forms a coherent module, refactor poor boundaries, and replace designs where better alternatives fit. Do not retain dormant legacy trees merely to preserve them. Record provenance and applicable notices when substantial code is copied. The source ledger below identifies places to look, not mandatory implementations.

The product model remains pane-first: a pane owns a process/PTY and canonical terminal state; layout/session documents reference endpoint-qualified panes independently of their lifetime. Backend workspace membership and agent authority do not follow a viewer's layout selection. TUI, browser and mobile clients may project the same state differently, including one selected pane without sidebar chrome.

The [initial experiment report](engine-proof.md) records historical evidence at `e9ee402`. It is not production acceptance. The native compositor, initial-presentation and startup-input observations still require closure; deleting Zellij is not permission to label them passing. Backend, metadata and agent work need not stop while independent host/rendering investigations proceed.

## Agreed Cargo workspace

The workspace uses these seven packages, without compatibility aliases or empty placeholder crates. The responsibilities below also identify where later agent, metadata, recovery and remote features belong; their presence in this table does not mean those features are implemented.

| Package/crate | Kind | Executable | Responsibility |
| --- | --- | --- | --- |
| `bkpr` | Executable package | `bkpr` | User entrypoint, frontend selection/bootstrap and utilities such as harness launch commands and explicit integration installation. Opens the terminal client by default. |
| `bkpr-client` | Library | — | Bkpr-specific terminal application: workspace/sidebar/popup content, navigation, interactions and daemon communication. |
| `bkpr-tui-framework` | Library | — | Reusable drawing/composition framework: terminal scene surfaces and widgets, layout, decorations, clipping, layers, hit testing, damage, graphics-resource lifetime and coordinated terminal output. |
| `bkpr-server` | Executable package | `bkprd` | Daemon startup, configuration and orchestration of backend services. |
| `bkpr-core` | Library | — | Backend pane/process lifecycle, multiplexing, workspace/layout registry, harnesses, agent communication, Git/CI and project metadata, persistence/recovery and authority. |
| `bkpr-protocol` | Library | — | Shared identities, requests/responses, events, capabilities, errors and terminal scene types. No frontend or backend implementation dependencies. |
| `bkpr-terminal-engine` | Library | — | Backend terminal-emulation adapter: ingest application output, extract canonical state, encode input and expose terminal effects. Adopt compatible upstream Ghostty Rust bindings; do not maintain a parallel generic SDK. |

Separate crates do not imply separate processes. `bkpr` calls the terminal client library in-process; `bkprd` runs separately. Bare `bkpr` connects to or starts the local daemon and opens the TUI. Management commands remain usable without initializing terminal drawing or requiring a TTY. Closing the client does not terminate backend-owned panes. Remote hosts will run their own server after the endpoint transport and trust contracts are implemented.

The client consumes `bkpr-protocol`, not backend implementation internals. The server assembles `bkpr-core`; the core uses the terminal-engine adapter. Keep backend business logic and authoritative associated data out of the frontend. Client-local focus, viewport, device capabilities and consent presentation remain frontend concerns. Explicit integration installation acts on the machine/account where the command runs, not silently on a remote endpoint.

Application UI is described once through `bkpr-tui-framework`. The framework seamlessly selects rich or plain presentation from observed capabilities and user preferences; application components do not maintain separate fancy/non-fancy implementations. Decorative graphics can fall back to characters, but meaningful unsupported content needs an honest indication. Pane scenes, sidebar, popups and decorations share geometry, clipping, layer order, hit testing, cursor policy, damage and one coordinated terminal writer. The framework presents canonical scenes; it is not another terminal emulator.

The initial framework retains the existing terminal writer and uses a thin retained scene/widget compositor rather than introducing a second render loop. Evaluate Ratatui or another maintained widget foundation when its concrete benefit outweighs integration cost; do not create conflicting text/image ownership or leak separate fancy/plain implementations into application components.

Add a GUI client only when implemented; `bkpr` may select it later without changing backend authority. Harness adapters, Git/CI, recovery and storage initially remain modules in the relevant libraries. Extract additional crates only for concrete shared consumers, independent interfaces or dependency boundaries.


## Reuse and design decisions

Prefer maintained libraries for terminal state/input encoding, PTYs, codecs, networking/authentication, widgets and storage. Own Bkpr's agent/harness behavior, lifecycle, control, metadata and permission policies. Copy a small useful algorithm when that is simpler than importing an application; do not add another terminal parser or mux authority beneath the canonical engine.

For each substantial dependency or source port, record:

- The concrete capability and smallest useful public boundary.
- Source/version, maintenance and upgrade ownership, supported platforms and reproducible acquisition.
- License/notice obligations, unsafe/FFI exposure and untrusted-input/resource bounds.
- The measured behavior and failure cases that justify adoption, rather than an unchanged legacy implementation test.

The libghostty-vt C API is unstable. The adapter now consumes the maintained safe/sys SDK from [`libghostty-rs`](https://github.com/Uzaaft/libghostty-rs), pinned at `575ed6b8ceb79ffaf247ecf7e34333876b0d5be4`. Native Ghostty stays at the previously tested `93967b7f9478e94dcbb42125c8a90f34feb40d75`, rather than silently downgrading to the SDK's older native pin. The separately reviewable dependency patch updates the parser-boundary ABI, exposes validated paste-offer correlation safely, supports the exact prepared native installation, and fixes optional image-data handling. Generic native resources, encoders and callbacks belong to the SDK; Bkpr owns normalized scenes, caches, resource bounds and authority policy. [The provenance/API matrix](../../vendor/libghostty-rs/UPSTREAM.toml) records archive hashes, ABI evidence, patch scope and reproducible acquisition. This is a pinned patched dependency, not an unmodified crates.io release or a claim that the patches have landed upstream.

Use `portable-pty` for OS PTY mechanics. The current `termwiz`, Crossterm and codec dependencies are infrastructure choices, not authority over product design. Evaluate a widget library for chrome only when needed. Use system OpenSSH over stdio without a transport PTY for remote endpoints; reuse host verification and selected authentication rather than inventing SSH or changing user credentials. Browser/TLS/authentication and transactional storage dependencies are chosen when their concrete contracts are implemented.

## Starting-material ledger

These revisions were inspected when removing the old runtime. Recheck current relevant `main` when porting; the listed revisions preserve provenance even if its implementation later changes.

### Bkpr source

Repository `calops/bkpr`, inspected main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, under `crates/bkpr/src/`:

| Area | Useful starting material | Redesign boundary |
| --- | --- | --- |
| Process recognition | `agent.rs`, `agent_contract.rs` | Separate process/CWD observations from authenticated pane/launch authority and semantic status. |
| Harness launch/binding | `harness.rs`, `claude.rs`, `codex.rs`, `codex_startup.rs`, `omp.rs`, `agy.rs`, `agy_receiver.rs`, `install.rs`, `installed.rs`; `integration/omp/index.ts` | Keep genuine harness capabilities and exact conversation/session binding; replace native-owner and plugin rendezvous dependencies. |
| Durable agent communication | `relay.rs`, `relay_tests.rs` | Reuse useful durable receipt/delivery logic; redesign ownership, storage or scheduling where warranted. Durable acceptance, wake, read and processed acknowledgement remain distinct product facts. |
| SSH agent forwarding | `agent_relay.rs` | Lease-selected upstream, raw path bytes and generation cancellation are useful; bind policy to the new execution/client model rather than old launcher locks. |
| Git/CI metadata | `project.rs`, `project_cache.rs`, `project_watch.rs`, `ci.rs`, `ci/github.rs`, `ci_contract.rs` | Extract backend services from Zellij pipe/session publication. Preserve authoritative repository/ref/account facts, not worker layout. |
| Recovery/layout reasoning | `recovery.rs`, `recovery_contract.rs` | Claim/liveness and split-tree algorithms may help; replace KDL/sidebar/session-owned process assumptions. New registry formats need not preserve old wire schemas. |
| Typed control/context | `context.rs`, `context_contract.rs`, `control.rs`, `control/{arguments,operations,native}.rs` | Reuse useful intent/schema ideas; design endpoint/pane capabilities and new identities rather than emulate old native operations. |

No disconnected copies of these domains are retained just to make them compile. When a domain is implemented, select useful code and tests against the new requirements; discarded mechanisms do not create regression obligations.

The native capture implementation revisited this same main revision before porting:

- Process recognizers and option-aware runtime entrypoint checks now live in `bkpr-core::harness_process`. A shared sampler replaces per-pane process walks; verified births, PTY ancestry and OS-authenticated connections establish authority independently of recognition.
- The Claude hook, Omp extension, Codex startup classifier/selection proxy and Antigravity root-MCP metadata mechanisms were adapted into `bkpr/src/harness` and `integration/omp/index.ts`. The former broker, native-owner publication, extension receiver/wake and Zellij rendezvous are not retained.
- Launch-local hooks/assets and explicit Antigravity installation preserve native settings and unsupported-mode behavior. Core owns bounded launch grants, conversation generations, foreground lifecycle and status selection; the client only presents typed facts.

The native agent service revisited `relay.rs`, `relay_tests.rs`, Claude/Omp delivery adapters and `agy_receiver.rs` at that main revision. Its private store now belongs to `bkpr-core::registry::agents`, authenticated operations use the native control dispatcher, and launch-local CLI adapters expose shared tool schemas. Native wake, read and processed acknowledgement retain separate provenance; no old broker process, legacy store migration or Zellij context publication is retained. The Codex relay's narrow socket-peer provenance port follows current core birth/ancestry checks rather than trusting a copied token. Generic filesystem events use maintained `notify` 8.2; Bkpr owns hook coordination and outcome policy.

Local project metadata revisits `project.rs` and `project_watch.rs` at main `9ad93bd`. Base precedence, whole-tree merge-base comparisons and NUL-delimited statistics inform `bkpr-core::registry::project`; the old CI/helper/native-session delivery architecture is not retained. Maintained `notify` and `ignore` provide filesystem events and ignore-aware traversal. Canonical-worktree caches and transient pane snapshots replace request-time Git discovery, including repository-scoped agent discovery.

CI/PR restoration uses `pre-native-rewrite` (`e9ee40261bf25b929799d231602573818a0af243`),
specifically `crates/bkpr/src/{ci.rs,ci/github.rs,ci_contract.rs,project.rs}` and
`crates/bkpr-zellij/src/ui.rs`. Useful source includes push/refspec discovery,
provider-side branch-tip queries, verified PR selection, canonical Linear comment
anchors, check normalization, PR-base comparisons and popup artwork/ordering.
The native implementation keeps the same-repository MIT notices; it does not
retain Zellij publication, plugin callbacks or the old Linux-only process driver.

`bkpr-core::registry::ci` owns source generations, cache projection, provider
scheduling and bounded children; `bkpr-protocol::ci` carries supplied facts and
exact refresh targets. `bkpr-client` projects those through the existing sidebar,
popup and common scene framework. No provider I/O runs in frontend code or under
the global registry writer. Identical active canonical-CWD sources share their
discovery/generation/result; different CWD/auth contexts remain separate.

Dependency choices: the already-packaged maintained GitHub CLI preserves the
user's active authentication context rather than adding a Rust HTTP SDK and
credential layer. [`scraper` 0.29](https://docs.rs/scraper/0.29.0/scraper/) (ISC,
default CLI/error features disabled) replaces hand-written HTML parsing.
[`time` 0.3.55](https://docs.rs/time/0.3.55/time/) (MIT/Apache-2.0) parses RFC3339
timestamps and formats static UTC freshness, replacing the copied calendar
algorithm and avoiding a periodic age redraw. Existing `notify` 8.2 watches
default/absolute/CWD-relative `gh` configuration without reading credentials.
Bkpr owns source retirement, admission, batching/backoff and presentation policy,
not HTML/date implementations or a second provider SDK.

Native wire family 7 adds CI facts and the shared `project.ci.refresh` operation;
remote framing, scene encoding and private recovery records keep their versions.
Old native sessions require their matching client and are never automatically
restarted. Runtime proof and tested limits are in
[the CI restoration report](engine-proof.md#native-ci-and-pr-popup-restoration).

Native wire family 8 adds compact agent/control replies, explicit query/body
pagination, quiet delivery, prompt/status observations and typed busy outcomes.
The full native registry remains available to the TUI; agent-facing expansion is
explicit. Remote framing, scene encoding, engine ABI and private recovery schema
remain unchanged. Existing live family-7 sessions need their matching binaries.


### Native fork source

Repository `calops/bkpr-zellij`, inspected main `d7b40a37a63d4934054516f6f445488348075a62`. MIT license, copyright 2020 Zellij contributors; preserve the notice for substantial copies. The sibling checkout contains user work and is read-only for this replacement task.

Rendering, image clipping/damage, cursor/focus, client surfaces, terminal capability/palette handling, input demultiplexing and clipboard transport are explicit reuse candidates. Do not import the fork's `Screen`, tab/plugin ownership or second terminal grid as a shortcut. Capture exact paths, symbols and dependencies when selecting a port; the fork is not a maintained SDK boundary or a specification the new client must reproduce.

History exit revisits Bkpr main `9ad93bd`'s `crates/bkpr-zellij/src/keyguide.rs`
and fork `d7b40a37`'s `zellij-server/src/screen.rs` mode synchronization and
`zellij-client/src/input_handler.rs` paste routing. Keep automatic return to
normal mode at live bottom, not the legacy blocking of typing/paste in Scroll.
The client owns navigation bindings and hints; core uses the existing native
per-view tracked viewport and pane input worker to resume exactly the input
target without another transport round trip. No terminal parser or dependency
is added, and no fork implementation is copied.

| Candidate | Starting source | Fit for the new client |
| --- | --- | --- |
| Host capability/palette model | `zellij-server/src/terminal_colors.rs`, `TerminalColors::update` | Small fixed-size observed-client state. Replace Zellij IPC/event adapters and its `xparse_color`/`AnsiCode` dependency with the new client boundary. RGB color replies don't establish truecolor capability; palette reset shouldn't erase independent capabilities. Current native `ColorProbe` already covers overlapping behavior, so avoid a duplicate module. |
| Damage and wide glyphs | `output/mod.rs`, `OutputBuffer::changed_chunks_in_viewport`, `changed_rects_in_viewport` | Adapt the algorithms to owned Ghostty frames: coalesce damage, retain row backgrounds and complete wide/combining glyphs. Don't copy the old grid or `TerminalCharacter` model. |
| Layer/cursor/pointer visibility | `output/mod.rs`, `FloatingPanesStack`; `output/client_scope.rs`; `panes/floating_panes/client_scope.rs` | Select client-visible ordered layers before text/image occlusion, cursor visibility and hit testing. Equal rectangles don't imply equal layer identities. Replace plugin-configuration ownership with explicit typed client/view state. |
| Host image resources | `output/mod.rs`, `HostKittyState`, `serialize_kitty_frame`; `panes/kitty_graphics/store.rs` | Useful transmitted-resource versus placement lifetime, explicit host-clear invalidation, quota/refcount/cache ideas. Adapt where useful; don't copy grid-side APC parsing or create a second canonical image model. |
| ANSI style transitions | `panes/terminal_character.rs`, `CharacterStyles::update_and_return_diff`; `output/unit/output_tests.rs` | SGR 22 clears both bold and dim: emit the desired pair when resetting either. Preserve the observable property, not the old serializer/cell implementation. |
| Clipboard framing and origin | `zellij-client/src/stdin_ansi_parser.rs`, `ClipboardForwardSlot`, `prepare_clipboard_forward`, reply/paste collectors; `zellij-server/src/screen.rs`, `ClipboardPasteGrant` | Fresh host request IDs prevent late/reused-ID confusion; preserve packet boundaries, exact MIME and original pane/client/selection/offer. Rewrite scheduling and grants against the native endpoint model, not `Screen` maps or plugin events. |
| Host query/input routing | `zellij-server/src/host_query.rs`; `zellij-client/src/lib.rs` query dispatch and `stdin_ansi_parser.rs` reply classification | Useful bounded query/reply isolation and capability evidence. Use maintained parsing/encoding primitives, keep keyboard bytes distinct, and don't inherit old shared-focus/input architecture. |

Fork behavior tests are useful evidence and bug examples, not an immutable suite or a requirement to duplicate old types, IDs, byte order or scheduling. The selected recessed/raised material port is documented below; its source algorithms do not bring in another renderer or terminal grid.

### Selection/copy dependency decision

The pinned `libghostty-rs` SDK supplies reusable selection gestures, native word/line expansion, tracked endpoints, explicit selection formatting and incremental search. A compiled integration probe against the retained native revision verifies those interaction primitives; its evidence is in worktree-local `target/smoke/terminal-reuse-audit/`. Reuse these APIs rather than copying generic terminal selection algorithms from Zellij. Bkpr owns view identity/lifetime, projection spans, input routing and authorized clipboard export.

The closest wrapper, `ratatui-ghostty` 0.2.0 at `65f98749210163158607d1ad2c22e0f35715fbee`, was inspected at its source and public API. It supplies local-terminal rendering/input/session helpers, not selection/copy/search, and depends on SDK `^0.1.1` instead of this branch's `0.2.1`. Its widget needs a native `Terminal` in the UI and performs full-grid conversion; adopting it does not remove this integration work while preserving daemon-owned canonical scenes. No wrapper benchmark or universal performance comparison is claimed. `tui-term` uses a different `vt100` backend; `alacritty_terminal` offers interaction primitives as another engine, not a Ghostty layer.

Main `9ad93bd`'s launcher leaves selection policy to native Zellij. Fork `d7b40a37`'s `zellij-server/src/lib.rs` defaults copy-on-select to true; `tab/mouse_handler.rs::execute_end_selection` retains origin-pane release and copies nonempty selections. These are interaction starting points, not a reason to copy the fork's terminal grid, selection algorithms or shared-client state. Existing `begin_clipboard` framing remains the host export path; terminal permissions remain authoritative.

### Split-resizing dependency decision

[Ratatui's layout module](https://docs.rs/ratatui/latest/ratatui/layout/index.html) supplies general constraints through `kasuari`; [Taffy](https://docs.rs/taffy/latest/taffy/) supplies CSS flex/grid/block layout. Neither supplies pointer capture, exact workspace revisions or stale-gesture ownership. The existing binary ratio tree needs simple partition/inverse arithmetic, not another tree or solver. Keep termwiz input, retained framework drawing, bounded background jobs and `LayoutUpdate`; no new runtime dependency is added for this slice.

Main `9ad93bd` delegates native resizing. Fork `d7b40a37`'s `zellij-server/src/panes/tiled_panes/pane_resizer.rs` solves constraints over native pane grids/stacks; `resize_feedback.rs` derives native border feedback. Those ownership models don't fit independent pane projections, so no source is copied. Native `tiling` now shares real-gutter partitioning between projection, drawing and interaction. The source review and actual-terminal proof live in worktree-local `target/smoke/resizing/`.


### Pane-rearrangement integration decision

Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `crates/bkpr-zellij/src/keyguide.rs::native_hint`, exposes native `MovePane`/`MovePaneBackwards`. The replacement TUI now uses `Ctrl-b Shift+Arrow` to exchange its focused leaf with the same nearest-center directional target used by focus navigation. Split axes, ratios, unrelated leaves, execution membership and focused identity are preserved. The revision-checked `LayoutUpdate` and existing bounded management worker own admission/conflict handling; pending workspace work rejects an additional move instead of buffering stale intent.

This is small Bkpr-specific integration over the existing layout tree, not a new terminal/layout subsystem: no new dependency, generic solver or copied fork implementation is introduced. Read-only mutation is denied; shared layout publication does not overwrite another client's local focus. Actual Kitty proof at 100×36 and 200×60 with styled full-grid redraws, original view/process identities and isolated input logs is retained under `target/smoke/rearrangement/`.

### Terminal-search dependency decision

Use the pinned safe SDK's `Search`, bounded `feed`/`tick`, native match navigation and reusable viewport `MatchBuffer`. Its `snapshot` feeds/reconciles before borrowing results; native active reload preserves completed/history progress rather than restarting the full search. Navigation feeds and selects, not blocking `run`. Match references are consumed before native mutation. Native result memory still scales with all matches per admitted view; no configurable native result cap is exposed.

The current [`ratatui-ghostty` 0.2.0 session API](https://docs.rs/ratatui-ghostty/latest/ratatui_ghostty/session/struct.SessionHandle.html) has no search operation and owns another terminal-processing thread; its [session documentation](https://docs.rs/ratatui-ghostty/latest/ratatui_ghostty/session/index.html) explicitly duplicates the viewport into a mutex-protected Ratatui buffer. It does not remove this integration while keeping daemon-owned scenes. Main `9ad93bd`'s `crates/bkpr-zellij/src/pane_search.rs` searches published metadata, not terminal text. Fork `d7b40a37`'s `panes/search.rs` implements algorithms over its own grid/rows; none are copied. No new dependency or native/SDK patch is added.

### Bkpr frontend ownership and reuse

Decide ownership before implementing a component. `bkpr-tui-framework` owns retained `UiSurface` drawing, `TextInput`/editing primitives with caller-supplied byte limits, window decoration, capability evidence and generic `SvgGraphics` preparation/resource placement. It does not know workspaces, harnesses, Bkpr actions or Bee/Hive artwork. `bkpr-client` owns sidebar selection, forms, finder fields, action dispatch/hints, supplied-metadata presentation, split geometry and artwork-to-marker mapping. The existing framework Scene remains the only clipping, cursor, hit-test, image-lifetime and physical output path; no second renderer or terminal parser is introduced.

The graphics engine is reusable mechanism, not Bkpr's visual design. `bkpr-client` chooses palette recipes/colors, shadow depth, border widths and other stylistic values; the framework accepts those parameters and owns geometry, rasterization, capability handling, clipping, damage and resource/output lifetimes. Generic overlay placement, layering, modal/passive input routing, focus restoration and dismissal likewise belong in the framework. Bkpr owns which overlays exist, their content, bindings and actions. Reuse the current compositor rather than adding a separate overlay or graphics loop.


Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `crates/bkpr-zellij/src/{main,ui,focused,keyguide,pane_search,finder_entry,finder,graphics}.rs`, supplies the product interactions and assets. The native implementation replaces plugin/native-host overlay ownership rather than retaining its transport. Pane/workspace-qualified selection, captured modal targets, revision checks and existing bounded management/input workers preserve current native authority. Metadata popovers and finder reuse the existing foreground-identity guard; output-only snapshot revisions do not invalidate metadata caches, while identity changes do. Project formatting reuses the inspector's freshness policy.

Use maintained `nucleo-matcher` 0.3 for metadata ranking, plus `unicode-segmentation`/`unicode-width` for the bounded single-line model. [`tui-input` 0.15.5](https://docs.rs/tui-input/0.15.5/tui_input/struct.Input.html) exposes codepoint cursor positions; [`tui-textarea` 0.7.0](https://docs.rs/tui-textarea/0.7.0/tui_textarea/struct.TextArea.html) supplies a multiline Ratatui editor/renderer, histories and selection not needed by these fields. The small shared model owns atomic byte admission and grapheme editing without adding a competing widget renderer; application bindings and the 4096-byte policy stay in the client.

Key-guide formatting adapts main `keyguide.rs::{Body,alias_rows,render_expanded}` without its ANSI writer or native modes. Product grouping, priority, width and descriptions stay in `bkpr-client`; framework windows, roles and Scene remain the presentation engine. [`textwrap` 0.16.4](https://docs.rs/textwrap/0.16.4/textwrap/fn.wrap.html) supplies maintained word wrapping instead of another generic implementation. Its [manifest](https://docs.rs/crate/textwrap/0.16.4/source/Cargo.toml) records MIT and MSRV 1.88, supported by this development toolchain; [upstream maintenance](https://github.com/mgeisler/textwrap/commit/07fdbbb92103a22c48a61be271dca36c739fdde3) includes current padding fixes. Only Unicode-width support is enabled: first-fit, no word breaking or hyphenation, no ICU/SMAWK/terminal probing. One measured layout produces rows and exact key spans on guide invalidation, replacing repeated hint collection and text-based span inference.

Grouped hints now declare concrete action membership, order, common description
and priority in client `PRESENTATION_GROUPS`. Filtering precedes grouping;
each row retains every member action and its actual alias chords. Main
`9ad93bd`'s `keyguide.rs::add_hint` grouped by description and priority; the
native version deliberately uses explicit semantic membership instead of text
identity or direction-name heuristics. No additional legacy implementation is copied.
Framework `chord::{Chord,ChordGroup,ChordLine}` owns protocol-neutral modifier
labels, alias/member separators, Unicode cell measurements, keycap spans and
whole-chord wrapping with repeated, explicit modifier scope. Client code owns
context, descriptions, popup width/priority policy and notice composition.
Notices reserve rows before hint layout rather than truncating rendered groups.
Existing `unicode-width`, `window::clean` and `textwrap` cover this small
presentation layer; no new hotkey registry, parser, UI framework or dependency
is introduced. Prepared guide output retains the existing invalidation cache.
Bkpr applies `Role::Muted` to gaps between the framework's exact keycap spans,
not to characters recognized as punctuation. Literal `/` keys therefore remain
accented while alias slashes, commas and scope parentheses are muted. Explicit
groups include folding and sidebar exit without changing concrete dispatch.
The redundant workspace/window/pane chooser shortcuts and forms are removed;
the independently reachable reopen-directory chooser keeps its original semantics.




Use maintained Linebender [`resvg/usvg` 0.49.0](https://github.com/linebender/resvg/blob/v0.49.0/crates/resvg/Cargo.toml), pinned with default features disabled for trusted path-only embedded artwork. MSRV 1.85 fits this edition-2024 workspace; the [release history](https://github.com/linebender/resvg/blob/v0.49.0/CHANGELOG.md) includes current bounding/filter robustness fixes. Resvg/usvg are Apache-2.0 OR MIT; tiny-skia 0.12 is BSD-3-Clause. One coalescing worker owns parsing/raster/tint/LRU work; cell dimensions and observed colors invalidate bounded resources. Explicit matching APC query replies—not TERM or truecolor—enable decorations. Ordinary Frame images/placements retain renderer namespaces, clipping and upload deduplication.

Bee/Hive assets come from main's `assets/` at that revision. Preserve their Pictogrammers/Material Design Apache-2.0 notices in `crates/bkpr-client/assets/`. Fork `d7b40a37a63d4934054516f6f445488348075a62`, `zellij-server/src/output/recessed_frames/{depth,raster,raised}.rs`, now supplies the continuous rounded-mask convolution and supersampling in framework `decoration.rs`; that file preserves exact provenance and the full MIT notice, Copyright (c) 2020 Zellij contributors. The existing worker, cache, Scene resources and output path remain authoritative.

### Overlay lifecycle and static graphics

Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `overlay.rs`, `focused.rs`, `ui.rs` and their callsites supply the workflow inventory: passive metadata/guide layers, finder, creation/rename and confirmations. Most product content already existed; it now uses framework `OverlayStack<F>` for geometry, layer ordering, opaque return-focus tokens and whole-gesture ownership. Main/fork outside-press cancellation is restored; wheel/motion/resize preserve drafts, pending mutations remain noncancellable, and removed/replaced owners retain a release tombstone. This is small lifecycle glue over existing Scene/window/input infrastructure, not a second widget renderer.

Main focused overlays request `dim_background`; the retained fork's `output/mod.rs` and `ui/pane_contents_and_ui.rs` implement output-only SGR faint, preserving bold/colors and images. Framework `Surface::faint` supplies that generic projection; Bkpr selects when it applies. Closing an overlay restores unchanged native styles without modifying canonical frames.

Framework `DecorationStyle` accepts caller-selected material, background, inset, radius, blur, opacity, lighting and exterior support. The existing graphics worker prepares a bounded 3x3 atlas with multi-cell tiles and sparse perimeter fragments; image size does not grow with pane dimensions. Caption exclusions preserve semantic overlay headings; unsupported geometry falls back instead of silently clamping the requested material. Materials retain the 16-entry/8-MiB cache alongside the existing SVG budget. `bkpr-client::appearance` owns all style values. Opaque-cell and transparent-graphics surfaces share one Scene; later opaque overlays clip lower images, and pane-local depths cannot escape scene order. Native sizing and pointer transforms use the same inset in rich/plain modes.

Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `finder.rs::{Layout::raised,Finder::render}`, `finder_entry.rs` and `ui.rs::tooltip`, now supply the finder presentation as well as matching semantics: three disjoint query/results/preview cards, 40/60 wide or stacked geometry, single-row selected entries, field-aware excerpts and grouped metadata. Fork `18890b231bc3dd29039dd6ca141ec0af49cf332e`, `output/recessed_frames.rs::{region_key,raised_margins}`, `regions.rs`, `colors.rs` and active `depth.rs::bevel_normal`, supply half-cell-width physical margins, the six-percent linear-light lift, luminance-preserving accent tint and all-edge facets. The old generic shadow-only policy was not main parity; raised surfaces now carry their fill and bevel, including the surrounding-surface highlight needed on light themes.

Framework `SurfaceContent::CellsClipped(Rect)` borrows one retained frame at its original source origin while clipping cell projection, hits, cursor and image occlusion to complete glyphs. `BackgroundSpan` extends retained semantic theming with caller-chosen plate backgrounds, preserving selection and hover inks/backgrounds. Bkpr chooses the finder regions, neutral/cyan and form-mode palettes, geometry and contents. It projects only opaque text interiors; sparse atlas fragments supply the curves and gutters without a square cell underlay. Eleven bounded material slots cover the existing pane/guide policies and main's neutral/red/green/yellow/blue/cyan region variants; the 16-entry/8-MiB cache, shared resources and stretched straight edges are unchanged.

The metadata preview reuses maintained `textwrap` 0.16 and the existing Unicode/nucleo helpers. Ratatui's maintained [`Paragraph`](https://docs.rs/ratatui/latest/ratatui/widgets/struct.Paragraph.html) was evaluated, but this bounded paragraph/semantic-span projection does not need another widget buffer bridge. No terminal parser or render loop was added. Provider facts remain absent rather than inferred. Backend-owned ephemeral project ordinals now follow main's `native.rs:1923–1962`: canonical common-Git identity, linked-worktree sharing, deterministic lowest-free reservations, uncertainty retention and last-owner release. Client `project_colors.rs` follows main's `palette.rs` finite accent/shade strategy and `ui.rs:1044–1153` project-name/marker placement; branches stay neutral and accents clear inherited faint/bold like main's SGR 22. Palette evidence and registry demand are cached separately from graphics capabilities.

Generic `ForegroundSpan` complements `BackgroundSpan` through `UiSurface::apply_theme_with_colors`, replacing the old background-only method without an alias. The framework resolves caller-supplied visible ink once per span, including inverse cells, while preserving selection/background/glyph invariants. Bkpr owns the palette, semantic roles and choice of colored fields. Shared structured metadata supplies sidebar and finder previews; no per-render process/provider/filesystem work or hidden attachment is introduced.

Adjacent pane frames overlap on one shared separator cell, so their native bodies remain disjoint with exactly one intervening row/column. Resize minima and hit tests use the same partitioning; unrepresentable splits project their first subtree without mutating backend membership/layouts. Material readiness is an input to retained chrome painting, not a destructive post-paint edit: ready pane perimeters contain no text, and fallback restores complete captions. The obsolete separate divider drawing API is removed.

Ready frame glyph removal must also restore their normal-color substrate. Fork `zellij-server/src/output/recessed_frames.rs::decorate_characters` explicitly fills cleared physical frame columns with `key.background`; merely retaining sidebar-colored blank cells leaves dark strips inside translated horizontal apertures. Native `theme_chrome` reuses its `ChromeContext` material-readiness list and applies the existing normal-background panel role to ready frame rectangles, omitting text-frame styling only there. The cached atlas keeps the outside mask opaque matte and the inside shadow translucent. No added image, rasterization, viewport inset or native scene mutation is needed; fallback still uses complete rounded text frames.


The fork's `output/recessed_frames/strips.rs` caches repeated 1024-pixel straight strips and coalesces chunks. This framework already prepares eight sparse fragments, with one stretched placement per straight edge and a dimension-independent atlas shared across panes/resizes. Representable constant-axis viewport/opaque-layer clips now retain the original resource rather than rasterizing derived strips. Exact native sampling phase still requires derived rasters for some arbitrary image scaling. Worker regressions verify actual raster reuse and 16-entry/8-MiB eviction; a physical-writer regression verifies one cold upload and none for unchanged, resized or moving-occlusion versions of that atlas. Consumer-held `Arc` references are outside the LRU byte budget.

Fork `joined.rs`/`contour.rs` joins rectangles of one semantic raised surface; it is not permission to union independent pane apertures and remove their opposing shadows. Framework `JoinedSurface` now restores the sidebar-anchor/card union, including concave corners and complete bevel/shadow support. Both materials use the existing graphics worker, Scene and encoder. Joined preparation retains 16 layouts/4 MiB and 512 patches/8 MiB; the presentation side keeps a separately bounded 16-layout/4-MiB lookup sharing immutable worker `Arc`s, so a warm request does not withdraw its material while waiting for a worker round trip. Consumer-held references are outside worker LRU accounting.

Constant-axis joined textures use three physical guard texels and sample the middle logical texel. This preserves dimension-independent straight-edge sharing while avoiding observed Kitty/llvmpipe foreground leakage from stretched one-texel images. It is not a full-length raster fallback: the logical raster is computed once, then guarded. Native-engine regressions and direct physical texture probes cover both orientations.

Decision: retain inline Kitty graphics uploads for frontend chrome and remove
the resource-backed transport experiment from `c500e72`. Existing image caches
avoid steady-state uploads; no demonstrated benefit warrants shared-memory/file
probes, resource preparation, leases or reply-routing machinery. Keep the image
caches, merged edges and asynchronous SVG rendering unchanged.

The existing renderer retains source-valid inactive native projections by last visible use, bounded to 512 entries and 8 MiB of RGBA charge beyond the current visible requirements. Source disappearance retires data immediately; placement removal alone does not retransmit pixels. Current supported image quotas are unchanged, including visible projections exceeding that inactive-cache budget. The ordered container is maintained [`hashlink` 0.12.2](https://crates.io/api/v1/crates/hashlink/0.12.2) (MIT/Apache-2.0, Rust 1.85, September 2026 release), not a custom linked list: its [`retain`, `to_back` and `pop_front` APIs](https://docs.rs/hashlink/0.12.2/hashlink/linked_hash_map/struct.LinkedHashMap.html) fit source retirement, batched visible promotion and byte/count eviction together. Bkpr owns only admission/lifetime policy. GPU/driver overhead is not the RGBA charge, and newly required arbitrary projections can still rasterize/encode synchronously; this is not a claim that all native-image preparation is off-path.

Actual debug rich/plain, live-theme, draft/gesture, local Git and framed two-client correction evidence is recorded in the [engine report](engine-proof.md#main-aligned-chrome-correction); the earlier functional pass is retained separately, not treated as visual parity. Provider-owned forge/PR/issues/CI snapshots and refresh remain unavailable; no frontend network calls, fake fields or no-op actions substitute for that deferred backend contract. Animation and a command palette remain outside this pass.

The user subsequently authorized a narrow animation extension: visible working
sidebar titles sweep their foreground toward a client-selected palette accent.
Framework `animation::{AnimationClock, TextSweep}` owns epoch-aligned coalesced
deadlines and allocation-free glyph-safe foreground mixing; the client owns
eligibility, style retention, colors, width and cadence. Existing `Instant`,
protocol styles, Unicode cell metadata and the differential scene renderer suffice;
no additional scheduler, graphics engine or dependency is needed for this small
effect. Animation-only ticks reuse presentation without backend observation,
graphics preparation or hit-map rebuilding. There are no animation deadlines
without eligible visible truecolor titles. Native/default ink remains static.

Debug evidence is under `target/smoke/title-sweep/`: protected workspace checks,
host-run tests, and a generic 160x50 Kitty renderer demo with ordinary/selected
Unicode titles plus a static graphics resource. Content-ready captures show
changes confined to the title band, identical idle frames and no graphics
commands after the initial frame. This is physical framework-effect evidence,
not an authenticated harness-status end-to-end screenshot.


### Terminal-native theme

`bkpr-tui-framework::Theme`, `Role` and `RoleSpan` own reusable color resolution and retained `UiSurface::apply_theme` styling. `bkpr-client` assigns roles to Bkpr components and guarded activity; neither framework theme nor palette probing knows workspaces or harness identities. Native frames retain their application styles; optional borrowed `Surface::palette` overrides project their default and indexed colors without applying UI roles or mutating canonical state. Palette equivalence is prepared once per surface, retaining compact native ANSI output for observed matches and resolved RGB for differing virtual pane palettes.

Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `crates/bkpr-zellij/src/palette.rs` and `ui.rs`, supplies the signed luminance surface shift, subdued foreground/selection/accent blends and semantic color assignments. Preserve that recipe without invented contrast-based recoloring. RGB surfaces require independent positive `Tc`/`RGB` evidence and both observed foreground/background. Otherwise native defaults, ANSI indices and selection/hover attributes remain meaningful. The legacy project-slot allocator and plugin transport are not copied.

The inherited palette recipe lives in `bkpr-client::appearance`, which supplies `ThemeStyles`. Framework `Theme` only caches caller-resolved styles against relevant terminal evidence and retains generic style application. Palette recipes, fallback semantic choices and decoration materials are client policy; no Bkpr-specific recipe remains in the graphics engine.

The theme picker uses the framework's generic `SearchList` inside the existing modal owner, with a content heading shared by graphical and rounded-cell rendering. Query editing reuses `TextInput`; fuzzy matching reuses maintained `nucleo-matcher` 0.3, already used by the finder. Ranking runs only on query changes, not hover or repaint. Client appearance policy leaves the chooser's background undimmed so preview colors remain accurate; ordinary modal overlays still use the same generic faint mechanism. No second editor, search engine, overlay loop or theme SDK is introduced.

`bkpr-client::themes` owns named palette data, the cached UI projection and borrowed pane overrides. `Ctrl-b t` previews highlighted choices across chrome and terminal panes, commits with Enter/left click, and restores the prior choice on Escape/outside dismissal. With positive RGB evidence, pane overrides replace default foreground/background and ANSI16 before inverse, selection/search annotations, faint and damage comparison. Explicit application RGB, higher palette indices and image resources remain unchanged. Native seeding still uses actual host observations; selected palettes never become backend facts or recolor another client's view. A client-instance choice has no configuration or backend persistence, and inferred mode returns to current canonical pane colors and the latest host-derived UI colors.

Palette data sources: [Catppuccin/kitty `4309831`](https://github.com/catppuccin/kitty/tree/43098316202b84d6a71f71aaf8360f102f4d3f1a/themes), four `.conf` variants (MIT); [altercation/solarized `62f656a`](https://github.com/altercation/solarized/blob/62f656a02f93c5190a8753159e34b385588d5ff3/xresources/solarized), with the documented light base substitutions (MIT); and [folke/tokyonight.nvim `cdc07ac`](https://github.com/folke/tokyonight.nvim/tree/cdc07ac78467a233fd62c493de29a17e0cf2b2b6/extras/kitty), night/storm/day generated files explicitly marked MIT (the repository's code license is Apache-2.0). Applicable MIT notices are retained in the data module. Small compile-time constants avoid a palette parser/dependency; Bkpr's existing client appearance policy still derives muted, blended and semantic colors.

PaperColor Light/Dark use the original [NLKNguyen/papercolor-theme `0cfe64f`](https://github.com/NLKNguyen/papercolor-theme/blob/0cfe64ffb24c21a6101b5f994ca342a74c977aef/colors/PaperColor.vim): the default light/dark `color00`–`color15` tables, with `Normal` foreground from `color07` and background from `color00`. Preserve the published slot order rather than remapping it to conventional color names. MIT, Copyright (c) 2015–2020 Nikyle Nguyen; the notice is retained with the catalog's existing MIT permission text. Optional Vim syntax/UI colors are not copied.




Fork `d7b40a37a63d4934054516f6f445488348075a62`, `zellij-server/src/terminal_colors.rs` and `zellij-client/src/{lib,stdin_ansi_parser,stdin_handler}.rs`, informs capability independence, duplicate suppression and event-triggered palette refresh. The existing bounded demultiplexer consumes private `CSI ?997;1n`/`?997;2n` reports; one input batch requests one refresh through the existing output owner. Mode 2031 changes require confirmed mutable support and retain the original setting for detach. No polling, second parser, global backend palette or decoration timer is introduced.

Existing termwiz/Crossterm, protocol styles and the Scene renderer supply infrastructure. [`palette` 0.7.6](https://docs.rs/palette/0.7.6/palette/) provides typed color spaces, conversion and mixing, not terminal evidence or retained semantic roles; its [metadata](https://crates.io/api/v1/crates/palette/0.7.6) records MIT OR Apache-2.0 and the 2024-04-28 release. The small inherited blend/luminance recipe remains glue rather than adding a broad color-conversion dependency. Color math runs only on relevant evidence changes; fixed style arrays, retained role/attribute buffers and unchanged-surface fast paths bound repeated work. SVG preparation uses final displayed ink, including selection, and keeps text when that ink is unknown.

### Named-session and client-lifetime boundary

`bkpr` owns session-name resolution, read-only local discovery and serialized daemon startup; `bkprd`/core own the persistent logical session ID and fresh daemon incarnation. One local name selects one daemon and registry. Exact UTF-8 names are SHA-256-addressed, not sanitized into path components. Main `9ad93bd`'s `launcher.rs` supplies the familiar default `bkpr`, `--session/-s`, `--detach-others/-d` and nonempty/control-free naming intent; no legacy native-session assets, plugin reload policy or automatic live-daemon restart is carried over.

Use maintained `clap` for native-argv parsing, `sha2` for stable addressing and `fs2` for the cross-launcher advisory lock; its guard explicitly unlocks before an inherited descriptor can prolong ownership. Core's exclusive root lease and atomic private `session.json` remain the single identity authority. Headless tools never bootstrap. Ordinary TUI/stop targeting defaults to `bkpr`; context/request/keyboard may inherit a pane's socket unless explicitly named.

The version-2 transport adds `SessionInfo`, a dedicated `ClientRegister` lifetime stream and process-bound ephemeral `ClientAuthority`. Managed short requests carry that authority; admin/headless requests never implicitly adopt managed views. Initialization precedes one-shot `ClientActivate`, and only successful activation may revoke other attachments. A repeated activation cannot detach later arrivals. EOF, owner exit or explicit detach retire managed views, queued callbacks and device grants without closing pane processes or rewriting layouts. Client and view lifetime are not bounded by fixed admission counts.

Keep existing synchronous Unix transport and narrow OS credential/exit observers rather than adding another async runtime. Linux retains a kernel peer pidfd; ordinary managed requests validate cheap kernel identity/liveness instead of scanning process/CWD/argv state on each key. Pane exec receives trusted immutable logical session ID/name, not client credentials. Actual scope/TTY/takeover acceptance and platform limits are recorded in the engine report; remote endpoint aggregation and cross-daemon agent discovery remain future work.

Pane records, running panes, workspaces, windows, clients and views have no fixed
count ceiling. Keep the existing durable-record, message, image, input and
traversal bounds. Local transport separately accounts small/large raw and decoded
request bytes, charges aliased Planus expansion before allocation, and bounds
short decode/encode stages without holding permits across blocking dispatch or
I/O. The synchronous socket/thread architecture remains proportional to live
connections and subject to OS limits; this is not a global heap bound.

TUI retirement dependencies are pane-local: another generation of the same pane
waits for its old view, but unrelated panes do not share an attachment ceiling.
Remote requests queue fairly for existing bounded Yamux work capacity instead
of rejecting additional views. Reuse Tokio's maintained FIFO semaphore and
[`futures::executor::block_on`](https://docs.rs/futures/latest/futures/executor/fn.block_on.html)
on existing request workers; no custom executor or second transport is added.
CI authentication history and request work have independent byte allowances,
not a limit derived from pane counts. Valid provider embargoes survive pressure.

Protocol 4 carries optional typed `session_closing` failure metadata with a specific
request disposition: `not_dispatched` or `unknown` after dispatch began. Missing
metadata and error strings do not grant shutdown semantics. Keep decoded requests
in the response drain through startup cancellation and closing admission. Immutable
identity queries remain available after persistence failure so explicit targeted
stop can reach cleanup without weakening target validation.

Client revocation cancels observational streams, not accepted mutation replies.
Preserve typed failures through snapshot/input workers and attach rollback;
only proven undispatched rejection or an observational interruption backed by a
typed closing reply/validated shutdown event may exit cleanly. Real failed or
ambiguous mutations remain errors without replay. Existing `serde`, `anyhow`,
Unix transport and lifetime state cover this boundary; no new runtime or dependency
is needed. Multi-client stop/last-shell/last-document proof is in
[typed session shutdown](engine-proof.md#typed-session-shutdown).

### Native binary transport

Wire family 6 uses [Planus 1.3.0](https://docs.rs/planus/1.3.0/planus/) and checked-in schema bindings; the initial binary cutover was family 5. The synchronous socket/lifetime ownership and authentication model stays unchanged; no second async runtime or transport service is introduced. CLI/MCP/provider interfaces and durable records keep their own JSON boundaries. Extensible metadata uses a recursive typed value union, not embedded JSON text; pane scenes have separate binary storage.

`bkpr-protocol::wire` owns the 24-byte little-endian admission header, typed codecs and bounded framing. Check magic, family, kind, reserved bits, required capabilities and declared size before allocating payload storage. Requests are bounded to 8 MiB and responses to 96 MiB. Bound traversal depth and aggregate owned expansion independently, including repeated offsets; reject unknown tags, inconsistent presence, duplicate metadata keys and invalid UTF-8 before dispatch.

`bytes::Bytes` retains validated scene ranges in the received allocation without copying scene contents. Fully validate cell boundaries, row/layout counts, styles, links, geometry, revisions and image budgets before native frame expansion. Check baseline and image-generation prerequisites before consumer mutation; invalid updates cannot advance poll cursors or replace frame, resource or clipboard state. This is zero-copy scene reception, not an end-to-end zero-allocation claim: canonical mutable cells/resources still need owned storage, and Planus's finished encoder slice currently requires an owned-output copy.

Preserve table ordinals, enum tags and the fixed scene structs within a family. Add only optional fields with conservative semantics; use required capability bits for new mandatory behavior, and bump the family for incompatible representation changes. Presence-bearing unions are required inside nullable wrapper tables, including optional spawn placement. Do not revive a JSON fallback or infer compatibility from matching product version strings. Family-6 attachment to a family-4/5 daemon fails closed without restarting, displacing or terminating it; its matching old client remains usable. Durable schema 4 is unchanged.

The schema compiler is development-only: `planus-cli =1.3.0`. Regenerate `wire/generated.rs` from `wire/native.fbs`, and `scene_wire/scene_generated.rs` from `scene_wire/scene.fbs`, using `planus rust --format false -o OUTPUT SCHEMA`. Generated modules are excluded from hand formatting; normal builds need only the locked runtime dependency. Acceptance and matched whole-mux CPU evidence are recorded in the engine report.

### Atomic process creation and placement

`PaneSpawn` carries named `PaneSpawnEntry` values and a complete
`SpawnLayoutNode`: explicit existing-pane IDs, request-local new references and
ordinary ratio/axis splits. Bound creation requires the entire ownership/CAS
triplet and layout; unowned administrative batches require neither. Resolve all
new IDs and validate ownership, uniqueness, depth, ratios, specs, capacity and
the bounded durable candidate before execution. Every new bound pane appears
exactly once; existing owned panes may remain hidden without losing ownership.

All workspace/window creation, reopening, restoration and batch placement share
the reservation/start/commit engine. Reserve the whole batch, prepare every
HOME/CWD outside global locks before the first exec, and start processes outside
registry/writer locks. The workspace reservation rejects competing layout/close
mutations; final CAS validation still protects publication. Rebuild against
current metadata to retain unrelated workspace commits, then persist once and
publish the full record plus all live handles under one state lock. Install
completion/recovery callbacks before exposing handles and process early exits
after releasing the writer lock. Keep the batch admission gate through callback
wiring so managed capture/context cannot adopt unpublished panes.

Return `created_panes` mappings and the resulting registry; ordinary TUI splits
must not follow spawning with a second layout CAS. Failure cleans up every
unpublished child without consuming older recovery intent. Directory setup and
external command effects are not rollbackable; immediate successful process
exit remains ordinary lifecycle. Failed durability publishes nothing, poisons
further commits and reports error, including ambiguous rename/fsync outcomes.
Never replay unknown creation. Existing `portable-pty`, `tempfile`, Planus and
registry primitives cover this Bkpr transaction; no new dependency, agent-specific
process system or transport is introduced. [Runtime evidence](engine-proof.md#atomic-pane-creation-and-placement)
records subscriber, TUI and failure acceptance.




### Exec snapshots and live pane context

Main `9ad93bd`, `context.rs::{current,current_pane}`, supplies the useful intent:
live canonical labels/membership take precedence over stale inherited workspace
names. Its Zellij-pane fallback, native-owner publication files and advisory
owner registry do not carry over. The native daemon already owns the execution
registry; protocol-4 `Context` reads that authority directly.

Every registry spawn path shares a private `SpawnContext` bundle. Session/pane
and optional execution-workspace snapshots are injected before native exec;
the existing full-prefix environment clearing removes outer context first.
Automatic workspace names are reserved once under the short transaction,
including metadata-only creation. Pending labels participate in allocation,
explicit duplicate labels remain allowed and RAII releases failed reservations.
No locks span PTY/FS startup; snapshots do not become persisted authority or
recurring render/input work.

Default lookup verifies the actual IPC peer's birth, ancestry and controlling
PTY, including immediate root exec before publication. Existing spawning state
and the Condvar provide bounded readiness; final lookup revalidates the live
runtime and reads current labels. Explicit live pane selection is same-user
private-socket metadata, not a new agent/view/device permission. Background
same-PTY jobs work; nested or detached/foreign/stale peers fail closed. Borrowed
projections never supply execution membership.

Existing `serde`, `clap`, process-birth sampling and spawn reservations cover this
Bkpr-specific boundary. No new generic context service, cache or dependency is
needed. The launcher owns headless presentation; the obsolete frontend
whole-registry forwarding helper is removed. Shared MCP `whoami` keeps its
existing authenticated launch context, with no expanded agent-control allowlist.

### SSH-agent forwarding boundary

Main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, `crates/bkpr/src/agent_relay.rs`, was inspected rather than ported. Its final-launcher cleanup can remove the stable socket while detached panes survive. The replacement drops launcher-side relay helpers, route sidecars, recursive relay discovery and whole-frame parsing; the daemon owns one listener and the existing process-bound attachment lease owns selection.

Protocol 4 publishes the original client source once at activation, preserving raw paths and resolving relative sources against the original invocation directory. Latest live activation wins even with no source; only a changed selected identity advances the routing epoch. Reserve publications under the client map lock, apply them outside it and reject delayed epochs. Every forwarded read/write checks the epoch and live owner. No signing request is retried or moved to another upstream.

[`ssh-agent-lib`](https://docs.rs/ssh-agent-lib/latest/ssh_agent_lib/agent/trait.Session.html) is maintained but provides typed protocol decoding rather than an opaque byte bridge. The existing blocking backend does not need a second async runtime. [`mio` 1.2.4](https://docs.rs/mio/1.2.4/mio/) was already locked transitively and now supplies the maintained Unix socket/reactor/waker infrastructure directly. This preserves unknown messages and connection-scoped extensions described by [OpenSSH's agent protocol](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.agent) without a second SSH implementation.

One fair reactor bounds connections to 32 and payload buffers to 2 MiB. Two fixed admission workers isolate filesystem lookup and pathname connect from established streams and shutdown; `O_NONBLOCK` alone does not bound lookup. The five-second deadline covers admission, not established signing/approval. Workers blocked in the kernel may outlive shutdown until lookup returns, hold no downstream or registry lock, and cannot publish after retirement; no replacement workers are created. If both are blocked, new admissions fail closed rather than growing threads.

The socket and its separate advisory lease stay daemon-owned at zero clients. Explicit unlock handles inherited descriptions, stale recovery requires a definite refused connection under that lease, and cleanup is inode-aware and disarmed after retirement. Fixture-isolated execs never receive the socket. Persistent SSH/Git configuration, profile mutation, remote endpoint enrollment and agent selection from input/size/clipboard state are outside this boundary.

### Explicit native recovery boundary

Keep the useful recovery intent from the source ledger, not main's KDL,
native-owner publication, session claims or saved launcher recipes. The native
registry owns a bounded version-3 map of authenticated harness kind, native
conversation ID and byte-preserving working directory. Older layout records
upgrade with no invented bindings; live capture/agent authority is never
restored from storage. Existing `serde`/`serde_json`, `fs2`, atomic private
storage, spawn reservations and `clap` cover this boundary without a new
persistence framework or dependency.

The approved policy is **current native configuration**. Omp's own session
manager may restore recorded model/thinking/service-tier state; its runtime
approval flags are not a Bkpr recovery contract. Codex requires a compatible
current provider/configuration for its exact native thread. Claude and
Antigravity retain their native resume and approval semantics. Save no old argv,
model/provider/profile/approval/sandbox flags; never switch provider, fork or
start a fresh conversation on refusal.

`recovery_list` distinguishes live, eligible and unavailable intent without
native-store/provider preflight. `pane_resume` requires the unchanged selected
descriptor, reserves the native kind/ID across aliases and starts only fixed
native adapters through the configured matching `bkpr run`. Successful exec is
reported as pending native binding. The original descriptor transfers only
after the matching authenticated native ID binds with fresh launch/inbox
authority; failure retains recoverable intent. Existing projection determines
in-place replacement versus a new workspace, without replacing a running shell.

Headless `bkpr resume` never bootstraps or mutates on listing. Shared MCP
`control` allowlists these same two operations, defaults to the authenticated
execution workspace and supports explicit daemon-local session/all scope.
Repository scope and arbitrary commands are rejected. Revalidate caller and
selected binding at spawn reservation; explicit destruction removes intent,
while daemon shutdown and ordinary harness return preserve it. Persistence
runs on verified lifecycle transitions, not recurring sampler/status updates.
A frontend picker remains a separate client feature.


## Contracts to make concrete

| Boundary | Responsibilities |
| --- | --- |
| Pane runtime | Real spawn/PTY/engine, process birth identity, resize, bounded I/O, snapshots/events, exit and explicit terminate. Views do not own process lifetime. |
| Canonical scene | Owned typed text/styles/widths, cursor, modes, colors, links, images/placements, damage and resource generations. A client doesn't parse a replay of the PTY into a competing terminal state. |
| Registry/layout | Durable endpoint/pane/workspace/layout identities and revisioned intent. Layout removal, disconnect and process termination are different operations. |
| View authority | Independent eligibility/capabilities for sizing, input, clipboard and control. Latest eligible visible attachment owns size unless pinned; input-read-only does not itself imply a sizing decision. |
| Control/metadata | One typed dispatcher for CLI/MCP, exact qualified targets, backend facts and namespaced frontend annotations. Creation doesn't imply focus, approval or readiness. |
| Endpoint transport | Bounded versioned framing, authentication, capability checks, cancellation, reconnect and resnapshot. SSH authentication alone doesn't authorize every pane operation. |
| Client projection | Local chrome/layout selection, actual terminal rendering/input/device replies, selection/scrollback, accessible interaction and platform-specific clipboard. |
| Agent services | Authenticated launch/conversation binding, truthful readiness, durable delivery/acknowledgement and appropriate native steering. Never type peer notifications into PTYs. |

Define identity epochs, revision ordering, subscriptions/coalescing, resnapshot, slow-client limits, permission expiry/revocation and error semantics before implementing concurrent consumers. A socket write or painted status badge is not acknowledgement of process input or model work.

## Delivery sequence

The local implementation now includes the seven-crate split, pinned safe SDK integration, typed Planus wire-family-6 control, independent PTYs, atomic multi-pane creation/placement, persisted workspace/layout metadata, bounded view authority, the retained-framework sidebar/forms/choosers, native harness capture/communication, explicit native conversation recovery and a single-existing-session SSH backend/headless connection. These are implemented portions of stages 2–6, not completion of every requirement below. Remote TUI attachment and endpoint-qualified composition, forge/CI services, a recovery picker, a second independent frontend and supported-product packaging remain future work. The [engine report](engine-proof.md) separates measured acceptance from unresolved gates.

### Current priority: restore Bkpr's main-branch frontend

The frontend parity target is Bkpr's own product layer on `main`, including the fork changes supporting it—not general Zellij feature parity. Restore the functional all-workspace sidebar, Bkpr's passive detail popups and focused forms/finder, context-sensitive effective-binding hints and window decorations, then Kitty-graphics chrome. Generic mux features such as zoom/fullscreen are not the next frontend priority. Reuse useful main/fork components through the single native composition framework; old plugin transports, helper panes and native-owner publication are not compatibility contracts. Web, GUI and mobile remain deferred.

The local overlay lifecycle, static graphics materials and main-aligned key-guide formatting now have actual rich/plain terminal evidence. The screenshot-reviewed [chrome correction](engine-proof.md#main-aligned-chrome-correction) supersedes the initial glossy/boxed appearance; the [key-guide pass](engine-proof.md#main-aligned-key-guide) preserves complete aliases and action explanations. This is not full overlay/provider or cross-runtime pixel parity: unavailable backend facts remain explicit. A command palette and decorative animation stay outside this pass. Performance work on existing terminal applications remains relevant without broadening that scope.


The current frontend slice is exercised in actual Kitty and xterm: sidebar navigation/folding/scrolling and double-clicks, metadata popovers without hidden views, identity-bound editable forms, name/cwd creation, metadata finder with compact previews, contextual hints, local clipboard paste and static SVG/text chrome. A popup covering a real divider consumes its drag without changing layout, native input or borrowed-pane membership. The [runtime report](engine-proof.md#bkpr-product-frontend) records exact candidates, source/dependency decisions and remaining backend/pixel-chrome limits.


The harness-capture slice is implemented:

- Supported foreground harnesses and CWDs are recognized from process-birth/PTY evidence. Titles do not supply identity or semantic status.
- Ordinary private-PATH wrappers and `bkpr run` use backend-owned launch identities. Native arguments, fresh/resume/fork/noninteractive behavior and unrelated settings remain authoritative; incompatible modes retain native execution without capture credentials.
- Claude hooks, an Omp extension, a response-correlated Codex app-server proxy and Antigravity root-MCP metadata bind exact native conversations. Hook/extension/connection presence is not communication readiness.
- Recognition, managed binding and genuine OSC 7501 status are separate protocol facts. Sidebar/header/chooser content preserves user names. Hidden-pane status is pushed without view admission or full-scene copying.
- Shared off-path process observations, generation checks and fresh exit-ownership barriers handle replacement, foreground/background transitions and shell return. Live grants, status and recognition are excluded from durable layout records.

Real Omp and Codex TUIs have isolated fresh/explicit/resume/fork acceptance, plus Codex picker selection. Claude's native first-run connectivity preflight prevents hooks in the offline fixture; Antigravity's Terms/Data Use prompt remains unapproved, so its genuine root-MCP callback is still an acceptance gate. Native Antigravity installation/configuration preservation and adapter/authentication boundaries have separate exercised evidence. These limits do not imply full harness or production parity.

Launch context, scoped discovery and durable peer communication are implemented against the new backend through shared MCP tools. Omp's extension supplies native binding/asides, not a separate tool implementation; its private package supplies MCP configuration and communication guidance. Claude tool-boundary/idle hooks, Codex exact-thread future-turn queue and Antigravity native receiver integrate without PTY peer input. Backend storage preserves exact idempotency and receipts, authenticates launch/receiver generations and suspends commits after durable failure. Installation, native binding and subscription readiness remain distinct. Actual acceptance and native prerequisites are recorded in the engine report, not inferred from source completion.

Local repository/worktree identity, branch/HEAD, working-tree summaries, committed merge-base totals, focused TUI details and shared MCP context are now implemented. Foreground CWD and event invalidation drive off-path collection; coverage failures and executable-filter preflight rejection remain explicit, never fabricated zero totals.

The native backend now implements one session per owned workspace, multiple independent window/layout documents per workspace and paired terminal workspace/window membership. Authoritative layouts reject foreign-window and foreign-workspace panes. Window revisions remain independent while workspace-close CAS covers child edits; optional-name workspace creation atomically commits its first window and terminal. Protocol 3 replaces the old single-layout API; durable schema 2 performs a deterministic, one-shot schema-1 upgrade without replaying argv and refuses ambiguous foreign layouts. Main's sidebar “Create terminal” uses a separate window, not a split. Full normal use of workspace references through their owner remains the cross-session/remote contract: input, creation, layout mutation and closure are not read-only. Assume the same user without introducing a permissions framework or weakening process-bound authentication. Cross-owner workspace-reference routing and forge/PR/CI enrichment remain separate work; preserve performance and the deferred sustained-output CPU investigation.

The user-approved lifecycle cascade is backend-owned: closing the last terminal, including normal shell exit, closes its workspace; closing the last workspace closes the session and exits attached clients after terminal restoration. Client detach and layout-only projection removal remain nonterminating. Apply the session rule to a last-workspace-close transition, not an intentionally empty headless daemon at startup. The count must not depend on which window a particular client currently displays.

### 1. Native branch baseline

Remove old runtime and build/release interfaces; make native `bkpr` the user entrypoint. Keep real engine/PTY/view/device behavior, relevant behavioral tests and historical evidence. Update README, project guidance and backlog without pretending unsupported old features still exist. Use only native checks; protect unrelated running programs and shared public artifact paths as an operational precaution, not a compatibility requirement.

### 2. Engine/client correctness and dependency adoption

Close the actual initial-image and startup-input observations with causal evidence, not periodic redraw, sleeps or false capability advertising. Exercise alternate screen, Unicode/width, reflow/scrollback, cursor, modes, key/mouse/focus, paste, links, colors and resource lifetime through real consumers. Keep exact MIME/selection/offer grants and query/reply isolation. Test allow/deny/stale/timeout/disconnect and unrelated-pane progress during clipboard consent.

Use source from the fork where it improves damage, image projection or device handling; prefer upstream libraries where their boundary fits. Replace redundant generic bindings through a compatible upstream pair. Sixel is currently unsupported: it is a product support decision before declaring terminal compatibility, not an obstacle to implementing the pane-first backend. Don't write a terminal engine to hide an adoption failure.

### 3. Multi-pane registry and layout documents

Implement durable independently owned panes, explicit termination, backend workspace membership and revisioned layouts with endpoint-qualified references. Demonstrate the same pane in multiple views, removal without termination, detach/reattach, deterministic size pins/leases and independent input/clipboard authority. Document daemon-crash and orphan policies honestly. Bound queues, memory, resources and stale generations; one slow view or synchronous engine callback must not stop unrelated panes.

Persist layout and metadata intent atomically. Keep desired attachment distinct from transient disconnect. Terminal history persistence is a separate opt-in product feature, not an accidental recovery record containing secrets.

### 4. Core TUI parity

Make normal launch, shell exit, workspace/pane creation, detach/reattach and daemon restart coherent before adding decorative metadata. Cold named-session startup restores owned layout identity and verified directories with the current shell, or a verified still-active native conversation through fixed current-configuration adapters and fresh authority. Do not replay arbitrary saved commands or infer native activity from historical descriptors. Retain failed intent without fallback; headless startup never executes saved history and live attachment never restores twice.

Exercise Bkpr's sidebar tree/cursor/folding/scrolling, passive hover/selection popups, focused creation/rename/finder overlays, effective contextual hints and decorations on the actual terminal. Preserve frontend-local selection/focus and source-row hit testing through metadata changes, geometry, modal cancellation and pointer handoff. Use published backend facts only; unavailable rich metadata must not be fabricated. Fancy chrome follows correct functional behavior and observed client capabilities, with a coherent plain fallback. Already implemented split/scrollback/selection/search behavior remains supported, but generic Zellij parity is not the frontend completion criterion.

Launch/restart shell replacement, per-view scrollback, terminal selection/copy, tiled-pane resizing and incremental terminal search now have actual terminal acceptance. Native tracked references, gestures, formatters and search supply generic behavior; the framework adds borrowed view-local highlights without modifying canonical scenes. Visibility epochs bind inspection lifetime; query acknowledgements and generations reject stale frontend results. Resizing uses one in-flight revisioned layout update with coalesced pointer intent. Search uses bounded pane-worker progress with no idle timer and records dense native result-memory costs explicitly. Cold named-session shell/native restoration is implemented; a frontend recovery picker and remaining genuine-host acceptance gates remain pending. Complete and report each agreed slice before advancing to another.

An independently implemented browser or narrow client remains a future acceptance check for additional frontend products, not a prerequisite for TUI parity. When that work resumes, it must consume the canonical protocol, handle resource/delta lifetime and reconnect, and coexist with an active TUI. Screenshots, hard-coded mocks or ANSI replay do not establish independent frontend support.

### 5. Shared control, recovery and metadata completion

Expose implemented workflows through one shared MCP API across harnesses. Restore active native conversations automatically on cold named startup and retain exact explicit recovery for historical/refused candidates, always with fresh launch authority rather than an exited launch's inbox. Native wake acceptance, reading and processed acknowledgement retain separate provenance; never use PTY typing or blind ambiguous replay for peer delivery. Explicit installations preserve unrelated settings.

Restore remaining project/forge/CI enrichment after core TUI workflows, preserving canonical worktree identity, actual checked remote/PR commits, selected provider account, batching/backoff and honest loading/error states. No Git fetch or provider call per redraw. Main's relevant Git/provider, recovery and agent code remains starting material, not a mandatory schema/storage design.

### 6. Authenticated remote endpoints

#### First milestone: connect to one existing remote session

The backend milestone is implemented through `RemoteSession` and `bkpr remote`;
its [actual SSH proof](engine-proof.md#remote-session-backend-and-headless-connection)
records exact binaries, bounds, native authority, failure behavior and limits.
The normal client [attaches directly over that endpoint](engine-proof.md#direct-remote-tui-attachment),
without a local gateway or second transport. It now supports multiple independent
local and remote attachments in one physical TUI. Native family 6, remote control 1
and durable schema 4 remain unchanged. Mixed-owner layouts and cross-session
workspace references remain deferred.

A compatible launcher/helper must already be installed on the remote host and
the selected session must already be running. Connecting does not install
software, create/restart a daemon, restore saved programs or merge local state.
The remote daemon remains the sole owner of its panes, layouts, processes,
harnesses, agent services and metadata. Terminal capabilities, theme, clipboard
and presentation stay local. The client discovers immutable remote launch
defaults before activation, including takeover; it never interprets remote CWD
or home paths against the local filesystem. Reuse the existing application and
framework paths for remote input, rendering and shutdown.

One local attachment owns one persistent system OpenSSH stdio connection with no
transport PTY and one remote helper process. The helper resolves the exact name
against remote private session state, returns stable session/current-daemon
identities and opens every native Unix connection from that same process. This
preserves native PID/birth-bound registration and request authority at the remote
daemon boundary; the authenticated SSH/helper lifetime is the cross-host trust
boundary, not a claim that a local desktop PID exists remotely. Pin the handshake
identity through native registration. Do not reuse a ControlMaster shared with
other attachments, issue SSH commands per request or expose the daemon as an
unrestricted TCP service.

Use maintained [Yamux](https://docs.rs/yamux/0.14.1/yamux/) for independent bounded
streams rather than implementing framing, credits and stream lifecycle ourselves.
Its default aggregate receive window is too large for this boundary; explicitly
bound windows, stream counts, queued opens, socket buffers and diagnostic bytes.
Reserve lifetime/control capacity apart from observations and split sending into
fair small chunks. A transport-confined current-thread Tokio runtime handles
SSH pipes and stream I/O; the native daemon, engine and existing local request
path remain synchronous. A connector returning local Unix stream pairs retains
the current cancellation, descriptor polling and cloned-endpoint interfaces.
Native Planus frames remain opaque binary bytes, including scenes/resources,
input/paste and clipboard messages.

The separate Planus remote-control protocol has its own version and bounded
admission header. It associates the requested name, native family/capabilities,
connection nonce, returned identity and optional remote agent source before
activation. A reserved control stream provides correlated heartbeat/lease
handling independently of scene backpressure. Expired leases, malformed control,
EOF and helper/SSH death retire the attachment and its views, not the session or
programs. Accepted mutations may complete; a lost result remains unknown and is
never replayed. TUI reconnect creates a fresh attachment/resnapshot only after a
previously active connection is lost; initial failures remain offline for explicit
retry. Logical identity stays pinned, and intentional retirement removes the target.
Old input queues, tokens, grants, bootstrap and takeover are never replayed.

OpenSSH owns encryption, authentication, host verification, aliases and
ProxyJump. Require known host keys and noninteractive authentication; do not
change profiles, known-hosts or credentials. Dynamic session names travel only
in framed control data, not interpolated shell commands. `--ssh-config` selects
a per-invocation config without modifying it. Agent forwarding is off by default;
explicit opt-in uses OpenSSH's existing forwarding and the helper's remote
`SSH_AUTH_SOCK`, never a local pathname or a second Bkpr agent-transport subsystem.
Remote native harness callbacks continue directly to the remote Unix socket
with their real OS/process/PTY authority; viewing a remote session grants no
local harness impersonation or merged agent discovery.

#### Multiple independent attachments and one sidebar tree

`bkpr` resolves repeated TUI targets and supplies one-shot local preparation to
`bkpr-client::run_sessions`. Remote-only startup does not resolve local HOME/CWD
or roots. Local preparation, SSH admission, activation and retirement run in
bounded workers; the physical input/render loop never joins them. Initial failure
and previously active transport loss have different retry policies. Force reconnect
invalidates the owner generation before worker admission. Transport cleanup owns a
child cancellation token, not its caller's attempt token.

`bkpr-client` owns session hierarchy, source-qualified cursor/actions,
fold policy, labels and metadata formatting. Each attachment keeps its authoritative
native registry and endpoint. The global tree consumes cached immutable metadata;
hover, keyboard browsing and popovers do not attach hidden native views. Session
removal hides the subtree immediately while stable tombstones finish origin-bound
receipts and cleanup. Explicit native retirement and nonempty-to-empty membership
transitions remove targets; bare EOF, initially empty headless state and retained
history do not establish semantic closure. The last remaining target's end exits
the client after TTY restoration.

Session separators are cached nonselectable rows, not decoration added during
rendering. Rebuild caches sibling continuations and parent row indices once;
composition walks at most three selected-to-parent edges to style only the
actual ancestor path. Inactive guides use a client-chosen 35% foreground blend
against the sidebar. Window labels remain neutral. The active native pane
supplies selection intent and trailing cells on both title and metadata rows;
only its title owns the hidden cursor anchor. Confirmed RGB cards share their
plate color, while unknown-background hover retains native inverse selection.
Session name and remote host are separate presentation fields, never routing
identities or parsed display strings. Shared grapheme-aware width budgeting
right-aligns the host and clips long names without splitting a grapheme.

`bkpr-core`, not the frontend, owns effective pane names. New unnamed executions
follow the retained latest OSC 0/2 title independently of the bounded effect ring;
empty native titles fall back to their base label. Explicit create/rename pins
the label. Private schema 4 records automatic naming intent, not transient OSC
contents; old records default to fixed labels because origin cannot be inferred.
Inspect, context, commit replies, automatic bootstrap and registry `Wait` share
the same effective projection. Metadata wakes include hidden-pane title changes
without opening a view, scanning processes or writing each title to disk.
Harness semantic status remains OSC 7501 and authenticated native facts.

The framework owns reusable retained `Navigation` selection/viewport/click mechanics
and the deadline-driven `Spinner`, parameterized by a static frame slice. Bkpr
supplies Nerd Font circle-slice frames, beekeeper glyph, hierarchy and styling.
Generic graphics, damage and native scene composition
stay in the existing framework; theme, chrome preference and toast queue belong to
one physical client, while native resource/device/release origins stay owner-qualified.

Dependency decision: evaluated maintained MIT-licensed
[`tui-tree-widget` 0.24.1](https://crates.io/crates/tui-tree-widget/0.24.1),
including its [dependencies](https://crates.io/api/v1/crates/tui-tree-widget/0.24.1/dependencies)
and [render/state implementation](https://github.com/EdJoPaTo/tui-rs-tree-widget/tree/v0.24.1).
It uses Ratatui's cell buffer; visible hit/navigation identities are populated by
its render pass and flattening allocates identifier paths per draw. The current
framework does not use Ratatui. Adapting that widget solely for this extra hierarchy
would introduce a second cell-buffer/render bridge and duplicate retained row state.
Extracted the existing row navigation instead, keeping one cached tree/render path
for single and multiple attachments. No upstream widget code was copied. The
[primary Nerd Font glyph map](https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json)
identifies `md-beekeeper` as U+F14E2, the requested `ple-honeycomb` as U+E0CC,
and server-network/server-network-off as U+F048D/U+F048E. The honeycomb uses
a PUA glyph followed by an ASCII space to reserve two cells, matching
[Kitty's documented private-use icon convention](https://sw.kovidgoyal.net/kitty/faq/#some-special-symbols-are-rendered-small-truncated-in-kitty).
No font-coverage guess, OSC 66 dependency or second glyph renderer is introduced;
the host still determines available font coverage and visual expansion.


Identity-only inspection performs the remote handshake without native client
registration or activation, so it does not consume a native slot, select an
agent source or take over clients. Only persistent connection accepts `-d`.
Normal connected requests use native managed client authority. Local `BKPR_*`
launch/context variables are removed before SSH exec; harness/agent operations
are rejected before opening a remote request channel rather than forwarding
local credentials or falling back to an administrative connection.

Acceptance must use actual isolated SSH and an already-running compatible
daemon: exact names, identity replacement, absent/stopped/unsafe targets,
host-key/helper/family failures, binary scenes and exact input/clipboard bytes,
simultaneous polls/input/control, resource backpressure, mutation reply loss,
helper/network failure, fresh reconnect and opt-in disposable-agent signing.
Prove detach leaves the original pane births alive and cleanup leaves no owned
processes. Keep observations and static design claims distinct.

#### Later milestones

Add TUI attachment only after the backend connection architecture and failure
semantics are accepted. Mixing workspaces from different local/remote sessions
is a later UX/design decision; do not assume mixed-session pane layouts,
aggregation or a local federation service in this first connection API.

### 7. Supported products and packaging

Declare the supported OS/terminal/client matrix from evidence. Browser/mobile need real rendering, font metrics, IME/soft keyboard, touch, accessibility, platform clipboard and authenticated connection handling—not just WebSockets. Background mobile views must not silently resize desktop panes.

Restore release/package outputs only with an offline reproducible native engine/toolchain dependency closure, licensing/notices, provenance and upgrade checks. Current Cargo/devshell operation must not be disguised as complete Nix packaging. No old-server import or live-PTY transfer is required; any useful declarative import is a separately designed feature.

## Verification

Run coordinated checks after related slices land: native workspace tests, all-target strict Clippy, formatting and an actual changed-path smoke. Only replacement-relevant tests count. Preserve behavioral boundaries, not source-text, wording, copies or incidental defaults. Do not run deleted Zellij/WASI checks to prevent regressions.

Use private HOME/XDG and sockets, owned process birth identities, fake clocks where useful, and actual maintained harness TUIs against isolated fake local providers. Never call real providers, change user profiles, auto-grant harness approvals or operate on live sessions for fixtures. Keep Linux process helpers/tests platform-gated. Retain useful versions, frames, protocol bytes, physical screenshots and receipts under this worktree's local `target/smoke/`, with exact cleanup.

Acceptance is tied to implemented capability: process/view independence, permission and generation boundaries, terminal/device fidelity, resource limits, authoritative metadata, genuine agent binding/delivery, recovery and remote trust. `main` and the fork supply evidence and starting code; they do not dictate the new design or an unchanged regression suite.

## Native harness adapter extensions

### OpenCode

The native integration uses OpenCode's own TUI and server plugin APIs, not a
replacement interface or ACP. Native API source was checked at
[`v1.19.5`, `810c0eba`](https://github.com/anomalyco/opencode/tree/810c0eba1a7348036f8bdf61e6632d0c36c6876c):
`packages/plugin/src/index.ts`, `packages/tui/src/component/prompt/index.tsx`,
`packages/opencode/src/session/prompt.ts`, `packages/opencode/src/effect/runner.ts`
and `packages/opencode/src/config/{tui,variable}.ts`.

- Passive TUI slots observe only the selected, typed root session. Child/home
  routes withdraw status and receiver eligibility without erasing the last
  authenticated root/recovery identity. Binding changes remain single-flight:
  the old bind's outcome is known before another is dispatched.
- The server's genuine `chat.message` hook reports the first body-free native
  submission in a **launch**, before native persistence. A→B→A retains that
  launch-wide observation; a fresh launch starts unknown despite saved history.
- Native peer pointers use the same `client.session` engine as the actual TUI.
  Its separate `client.v2.session` engine omitted the TUI's MCP and instruction
  context in genuine native requests and is not a substitute. Busy insertion
  preserves the existing native runner and the user's unsubmitted draft.
  `promptAsync` HTTP 204 is not durable admission: report `unknown`, never replay.
- One shared MCP serves the same catalog. Native plugins and MCP bootstrap read
  a launch-local credential file rather than placing bearer credentials in
  OpenCode's environment. Shared asset helpers create directories (0700) and
  files (0600) from the outset; no persistent integration installation is performed.
- Global/project settings remain native. Explicit TUI JSONC overlays retain
  nested/top-level precedence and literal relative resource origins. Unsafe
  runtime-dependent relocation leaves the native launch unchanged and unmanaged
  with a diagnostic, rather than guessing wrapper-provided environment values.
  Existing Omp callback plumbing is shared, not duplicated.

The only new Rust dependency is
[`jsonc-parser` 0.34.0](https://crates.io/crates/jsonc-parser/0.34.0), maintained by
dprint, MIT, published 2026-09-27; its serde integration handles native JSONC
without a custom comment stripper or optional CST machinery. Plugin runtime
imports are Node builtins and shared Bkpr glue. OpenCode still applies its own
SDK dependency-installation policy when loading server plugins; fixtures bound
offline npm retries without changing production flags or seeding approvals/caches.

User-approved semantic-status outlet: OpenCode alone currently uses a generic,
typed, retained backend connection instead of bypassing its private renderer.
Enrollment authenticates OS identity/birth and pane ancestry once; reports bind
the exact launch, native conversation, binding generation and revocable process
leases. Reports perform no process scans, filesystem/storage I/O or full-scene
clones. ACK means volatile publication, not durable storage or input admission.
EOF/replacement/revocation clears only that enrollment. Existing integrations
keep OSC 7501. The native protocol family is 9 and requires matching executables.

Actual 1.19.5 and installed 1.19.0 TUI/MCP/delivery/recovery evidence is in the
[runtime report](engine-proof.md#opencode-native-integration).

### Pi

The native integration targets
[`v1.5.2`, `b4afdddf`](https://github.com/earendil-works/pi/tree/b4afdddf44e059c67ccf5097e5317d913521b470):
`packages/coding-agent/src/core/extensions/types.ts`,
`packages/coding-agent/src/core/agent-session.ts`,
`packages/coding-agent/src/core/session-manager.ts`,
`packages/coding-agent/src/cli/setup.ts`,
`packages/coding-agent/src/modes/interactive/program-status-reporter.ts` and
`packages/tui/src/{terminal,program-status}.ts`.

- A private native extension awaits exact session-manager binding in
  `session_start`, before Pi accepts input. New/resume/fork/reload retire the old
  receiver and bind the replacement runtime. Tree navigation retains its native
  session identity. A launch-wide ordering barrier prevents replaying an
  ambiguous bind or first-prompt observation across extension reloads.
- Pi's own `registerMcpServer` supplies the shared Bkpr MCP through native default
  codemode. Native file configuration keeps its same-name precedence. No custom
  MCP client, SDK implementation or persistent configuration is introduced.
- `before_agent_start` records only actual user submissions, without prompt
  bodies. Extension-authored inbox messages do not create that observation.
  Native `sendMessage` steering preserves the editor draft, including while busy.
  Its void return cannot prove admission: wake outcomes remain `unknown`, without
  replay, independently of later inbox reads and processed acknowledgements.
- Native `ProgramStatusReporter` already owns OSC 7501 lifecycle reporting.
  Bkpr neither writes behind Pi's renderer nor uses the OpenCode status outlet;
  native status negotiation and `PI_PROGRAM_STATUS` remain authoritative.
- Capture credentials stay in owner-private launch files, not Pi's environment.
  Native extension and appended guidance flags precede the original arguments,
  preserving the user's terminator and malformed-option behavior. Noninteractive,
  protocol, utility, disabled-MCP/extension and ephemeral-session modes remain
  unmanaged native launches.
- Native recognition supports the maintained npm package entrypoints and its
  `process.title` rewrite to a sole `pi` argv value, only on a verified JS runtime.
  This is process observation, not terminal-title or semantic-status inference;
  process birth, foreground and ancestry authority checks remain unchanged.
- Recovery uses the exact authenticated native ID with current native
  `--session` configuration, never stored credentials, old command arguments,
  implicit forks or a fresh-conversation fallback.

Pi needs no new Rust or JavaScript dependency. Its maintained native extension,
MCP, codemode and terminal APIs cover the integration; shared Bkpr callback and
private-asset helpers provide the launch-specific glue.

