## Unreleased

### Fixes

* Reuse the validated native engine in `just build`, `just run` and `just test`. Re-extract, patch and prepare it only when missing or invalid instead of refreshing native inputs on every invocation.
* Route Omp tools through the shared launch-local MCP server and include communication guidance as a private package skill. Remove Omp-specific tool registration/schema conversion; keep the extension for native conversation binding and step-boundary notifications.
* Reap explicitly terminated pane processes before stopping their PTY writer. `portable-pty` emits newline/VEOF on writer drop; those bytes must not reach a still-live application or pending shell command.
* Use the stable launch-local MCP name `bkpr` for Claude, Codex and Omp instead of a launch-ID suffix. Keep authentication launch-scoped and persistent configuration untouched.
* Preserve the active keyboard layout when decoding Kitty control keys instead of substituting the alternate PC-101 key. Fix AZERTY Ctrl-W becoming Ctrl-Z while ordinary text remains correct.

### Breaking branch replacement

* `feat/pane-first-mux` replaces the previous runtime with seven focused crates: `bkpr`, `bkpr-client`, `bkpr-tui-framework`, `bkpr-server` (the `bkprd` executable), `bkpr-core`, `bkpr-protocol` and `bkpr-terminal-engine`. There is no legacy launcher, plugin, binary alias or compatibility track on this branch.
* Remove the old host-runtime flake input, WASI build/export paths, sibling-runtime recipes and obsolete package/cache/publication automation. Nix now supplies only the default native development shell; Cargo plus the pinned engine preparation helper is the supported build path. Native Nix packaging requires a reproducible, offline engine dependency closure and runtime acceptance before package/app outputs return.
* Expose the native daemon, TUI, typed version-4 control protocol and keyboard client. Backend-owned workspaces contain independent windows/layouts and owned panes; layout-only removal or client detach does not terminate a process. Metadata persists, but process and terminal-content recovery is not implemented. Forge/CI services remain unavailable. Source from current `main` and the native fork remains starting material for reuse or redesign, not a frozen API or regression requirement.
* Native graphics initial-presentation gates remain unaccepted. This replacement is not production acceptance and does not modify installed applications or live sessions.

### Native named sessions

* Restore `--session/-s` with default `bkpr`, exact UTF-8 names and separate daemon/state roots. Add read-only `bkpr list` and explicitly targeted `bkpr stop`; serialize concurrent bootstrap and validate stored/live identity without automatically restarting a live incompatible server.
* Add process-bound, ephemeral client registration and a dedicated lifetime stream. Initialize the TUI before one-shot `-d/--detach-others` activation; revoke other attachments without closing programs or layouts, waiting for their acknowledgement, or detaching clients that arrive later.
* Bind managed views, queued native callbacks, device grants and sizing to revocable client authority. Retire it on EOF, owner exit or detach, including fork-inherited lifetime sockets. Inject trusted logical session ID/name snapshots before pane exec.
* Move daemon bootstrap out of the terminal-client library into the launcher. Keep headless tools nonbootstrapping, separate lifetime/watch/input admission, and keep process/CWD scans off managed per-key authentication.

### Native pane context

* Inject canonical immutable session, pane and workspace identities before every native exec. Reserve automatic workspace names before process startup, include pending names in metadata-only creation and release failed reservations without blocking unrelated workspaces.
* Replace the old whole-registry context forwarding helper with typed live `context` queries. Default selection verifies the caller's process birth, ancestry and controlling PTY; explicit `--pane` selects exact same-user live metadata. Add compact `--json`, keeping default output indented and headless.
* Read current labels and execution membership without trusting inherited names, client focus or borrowed projections. Reject stopped/closing/foreign/nested-PTY contexts; retain immutable old process snapshots after rename or membership changes. No native settings, input/view authority, persisted credentials or new dependencies are introduced.

### Native SSH-agent forwarding

* Give ordinary managed panes a stable daemon-owned agent socket. Select the latest live activated attachment, including explicit absence; registration and unselected detach do not reroute signing. Retain the listener at zero clients and fall back only when the selected attachment retires.
* Forward opaque bidirectional streams without parsing extensions, framing limits or approval timeouts. Fence retired connections before further I/O and never replay a partial signing request against a new source.
* Bound buffering, connection admission and filesystem/connect work independently of terminal input/rendering. A stuck upstream pathname cannot stall established streams, route revocation or reactor shutdown.
* Keep isolated fixture panes credential-free, preserve raw Unix paths, hold an explicit advisory socket lease and remove only owned socket inodes. Reserve companion-path space before named startup; never modify persistent SSH/Git settings.
* Protocol 4 adds one-shot launch-local agent source publication to authenticated client activation. Use a matching fresh client/server pair; incompatible live daemons fail closed rather than being restarted.

### Owned workspace windows and session closure

* Own each workspace in one session, with independent windows and explicit terminal membership. Reject foreign-window/workspace layout borrowing; keep active-window choice per client and destructive changes revision-guarded.
* Restore separate full-window terminal creation rather than substituting a split. Create optional-name workspaces and their initial window/terminal atomically; choose collision-free names on the backend and preserve exact native directories.
* Close windows/workspaces by terminating their owned executions, including hidden or unprojected terminals. Cascade normal last-shell exit into workspace and final-session closure, notify attachments and reap residual administrative executions without requiring client acknowledgement. Detach, layout-only removal and initially empty headless startup remain nonterminating.
* Drain deferred sibling completions after close reservations, retain de-owned exit receipts and bound final-response delivery with an absolute nonblocking write deadline. Cancel retired client snapshot generations and settle accepted policy revocation before shutdown detach.
* Replace the single-layout API with protocol 3 and durable schema 2. Upgrade unambiguous schema-1 metadata once without command replay; reject ambiguous foreign layouts without replacing metadata. Cross-session/SSH workspace routing remains separate work, with normal same-user use through its owner rather than read-only references.

### Bkpr frontend restoration

* Restore the all-workspace sidebar's independent keyboard selection, folding, scrolling, rename/create/close-terminal actions and passive metadata popovers.
* Restore focused name/directory forms, grapheme-boundary editing and paste, identity-bound modal intent, honest asynchronous errors and the metadata-only fuzzy finder with filters and responsive previews.
* Share typed binding dispatch with contextual hints; compose bounded popup/guide windows without taking native pointer gestures or leaking UI keys into applications. Normalize legacy Ctrl-Space/NUL at the framework input boundary.
* Put retained UI surfaces, text editing, window decoration, capability gating and bounded off-thread SVG preparation in the reusable TUI framework. Keep Bkpr-specific models/assets in the client. Restore static bee/hive artwork through the existing scene/image lifecycle with terminal-default colors and readable unsupported-host fallbacks.
* Reuse foreground-identity and project freshness presentation across metadata views. Preserve native clipboard framing for applications while local forms/finder receive ordinary paste, restoring the host's original mode on detach.
* Derive a shared framework theme from confirmed terminal color capabilities and observed foreground/background/ANSI colors. Apply readable surfaces, selection, muted text and semantic accents throughout Bkpr chrome; retain native/ANSI fallbacks and canonical application styles. Refresh on actual private theme notifications without polling, preserve retained field/finder state and tint artwork from its final displayed ink.


### Development

* Use the pane-first worktree's own incremental Cargo target and native prerequisite directory instead of sharing main's cache. Preparation helpers default to that local target; builds remain limited to eight jobs. Research maintained, widely used dependencies before sizeable custom implementation.

### Native workspace implementation

* Bare `bkpr` connects to or starts `bkprd` and opens the TUI. One retained framework composes the sidebar, terminal projections, forms and choosers, including physical pointer hit testing and per-pane graphics namespaces.
* Use a pinned, source-auditable `libghostty-rs` SDK snapshot for generic terminal ownership and encoders. Local SDK patches preserve the tested native revision, clipboard-offer provenance and a pre-allocation decoded-image budget; exact build provenance remains verified.
* Separate size policy, input and clipboard authority. Register sizing-policy changes without waiting for blocked native work; bound pane/view/control admission and preserve FIFO ordering for accepted input.
* Dispatch attachment/management work asynchronously and isolate native input per pane, with reserved device-response admission and explicit unknown-outcome handling. Keep complete pointer gestures bound to their original pane/view through cross-pane dragging, backpressure and projection retirement.
* On fresh daemon startup with explicit bootstrap argv, create a new uniquely named `Main` workspace when retained metadata has no running executions; preserve historical layouts and never replay old commands. Direct `bkprd` startup without bootstrap argv stays headless. Attaching to an existing daemon does not spawn or reset layouts; initial TUI selection prefers a running projection.
* Add uppercase `Ctrl-b R` to confirm a fresh configured shell in an unavailable pane's selected-workspace position. Preserve the old record and other workspaces, use revision-guarded atomic replacement, keep failure/unknown outcomes honest and block read-only clients. Retained directories never silently fall back; confirmation offers an explicit current-directory choice. `Ctrl-b n` remains a new split. This does not restore processes, harness conversations or durable terminal history.
* Paint bounded changed-cell spans rather than whole composed rows, avoiding redundant static sidebar output while preserving wide/combining text, styles, links, erasure, graphics and cursor behavior.
* Keep sustained-animation CPU efficiency explicitly deferred; no frame throttling or fidelity reduction is introduced to conceal it.

### Native scrollback navigation

* Add attachment-local TUI history navigation: `Shift+PageUp`, `Shift+PageDown` and `Ctrl-b s`; arrows move rows, PageUp/PageDown move overlapping visible pages, Home reaches oldest retained rows, and Esc/Enter/End return live. Keep existing workspace shortcuts unchanged, show history offset and a live-return hint, hide the application cursor, and block unintended typing/paste while inspecting.
* Route vertical wheel input using exact native mouse-reporting and primary/alternate-screen facts. Shift+wheel explicitly navigates history; ordinary wheel retains application ownership when reporting is enabled, the alternate screen is active, or the daemon has not advertised exact-mode support. Alternate-screen history reports unavailable instead of exposing the primary buffer.
* Anchor each view in bounded native in-memory history while output continues, without copying full history per key or changing another client's view. Preserve accepted key/pointer releases, paste FIFO and clipboard grants; allow read-only inspection. Projection retirement, workspace switch or detach starts a later view live; no selection/copy/search or persistent terminal-history recovery is added.

### Native selection and copy

* Add per-view drag, double-click word, triple-click line and Alt-rectangle selection using Ghostty's maintained gesture and formatter APIs. Track history endpoints across output/resize and keep edge autoscroll, highlights and pointer capture local to the originating view.
* Copy completed selections on release and with `Ctrl-b y` through existing terminal clipboard framing. Permit read-only inspection/copy, respect client deny and host permissions, reject oversized copies rather than truncating, and discard stale exports after focus/selection/view retirement.
* Publish selection-only changes as bounded view annotations without cloning/resending canonical scenes. Separate visibility epochs from sizing activity so focus/lease updates do not reject queued selection. Report known selection rejection without stopping application input; ambiguous transport remains non-replayable.

### Native tiled-pane resizing

* Add `Ctrl-b e` resize mode: arrows move a containing divider by one cell, Shift moves five, held arrows repeat, and Enter/Esc finishes without application input. Add left-drag resizing on real one-cell divider gutters outside pane content.
* Share partition arithmetic across pane projection, divider drawing and hit testing. Preserve unrelated ratios/topology, clamp useful subtree sizes and keep small-window projection separate from saved layout intent.
* Reuse the existing revisioned backend layout API and bounded background pool. Retain only the latest pending pointer position behind one in-flight commit, preserve final release, block read-only mutation and retire foreign/focus/modal/geometry changes without replaying ambiguous updates.

### Native pane rearrangement

* Add `Ctrl-b Shift+Arrow` to exchange the focused pane with the nearest pane in that direction. Keep unmodified prefix arrows as focus navigation, preserve split geometry and focused execution, and publish the saved arrangement to other clients without moving their focus.
* Reuse asynchronous revision-checked layout updates. Reject read-only changes and additional moves during pending workspace management; preserve processes and existing views, and never replay ambiguous mutations.

### Native terminal search

* Add `Ctrl-b /` literal terminal/history search through the pinned SDK's incremental search APIs. Support text/paste/backspace, Ctrl-u, next/previous matching with Enter/Shift-Enter, honest running/count/no-match status and Esc clear with explicit history live return.
* Keep queries, native result state, history navigation and highlight annotations view-local. Reuse existing pane workers and input lanes, coalesce queries, reject stale generations, preserve application-owned releases and allow read-only search without input/clipboard authority.
* Recover a real newest native match when resize/reflow invalidates the chosen one. Preserve whole wide/combining glyphs, styles and resources; terminal selection takes precedence over chosen/ordinary search highlighting. Record dense native result-memory costs without silently truncating matches.

### Native harness capture

* Recognize foreground harnesses and directories, authenticate backend-owned launch identities and bind native Claude, Codex, Omp and Antigravity conversations through their own hooks, extensions or protocols. Restore ordinary-command wrappers and `bkpr run`, preserve native argument/exit behavior, and keep recognition-only and incompatible startup modes credential-free.
* Show recognition, managed binding and genuine OSC 7501 status separately in sidebar/header/chooser content. Preserve user names; clear stale facts on replacement, shell return and exit; retain a live launch across foreground/background transitions. Push hidden-pane status without opening terminal views.
* Add explicit `bkpr install agy` and dry-run with conflict protection and native-CLI preservation of unrelated settings. Normal launches do not rewrite harness configuration or approve native onboarding.
* Share process sampling outside input/render paths and global state locks; use fresh exit-ownership barriers before reaping. Preserve non-UTF-8 Unix directories over the control protocol without corrupting registry state. Keep capture capabilities and live associations out of durable layout records.
* Stage the matching `bkpr` utility with pane wrappers and honor the pane's inherited socket for headless context/control. Keep remote endpoints and the sustained-animation CPU-efficiency work deferred.

### Native harness recovery

* Persist only authenticated native harness kind, conversation ID and raw working directory in private registry version 3. Upgrade older layout records without inventing native bindings; keep tokens, launch/inbox authority, terminal contents and native settings out of recovery storage.
* Add read-only headless `bkpr resume`, exact `--pane` selection and allowlisted shared-MCP `control` listing/resume. Default agent control to live execution-workspace scope; explicit session/all remain daemon-local, with no repository scope or arbitrary command execution.
* Resume through fixed native adapters with current configuration, fresh authority and explicit pending-binding results. Preserve original intent until the selected native conversation binds; reject stale/duplicate/destructive races and never replay saved argv, select a provider or silently fall back to a fresh session.
* Recovery remains explicit native-conversation restoration, not process/terminal-content resurrection. A frontend picker and the existing genuine Claude/Antigravity native acceptance gates remain open.

### Native agent communication

* Add backend-owned launch context, titles, scoped discovery, durable send/inbox/processed acknowledgement and metadata-only outbox tools. Enforce exact sender-scoped idempotency, receiver-only access, live execution membership and canonical repository scope.
* Persist private messages and receipts without authentication grants. Retain unacknowledged bodies for seven days after persisted recipient retirement, purge bodies at their deadline, preserve receipt/wake history and retire old authority on restart. Suspend all commits after durable-store failure and report ambiguous commits truthfully.
* Integrate shared MCP tools with native Omp asides, Claude tool-boundary and idle hooks, Codex exact-thread future-turn queue and Antigravity native receiver tools. Preserve existing MCP registrations with launch-local namespaces; no persistent installation changes outside explicit opt-in and no PTY peer delivery.
* Distinguish native binding, helper registration and actual subscribed readiness. Coalesce opaque notifications, guard outcomes by exact receiver/generation and never blindly replay ambiguous delivery. An unobservable native offer remains unknown, distinct from inbox read and processed acknowledgement.
* Reserve independent receiver admission, support caller-owned cancellable transport and block idle receivers on push notifications rather than fast polling. Use maintained native filesystem notifications for Claude hook coordination.

### Native local projects

* Publish transient backend-owned local project/Git snapshots for pane directories, with independent worktree HEADs and shared canonical repository identity. Keep Git work off input/render paths and expose loading, non-repository, error and unknown-base states instead of fabricating ready/zero results.
* Annotate pane headers/sidebar with project, worktree and branch; add `Ctrl-b i` focused project details with Unicode wrapping and arrow scrolling. Show local staged/unstaged/untracked data and whole HEAD-versus-merge-base committed totals without renaming panes or reclassifying workspaces.
* Include the same cached project facts in authenticated `whoami` and detailed discovery. Resolve repository scope from ready backend snapshots rather than launching Git during agent requests. Forge/CI metadata, PR-target diffs and exact process/terminal-content restoration remain unavailable.
* Preflight-reject tracked paths selecting configured executable Git `clean`/`process` filters, including clean working trees where dirtiness checks could invoke them. Report an explicit statistics error and label retained values stale rather than silently alter filter semantics. Disable fsmonitor, external diff and textconv for metadata commands; this is not an adversarial repository sandbox against concurrent configuration/attribute changes.

## Previous implementation — unreleased historical notes

The entries below record work preceding the native branch replacement. They do not describe capabilities exposed by the current executable.

### Features

* Add Antigravity launch identities, MCP conversation binding, native peer wake and exact-conversation recovery.
* Add `bkpr install agy` with native MCP registration, dry-run and conflict protection.
* Prefer native active-turn steering for peer notifications: Antigravity uses native agent messages, Omp uses agent-step asides, and Claude adds tool-boundary context while retaining idle wake. Codex retains future-turn delivery.

### Maintenance

* Add an isolated pane-first engine development experiment and record its build recipe, runtime evidence and unresolved adoption gates. The shipping runtime remains unchanged; this isn't the full Zellij replacement.

### Breaking Changes

* Relay protocol 4 adds authenticated AGY native wake outcomes. Older brokers fail closed and must be stopped explicitly after inspecting pending work; live brokers are never replaced automatically.

## [0.4.2](https://github.com/calops/bkpr/compare/v0.4.1...v0.4.2) (2026-10-26)

## [0.4.1](https://github.com/calops/bkpr/compare/v0.4.0...v0.4.1) (2026-10-26)

### Bug Fixes

* **deps:** pin corrected native package version ([f65dcc0](https://github.com/calops/bkpr/commit/f65dcc0193a5b919500b8cff3779e063415723ee))

## [0.4.0](https://github.com/calops/bkpr/compare/v0.3.0...v0.4.0) (2026-10-26)

### Features

* share focused overlays for finder and creation forms ([90e7fcb](https://github.com/calops/bkpr/commit/90e7fcb9ac7c08a7f452b3be353fc9483a84e874))

## [0.3.0](https://github.com/calops/bkpr/compare/v0.2.2...v0.3.0) (2026-10-26)

### ⚠ BREAKING CHANGES

* **relay:** discover_agents returns compact records unless detail
is true, and title no longer defaults to "{harness} agent".

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
* **context:** Workspace scope now means the live Bkpr workspace, not repository/CWD grouping. Native 0.45.1-bkpr.3 and broker protocol 3 require fresh native sessions and explicit broker replacement; retained messages and receipts preserve their original scope.
* **bridge:** send_message requires an idempotency_key. Broker protocol
2 rejects older clients and brokers; restart the broker explicitly and
relaunch managed harnesses after preserving their pending work.

### Features

* add explicit harness launches and eager Codex messaging ([84c7621](https://github.com/calops/bkpr/commit/84c762199606eb18a5cb1598cbc474ceda6a99b0))
* **bridge:** make delivery state and retries explicit ([8f54543](https://github.com/calops/bkpr/commit/8f5454321926b9b4e2a3514d68a7b7fdd137ea94))
* **context:** scope agents to native workspaces ([2ee49d4](https://github.com/calops/bkpr/commit/2ee49d4d1a1f66bdb7ca1f004659ce4ab82c59c4))
* **control:** add typed CLI and MCP session control ([a508e67](https://github.com/calops/bkpr/commit/a508e67c2906d82bc1f49c523b2fc06b406183c5))
* **control:** use baseline APIs and add bottom-relative pane reads ([83d5893](https://github.com/calops/bkpr/commit/83d5893c09a6310672182fb37c17053b2bb37e93))
* merge live harness status discovery ([b056fe5](https://github.com/calops/bkpr/commit/b056fe53b50be9565816173642daefa3602e18cc))
* **omp:** bind native session ids for recovery ([b1e485e](https://github.com/calops/bkpr/commit/b1e485e8f9ec31d75e511bde5f81caad5e1c1e55))
* **recovery:** add bkpr resume and overwrite prompt ([bcbbc69](https://github.com/calops/bkpr/commit/bcbbc699a882294cd5352a28fcc1c16d3e1b892b))
* **recovery:** add restore-pane leaf launcher ([e56aef4](https://github.com/calops/bkpr/commit/e56aef49e9ce80beef77a5c1a8824aec07518120))
* **recovery:** add versioned recovery record and restore layout ([a31b8a0](https://github.com/calops/bkpr/commit/a31b8a0ef6db5cac7c4ba0e08c11462d0c106fd5))
* **recovery:** persist owner-claimed recovery records ([b62170e](https://github.com/calops/bkpr/commit/b62170ee0f68da42a35d121360fa703cae0dd145))
* **recovery:** resolve resumable harness sessions from relay state ([7d5e08f](https://github.com/calops/bkpr/commit/7d5e08f46a727de395c2b35c2cff0c46c0dff6f3))
* **recovery:** seed restored pane names and hold the restore baseline ([70b119c](https://github.com/calops/bkpr/commit/70b119c8b2f9dc638d9638aca198224facf37495))
* **recovery:** stream owner topology into recovery records ([4385dae](https://github.com/calops/bkpr/commit/4385dae7e39fbff472e11435fb555ee49feda94e))
* **recovery:** type harness resume commands into the restored shell ([6653bc7](https://github.com/calops/bkpr/commit/6653bc71efdf387f17b91afd4bc968fff6481ee2))
* **relay:** compact agent discovery with separate titles ([4af6d2c](https://github.com/calops/bkpr/commit/4af6d2cadf45225ba2503b25152eed9e25ce1c37))
* **relay:** expose live harness status in agent discovery ([a4a7e7b](https://github.com/calops/bkpr/commit/a4a7e7b7579b182e7e608f4878947ff5a9510eaa))
* **ui:** animate working harness clock hands clockwise ([d16e7e3](https://github.com/calops/bkpr/commit/d16e7e32fe35155d7b2019ab3babd64dff800c32))

### Bug Fixes

* allocate project colors across the whole session ([b8ecfb2](https://github.com/calops/bkpr/commit/b8ecfb24d7554df67f8564edad1f97695c3260f8))
* **bridge:** accept Codex native socket indirection ([fedef93](https://github.com/calops/bkpr/commit/fedef93fca8bc7966a52ea45ad3470a9a0690d46))
* **bridge:** keep the Codex MCP server name stable ([71d2752](https://github.com/calops/bkpr/commit/71d2752d54a2c84fc8b7a1a64539392a4c98877e))
* pin native runtime with terminal palette support ([1ece95b](https://github.com/calops/bkpr/commit/1ece95b9f55a2a9063e8f1c0a2fdc9e9ffc9d26a))
* preserve attachment to pre-context sessions ([3a6d9dd](https://github.com/calops/bkpr/commit/3a6d9ddcc365bc51f529c19141895a30fc252325))
* preserve sidebar controls and stop polling completed checks ([64f2535](https://github.com/calops/bkpr/commit/64f25350af398eae5a59211440debbd1c9162f73))
* **recovery:** detach the shared relay broker from the pane that starts it ([d4131b1](https://github.com/calops/bkpr/commit/d4131b1a4d8f21691ceb7a61dcc782a67d1295c3))
* **recovery:** harden restore layouts, owner sequencing and record ownership ([a8e5d9e](https://github.com/calops/bkpr/commit/a8e5d9e84ac25256adcbfefffaa70d58cd6a6549))
* **recovery:** hide the typed resume command and check flags by position ([a5c90e7](https://github.com/calops/bkpr/commit/a5c90e786fa02985ddf9a2e3eb8c1e83c0ea95ae))
* **recovery:** keep a refused harness resume's link for a later retry ([e43e08f](https://github.com/calops/bkpr/commit/e43e08fbc7a9684d8ced4e71544e689350b8ee5e))
* **recovery:** keep fresh launches working without recovery state ([e724309](https://github.com/calops/bkpr/commit/e72430910a75bfaf5441ed8e70420d6a932f7181))
* **recovery:** keep restored panes usable through job control and denial ([1b7c3c6](https://github.com/calops/bkpr/commit/1b7c3c6ef5aadf99e98e98be2fcfeed400265616))
* **recovery:** record hidden stack members the runtime parks as suppressed ([922e9f0](https://github.com/calops/bkpr/commit/922e9f0542c9d19fcfd2ac85c7e754cc8c7f87e5))
* **recovery:** refuse resume while the agent's own process is live ([26bf485](https://github.com/calops/bkpr/commit/26bf485fb60f8098dfe0206691dbed60690fe9ba))
* **recovery:** resume each native session from one restored pane only ([5f85912](https://github.com/calops/bkpr/commit/5f85912ceeb8fa64222a45f5999f6f6297e27a6a))
* **recovery:** retry unacknowledged recovery writes with backoff ([582a58e](https://github.com/calops/bkpr/commit/582a58ec6e4056018a3708abcb1b63f25323e575))
* **recovery:** start the relay broker before stopping a previous Codex server ([4d7909a](https://github.com/calops/bkpr/commit/4d7909a8ceed5bd9d1119039b5763d13dd0224c0))
* **relay:** prune exited agents and keep messaging on resume ([b0a4172](https://github.com/calops/bkpr/commit/b0a4172a408f412ae203d957e1c5b50184f4a5a8))
* **release:** keep automatic versions within 0.x ([31788d1](https://github.com/calops/bkpr/commit/31788d1b07fc7b024aa5bbaf11e413fa55b34f20))
* **sidebar:** preserve current titles across owner context updates ([34e7004](https://github.com/calops/bkpr/commit/34e7004f7125a4dac99b03922bbe959346a937e9))
* start attachments locked and highlight unlocked hints ([fb28ad9](https://github.com/calops/bkpr/commit/fb28ad900c1475b5786c58411d4fcaf24c5a4810))
* **ui:** apply native terminal colors across plugin roles ([7f9c5ae](https://github.com/calops/bkpr/commit/7f9c5ae4a3cb465d0da6c95e055a178b277f2e03))
* **ui:** soften accents and shade terminal-native surfaces ([4e2aeb4](https://github.com/calops/bkpr/commit/4e2aeb46b3b9df45135c4c5821df0dacb49051a2))
* **ui:** use the terminal palette for sidebar and popup styles ([a6bec51](https://github.com/calops/bkpr/commit/a6bec51bca6e0e79e9c5787d8df1eb7acebe06ff))
* use terminal ANSI colors for native frames ([dc57c95](https://github.com/calops/bkpr/commit/dc57c95299a27ec415672efc931192ce72873a4d))

## [0.2.2](https://github.com/calops/bkpr/compare/v0.2.1...v0.2.2) (2026-10-19)

### Bug Fixes

* **ci:** scope GitHub execution and sharing to item cwd ([a8d5744](https://github.com/calops/bkpr/commit/a8d5744349e81bee29d038f07bddd00549a40622))
* **nix:** keep Rust source paths out of the runtime closure ([ac76218](https://github.com/calops/bkpr/commit/ac76218bb4bf1ec7a5d9ccb35bc81c9f735e117f))
* **release:** publish tags and dispatch builds without channel notes ([f426e43](https://github.com/calops/bkpr/commit/f426e4319a399ce4426d2fc1ef9a7241f594f24a))

## [0.2.1](https://github.com/calops/bkpr/compare/v0.2.0...v0.2.1) (2026-10-19)

## [0.2.0](https://github.com/calops/bkpr/compare/v0.1.1...v0.2.0) (2026-10-19)

### Features

* **clipboard:** support image clipboard access through the native fork ([a4258e7](https://github.com/calops/bkpr/commit/a4258e70928c46780fc3325dd24a060b07a32d47))
* opt native sidebars into session-wide remembered widths ([81dca19](https://github.com/calops/bkpr/commit/81dca19b9f5d16ed99f06b2da5fd711177e0bd7d))

### Bug Fixes

* bound popup redraws and track remote CI heads ([901485e](https://github.com/calops/bkpr/commit/901485ea35f429177058b4420b3b647fb50e19f7))

## [0.1.1](https://github.com/calops/bkpr/compare/v0.1.0...v0.1.1) (2026-10-16)

### Bug Fixes

* **nix:** include bundled OMP extension in package source ([627e416](https://github.com/calops/bkpr/commit/627e41636d388bf30461cec488b134cf41493f2e))
