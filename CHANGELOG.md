## Unreleased

### Fixes

* Include the serving daemon's native wire family as `protocol_version` in compact and detailed `whoami` replies.
* Strip OMP's leading pi/status decoration from recognized harness titles in sidebar, pane captions, popups and finder, preserving raw titles and ordinary program text.

## [0.6.0](https://github.com/calops/bkpr/compare/v0.5.1...v0.6.0) (2026-11-10)

### ⚠ BREAKING CHANGES

* **agents:** Native wire family 8 requires matching bkpr and bkprd binaries. Agent/control outputs are compact by default; use explicit detail, full replies and body/result pagination. Existing incompatible sessions are not restarted automatically.
* **core:** the exported fixed-count admission constants are removed from bkpr-protocol. Native wire family 7 and operation encodings remain unchanged.
* **ci:** `bkpr` and `bkprd` require matching native wire family 7. Existing family 4/5/6 sessions need their matching old client or an explicit stop before upgrading.
* **client:** Remove `Ctrl-b w`, `Ctrl-b W` and `Ctrl-b o` chooser shortcuts and their dedicated forms. Use the sidebar or finder instead.
* **client:** Ctrl-] no longer detaches. Use Ctrl-b d. Prefix b/S, c/N, r/p and !/X are removed; use Ctrl-Space and sidebar n/N/r/x instead.

### Features

* **ci:** restore GitHub checks and PR cards ([88902be](https://github.com/calops/bkpr/commit/88902be428c62d109848714026bfd1ca3bc59dce))
* **client:** group related keybinding hints ([df89ad9](https://github.com/calops/bkpr/commit/df89ad99c7b8fa8919f4a443a3a031d46c82dcb9))
* **cli:** generate shell completions from clap ([9a42800](https://github.com/calops/bkpr/commit/9a4280014c406fff49f2094b52c3b2cc8732a51c))

### Bug Fixes

* **agents:** bound tool output and release retired resources ([6556b97](https://github.com/calops/bkpr/commit/6556b9756a0685ace3495d8ca3340cd35c9571b9))
* **client:** keep sidebar status icons inside viewport ([41bb5ed](https://github.com/calops/bkpr/commit/41bb5edf0937f514c6df85df39ca1efd21169d84))
* **client:** preserve sidebar separator and highlight ownership ([7513b13](https://github.com/calops/bkpr/commit/7513b137ffee91636f4e8b6b1cb5d8ea7e18c1a8))
* **client:** refine grouped hints and remove chooser shortcuts ([cdf88d1](https://github.com/calops/bkpr/commit/cdf88d1df10bf12dbfaa7b76ae5efb1e5888aec6))
* **client:** settle held keys on terminal focus loss ([a82aec4](https://github.com/calops/bkpr/commit/a82aec4577328ce1d70be1dac9686cf86d41af3c))
* **cli:** make help concise and task-focused ([11b88e2](https://github.com/calops/bkpr/commit/11b88e2e6f6d581690e609ce35a1af9b9f85a69a))
* **core:** remove fixed pane and attachment ceilings ([2318408](https://github.com/calops/bkpr/commit/23184081ab252af1c69040b4cf88d43969555293))
* **git:** isolate refreshes and stabilize cached metadata displays ([3113c65](https://github.com/calops/bkpr/commit/3113c65479fc6367bd5007da28d8d88abd3e7639))
* **tui:** clear complete sidebar frame gutter ([740507b](https://github.com/calops/bkpr/commit/740507b25426cdb89fbd73d6b969d9e363147dac))
* **tui:** crop sidebar-adjacent frame gutter ([7d833a6](https://github.com/calops/bkpr/commit/7d833a6a0e3d33643386632c6540e0a885f908ce))
* **tui:** preserve sidebar frame bevel and shadow ([d486adf](https://github.com/calops/bkpr/commit/d486adf74264e989cd691945bb9418749f00d462))

### Code Refactoring

* **client:** separate sidebar management from prefix commands ([dd56c77](https://github.com/calops/bkpr/commit/dd56c773ca171390a44e1b94c690f0bfdc9af58d))

## Unreleased

### Breaking changes

* Remove the `Ctrl-b w`, `Ctrl-b W` and `Ctrl-b o` chooser shortcuts and their dedicated forms. Use the sidebar or finder instead; keep the directory-choice overlay for reopening an unavailable pane.
* Native wire family 8 requires matching `bkpr`/`bkprd` binaries. Agent/control replies are compact by default; full metadata/body reads require explicit detail, pagination or `--reply full`. Existing incompatible daemons are not restarted automatically.

### Fixes

* Bound MCP tool output, shorten shared instructions and return IDs/revisions instead of whole registries. Add filtered inspection, trimmed revision-fenced viewport pages, complete message body pages and authenticated temporary result expansion.
* Separate unread summaries, explicit processed acknowledgements and quiet delivery; release expired wake claims without replaying ambiguous wakes. Keep historical delivery receipts and recovery metadata.
* Reap verified adopted zombies while panes remain alive, retain birth-bound root ownership through sampling gaps, retry faulted-pane cleanup and narrow window-close reservations.
* Recognize native Claude installer paths, expose body-free first-prompt and OSC status-change observations, and add private bounded lifecycle diagnostics without logging prompts, argv or message/terminal bodies.
* Mute grouped-hint separator punctuation without dimming literal punctuation keys. Shorten descriptions, group workspace folding and sidebar exit bindings, and label `Ctrl-b x` as “remove from layout.”
* Keep sidebar activity icons in the full text viewport by making only the flat exterior matte of the adjacent straight rich frame transparent; preserve its bevel, inner shadow, corners and internal opaque seams.
* Keep the sidebar separator column on the sidebar background in both rich and plain rendering, independently of pane frame colors.
* Extend sidebar item highlights across the first padding cell and shared separator, preserving the full two-line item background.

## [0.5.1](https://github.com/calops/bkpr/compare/v0.5.0...v0.5.1) (2026-11-06)

### Bug Fixes

* preserve layout text through native Shift encoding ([713d00b](https://github.com/calops/bkpr/commit/713d00b0c516f025a3e3741e610a6cbdc34c9f32))

## [0.5.0](https://github.com/calops/bkpr/compare/v0.4.2...v0.5.0) (2026-11-06)

### ⚠ BREAKING CHANGES

* **client:** Spinner::new now requires a static frame slice. Callers choose spinner glyphs instead of relying on framework-owned frames.
* **client:** use `bkpr_client::run_sessions` with `SessionTarget` instead of separate local/remote TUI entrypoints. TUI `-s` accepts `host:name`; use `local:` to keep literal local colons. Utility target names stay literal.
* **client:** terminal scene surfaces now include a per-client palette override; surface constructors must set the palette field.
* **control:** MCP control now accepts {operation,arguments,scope?} instead of recovery-only {request:...}. Explicit input.* operations use the separate terminal_input tool.
* **client:** Replace `--ssh` with `-r`/`--remote` and `--helper` with `--remote-bkpr-bin`, including headless remote commands. No aliases or host:session grammar are retained.
* **tui:** Physical-input consumers must handle `ClientEvent::Mouse` and `InputPart::Mouse` with explicit SGR button/action metadata.
* **core:** `PaneSpawn` now accepts `panes` and `layout` and returns `created_panes`. Native wire family 6 requires matching client/server binaries; existing incompatible sessions are not restarted.
* **protocol:** Wire family 5 requires matching client/server binaries.
Existing version-4 sessions remain usable with their old clients; new
clients refuse them without restarting daemons or replaying requests.
* **tui:** ProjectSnapshot adds the public color_slot field. UiSurface::apply_theme_with_backgrounds is replaced by apply_theme_with_colors, accepting explicit foreground and background spans; all workspace callers are migrated.
* the Rust Cell.text field is CompactString rather than String; JSON consumers are unchanged.
* **tui:** SurfaceContent adds CellsClipped. Exhaustive scene-content matches must handle clipped cell surfaces.
* **context:** `bkpr context` now returns one live pane context, not the whole registry. Use the versioned `inspect` request for registry snapshots.
* **recovery:** the registry now writes version 3. Older daemons can't read updated records.
* **ssh:** control protocol 4 requires launch-local agent source publication during client activation.
* Protocol 3 replaces the single workspace layout with owned windows. Existing daemons need a fresh matching runtime; schema-1 metadata upgrades only when ownership is unambiguous.
* replace the combined prototype with bkpr/bkprd and version-2 control; remove the old daemon and request interfaces.
* legacy commands, plugin integration, Zellij runtime and old package outputs are removed from this replacement branch. The native executable currently exposes daemon, TUI, request and keyboard operations; remaining product services are developed against the new pane-first model.
* **agy:** Bridge readiness uses notification_transport instead of future_turn_transport. AGY peer notifications can now steer active turns.
* **agy:** Relay protocol 4 requires an explicit broker restart and managed harness relaunches. Existing brokers are never replaced automatically.

### Features

* add keyboard and mouse pane resizing ([ad02b86](https://github.com/calops/bkpr/commit/ad02b86b7e734e7db76bea8189d3a0738eb232f6))
* add pane-first workspaces and a native TUI client ([aca5ca5](https://github.com/calops/bkpr/commit/aca5ca50346421811f0137b85a704125be15eff5))
* add per-view native terminal search ([2295a5f](https://github.com/calops/bkpr/commit/2295a5fad277f4e628c9b27a47942b2ce33264c3))
* add per-view terminal selection and copy ([5f57cad](https://github.com/calops/bkpr/commit/5f57cade38a919410e47f93c87e55ea04a6a541f))
* add per-view TUI scrollback navigation ([b70076e](https://github.com/calops/bkpr/commit/b70076ec79591f2bf4d32c958ad490766897ae74))
* add persistent installs for Claude, omp and AGY ([1e2ba2c](https://github.com/calops/bkpr/commit/1e2ba2c417586384bb2706ba574be7918e54c8f4))
* **agy:** add relay integration and MCP installation ([a651a89](https://github.com/calops/bkpr/commit/a651a89723b90cf95a0bf2bfd0264e35bb2807fa))
* **agy:** prefer active-turn peer notifications ([6095b4f](https://github.com/calops/bkpr/commit/6095b4fdf05fa8d1eaf2e17e9f20962c1cda25b3))
* **client:** add independent multi-session attachments ([329ca83](https://github.com/calops/bkpr/commit/329ca837cb045d79b445fe5a441cba7c3943d2c7))
* **client:** add live theme selection and resume typing from scrollback ([62be190](https://github.com/calops/bkpr/commit/62be190e2467f49154eefc84da6061e372cdaad8))
* **client:** attach the TUI directly to remote sessions ([476e6eb](https://github.com/calops/bkpr/commit/476e6eb3b67cf9e7ffcd4567a648099d71ad0306))
* **context:** bind native exec snapshots and live pane identity ([43798ec](https://github.com/calops/bkpr/commit/43798ec4587de3be34d82195c71e6a6a46c8209d))
* **control:** expose discoverable CLI and MCP operations ([b41d187](https://github.com/calops/bkpr/commit/b41d1873921f1eb116df2eec2f5b0654c54b80cb))
* make TUI startup and pane reopening usable ([a387cde](https://github.com/calops/bkpr/commit/a387cde56430665b52d8734fd71f4fd2afce08ff))
* **recovery:** resume native harness conversations with fresh authority ([20d9574](https://github.com/calops/bkpr/commit/20d957461da0327f9ac5e930209d32b3f704aafb))
* **relay:** steer Omp and Claude at native turn boundaries ([cd242e7](https://github.com/calops/bkpr/commit/cd242e769cb644fa71bad2ed4407720ba2e91e4b))
* **remote:** connect to existing sessions over SSH ([2785501](https://github.com/calops/bkpr/commit/2785501c27fe96f0b6ba5e11fea807bc8170de9d))
* restore local project metadata ([12b1013](https://github.com/calops/bkpr/commit/12b1013c355276bb014016311ab1f9f541c739ef))
* restore native agent communication ([96370f3](https://github.com/calops/bkpr/commit/96370f367c59accd3a4a297784b8f6aaefc08cea))
* restore native harness capture ([1f09f5a](https://github.com/calops/bkpr/commit/1f09f5ab941d37b3f26e8b36dd6df66d2de474b0))
* restore native session and workspace workflows ([c5f9623](https://github.com/calops/bkpr/commit/c5f9623d13ccf94e1b8f6d0223187a9ce56b9c85))
* restore owned workspace windows and session closure ([0105d29](https://github.com/calops/bkpr/commit/0105d299ba91be58d2f32f4b09d08ac28bfd3709))
* reuse focused overlays for pane and workspace rename ([ff99c8c](https://github.com/calops/bkpr/commit/ff99c8c8957a5d972246ce56e9dfe3dababc6f9a))
* **server:** expose immutable daemon spawn defaults ([df77ada](https://github.com/calops/bkpr/commit/df77ada3946726ba55bf6f54f5ec5b651e626746))
* **session:** restore shells and active harnesses on cold startup ([92ee87d](https://github.com/calops/bkpr/commit/92ee87d5689178ba2f173dfea6c045219230abc3))
* **ssh:** forward agents through the owning daemon ([0b38e46](https://github.com/calops/bkpr/commit/0b38e468b06286bdff22fd1199eb6262a0e9a420))
* **tui:** restore contextual chrome and terminal-aware colors ([6cf9d0e](https://github.com/calops/bkpr/commit/6cf9d0e95e3b6ca691cb7db259a1f792adaae95e))
* **tui:** restore main-aligned finder cards and raised overlays ([101aa47](https://github.com/calops/bkpr/commit/101aa474c001bb83fcee4a12a36933338b9a4ace))
* **tui:** restore sidebar metadata and project colors ([5c0a8ad](https://github.com/calops/bkpr/commit/5c0a8adb4dc76632cb85446e48ca7034f0f348f2))
* **tui:** show transient notices as dismissible toasts ([c203ace](https://github.com/calops/bkpr/commit/c203aceada4aa8a1f56539289bf91ca2b04f0307))
* **ui:** add structure to naming overlays ([b49160f](https://github.com/calops/bkpr/commit/b49160f7e58909925c79bb13e531f1a8b342617c))
* **ui:** default new sessions to rounded full frames ([e723496](https://github.com/calops/bkpr/commit/e7234969b537300d4f1abc8919ceaf94d0530fd7))
* **ui:** join sidebar hover items with raised details cards ([c87100f](https://github.com/calops/bkpr/commit/c87100f066c48484a516e345b27a388608d8b876))
* **ui:** merge cached recessed pane frames ([f94c048](https://github.com/calops/bkpr/commit/f94c0487f72429ece383cd8c2f78d475a8e522c5))
* **ui:** render cached bee and hive markers ([a447e58](https://github.com/calops/bkpr/commit/a447e58aae19944b08cf27ad8f6491f602c86062))
* **ui:** render finder and forms as raised tinted cards ([63b97ee](https://github.com/calops/bkpr/commit/63b97ee0fc9e5fd79ac885ed3ae7757a3fdcc44c))
* **ui:** render hint cards as raised palette-aware surfaces ([8f7ee10](https://github.com/calops/bkpr/commit/8f7ee10e4e9e00fd3c42165231ddac32faf8d5bf))
* **ui:** toggle graphical pane borders with Alt Shift B ([8694841](https://github.com/calops/bkpr/commit/8694841559d697e8dc4e08f1029777f06b5cb3a1))

### Bug Fixes

* **build:** reuse the validated native engine in just recipes ([61b8a79](https://github.com/calops/bkpr/commit/61b8a79cb5733f007ee538924aa42cd3061d469a))
* **client:** refine session tree presentation ([42c3de6](https://github.com/calops/bkpr/commit/42c3de6d0dc5de30220a46329a5e2d94f1a28b18))
* close completed shell projections and retire input cleanly ([fa2d970](https://github.com/calops/bkpr/commit/fa2d9704fa2506582c0d0ac5ddd467a5d538f400))
* **core:** create panes with their final layout atomically ([b6c329c](https://github.com/calops/bkpr/commit/b6c329c7e0c3ec7a835bee199d36d77f2d7eed19))
* **core:** publish effective native pane titles ([643b217](https://github.com/calops/bkpr/commit/643b217050d8d9f0df4cbfdabf61784901f0a52a))
* **deps:** pin native bold and faint rendering fix ([766c161](https://github.com/calops/bkpr/commit/766c161fb01660da6c156c02e10429fe5a3f93b1))
* eliminate full-screen scene and rendering bottlenecks ([c572794](https://github.com/calops/bkpr/commit/c572794f33be35f0e1563e61e622aa4fbaa64e8c))
* preserve keyboard layout for modified keys ([a7a260a](https://github.com/calops/bkpr/commit/a7a260ae2846d82982a931ebc3e4f8f00b3a0bb0))
* **relay:** complete startup locking for idle shutdown ([bd418d6](https://github.com/calops/bkpr/commit/bd418d60d456d70621fd81ff56f1af39c4def91b))
* remove blocking scene polls and full-screen typing repaints ([bfd52ef](https://github.com/calops/bkpr/commit/bfd52ef74fa3030e6d3285ac6ed48bf9f8624ebc))
* **session:** exit cleanly on expected shutdown ([62fcf99](https://github.com/calops/bkpr/commit/62fcf99b8824a3233a47fd6dec7c95d57e3144a0))
* **tui:** preserve sidebar graphics and focus through navigation ([8b3145c](https://github.com/calops/bkpr/commit/8b3145ce7ce742d99205f5ec5e6d637baa63fa51))
* **ui:** balance form padding and accent finder borders ([65db355](https://github.com/calops/bkpr/commit/65db35590124e8450e2b5fbee01d6428fd4a5e0e))
* use a stable launch-local MCP name ([47cf655](https://github.com/calops/bkpr/commit/47cf65527c13818fe444c6fc17dd85daea7c0b87))

### Performance Improvements

* **engine:** avoid redundant cell grapheme queries ([e13b4e3](https://github.com/calops/bkpr/commit/e13b4e3007f13da0d42b4437518527b1fd44e1ed))
* keep short terminal cell text inline ([d4c4562](https://github.com/calops/bkpr/commit/d4c456216090978b03fcff614578fc4b922f262f))
* **protocol:** replace native JSON IPC with Planus ([22b8fdc](https://github.com/calops/bkpr/commit/22b8fdc3e7aab0070307b656449f6a9d9c0c31b5))
* **protocol:** reuse scene encoder run buffers ([7b124fa](https://github.com/calops/bkpr/commit/7b124fa5710eeb98133ff73857ae7476630198df))

### Code Refactoring

* replace Zellij companion with native pane runtime ([773bf77](https://github.com/calops/bkpr/commit/773bf775422350a505dccb619513bd6ac7fe7c91))

## Native implementation development notes

These notes describe the native replacement. Versioned release entries are
generated above this section by the release workflow. The former Zellij runtime
is not retained as a backend or an upgrade path for existing sessions.

### Packaging and releases

* Restore sandboxed native Nix packages and app outputs for x86-64 Linux, ARM64 Linux and ARM64 macOS. Ship matching `bkpr` and `bkprd` executables together and cache the pinned Ghostty engine separately.
* Restore the three-platform Cachix build, immutable Conventional Commit version tags and manual release preview. Branch dispatch validates the same packages without promoting `unstable` or publishing notes; tagged publication remains gated on every platform build.
* Keep automatic versions in `0.x`, including the breaking native replacement. Earlier releases and their sessions belong to the former implementation; running native daemons also retain their existing code until explicitly restarted.
* Return distribution to Nix-only; remove Linux tarball/Debian generation, asset publication and existing-tag backfill plumbing. Leave previously published release assets unchanged.
* Move native packages into self-contained nixpkgs-style folders with released source/Cargo pins and ordinary `callPackage` arguments. Keep checkout overrides in the flake and select `bkpr` as the package's main program.
* Keep build/cache and tagged release notes publication in one workflow graph, gated on all three platform builds and packaged executable loader checks. Keep manual version/tag planning separate.

### Fixes

* Settle held keyboard input when the physical terminal loses focus, including inactive sessions, instead of waiting forever for a compositor-swallowed key-up. Keep releases on their original pane/view behind accepted paste/input, retain blocked cleanup ownership, fence retired generations and consume late hardware releases without duplicate delivery.
* Reuse the validated native engine in `just build`, `just run` and `just test`. Re-extract, patch and prepare it only when missing or invalid instead of refreshing native inputs on every invocation.
* Route Omp tools through the shared launch-local MCP server and include communication guidance as a private package skill. Remove Omp-specific tool registration/schema conversion; keep the extension for native conversation binding and step-boundary notifications.
* Reap explicitly terminated pane processes before stopping their PTY writer. `portable-pty` emits newline/VEOF on writer drop; those bytes must not reach a still-live application or pending shell command.
* Use the stable launch-local MCP name `bkpr` for Claude, Codex and Omp instead of a launch-ID suffix. Keep authentication launch-scoped and persistent configuration untouched.
* Preserve the active keyboard layout when decoding Kitty control keys instead of substituting the alternate PC-101 key. Fix AZERTY Ctrl-W becoming Ctrl-Z while ordinary text remains correct.
* Mark Shift consumed when it produces layout-dependent printable text before native keyboard encoding. Preserve raw modifiers for enhanced shortcut/event reporting, reset consumption between events, and keep Ctrl/layout identity and navigation semantics intact. Fix AZERTY punctuation reaching Neovim as the unshifted character without application- or character-specific mappings.
* Explicitly shut down the remote disconnect fixture's socket instead of relying on its last descriptor closing. Keep a duplicate descriptor open to cover concurrent fork inheritance without sleeps or weaker lifecycle/authority assertions.
* Create one or many panes together with their final window layout through request-local references. Remove the TUI's spawn-then-layout workaround; validate the complete batch before execution, reserve capacity/ownership, publish once after durability, and reap unpublished children after startup or commit failure.
* Publish current OSC 0/2 pane titles consistently through inspection, context and registry subscriptions, including hidden panes. Keep explicit names pinned, preserve naming intent across restart, and never persist transient terminal titles or infer harness status from them.

### Performance

* Keep short terminal-cell graphemes inline with `compact_str`, avoiding their individual text allocations during extraction, cloning and client scene expansion. Preserve arbitrary Unicode through heap fallback. Matched release benchmarks and remaining CPU gaps are recorded in the engine report; no frame cap, extra delay or slower recognition polling is introduced.
* Cover final combining and long Unicode grapheme output after a partial-buffer native PTY exit with an actual lifecycle regression.
* Replace native JSON request/response, pane-scene and client-lifetime streams with typed Planus binary messages. Keep scene reception zero-copy through shared byte slices; validate complete updates before expanding cells/resources or changing the displayed baseline. CLI/MCP/provider and persistence JSON remain separate boundaries.
* Wire family 5 requires a matching client/server pair. Reject incompatible families, required capabilities and malformed unions before dispatch; preserve existing sessions, typed shutdown outcomes, cancellation and non-replayable mutations. Do not automatically restart a live version-4 daemon.
* Reduce native snapshot extraction work through the existing SDK's raw-codepoint and reusable UTF-8 APIs instead of repeatedly querying and converting grapheme codepoints for every cell. Preserve Unicode clusters, native widths, image placeholders and snapshot cadence; matched client-plus-daemon release CPU results are recorded in the engine report.
* Reuse response-local text, layout and row-offset buffers when encoding Planus scene runs instead of allocating and dropping buffers for every style/link span. Preserve empty-cell, Unicode, delta and resource semantics; matched replay CPU falls without adding a frame cap, delay or dependency.

### Discoverable control

* Share a typed operation catalog between `bkpr api help/schema/call` and the launch-local MCP. Generate argument and result schemas with maintained Schemars types; expose existing workspace/window/pane lifecycle, atomic batch placement, layouts, recovery and complete visible-frame text reads without a second dispatcher or spawn subsystem.
* Expose text, bracketed paste, native key events and raw bytes through a separate `terminal_input` MCP tool. Preserve exact input, explicit submission, private temporary views, client takeover and launch End/process-exit revocation at native writer admission. Keep successful input receipts after cleanup and failed/unknown outcomes nonreplayable.
* Keep process observation off registry writer locks, fence scoped targets before execution and publication, and validate each batch child before starting it. Do not export another workspace's registry metadata or borrow a human client's size, focus, view or clipboard authority.
* **Breaking:** replace recovery-only MCP `control {request: ...}` with the discoverable `{operation, arguments, scope?}` envelope. Native wire family 6, remote control 1 and durable schema 4 remain unchanged; older live daemons may reject new operations and are never restarted automatically.

### Development checks

* Keep the authenticated native-capture fixture stable with a private blocking pipe instead of repeatedly forking `sleep` during process inspection. Wait for the nested fixture to finish before binding, and assert recovery snapshots outside the registry mutex so a failure cannot poison cleanup. Production foreground validation and automatic-resume policy are unchanged.

### Breaking branch replacement

* Replace the previous runtime with seven focused crates: `bkpr`, `bkpr-client`, `bkpr-tui-framework`, `bkpr-server` (the `bkprd` executable), `bkpr-core`, `bkpr-protocol` and `bkpr-terminal-engine`. There is no legacy launcher, plugin, binary alias or compatibility track.
* Advance native wire family 5 → 6 for batch `pane_spawn` and explicit placement. Return `created_panes` reference-to-ID mappings; require a matching client/server pair and leave existing incompatible sessions running without automatic restart. Durable schema 4 is unchanged.
* Remove the old host-runtime flake input, WASI build/export paths and sibling-runtime recipes. Native packages now use a reproducible, offline Ghostty dependency closure; the default devshell keeps the incremental Cargo development path.
* Expose the native daemon, TUI, typed version-6 protocol and keyboard client. Backend-owned workspaces contain independent windows/layouts and owned panes; layout-only removal or client detach does not terminate a process. Layouts, directories and authenticated harness recovery intent persist, but running processes and terminal contents do not. Forge/CI services remain unavailable.
* Native acceptance and remaining limitations are recorded in the engine report. Publication does not replace installed applications or restart live sessions.

### Remote sessions

* Attach multiple local and existing remote sessions in one TUI with repeated `-s`. Use `host:name` for SSH, `local:name` to escape local colons, and `-r HOST` as the invocation-wide default host. Utilities remain single-target with literal names.
* Keep one independent no-PTY OpenSSH/Yamux connection and remote helper per remote attachment. Preserve native process-bound authority, pinned identities, bounded admission, exact binary scenes/input/clipboard and unknown mutation outcomes without replay.
* Present one owner-qualified session/workspace/window/pane tree with a blank line between sessions and continuous multi-window connectors. Keep passive navigation and popovers metadata-only; route explicit actions through their original owner and generation. Use the beekeeper session icon, neutral two-cell `` window icons, ancestor-only bright guides and a full-width highlight covering both focused-pane rows.
* Keep remote session names on the left and hosts right-aligned. Mute healthy host labels without an up badge; replace their network icon with a colored Nerd Font spinner or network-off error icon when needed.
* Prepare local targets off the UI thread after terminal setup, once per invocation. Discover authoritative defaults before one-shot activation/takeover; remote-only startup needs no local HOME, CWD, daemon or session state.
* Keep initial connection failures visibly offline for manual retry. Reconnect previously active attachments after transport loss without replaying input, bootstrap or takeover. Remove intentionally detached/ended sessions and exit when the last target ends; reject replacement logical identities instead of adopting them.
* Share physical theme, rendering preference, sizing, device routing and transient toasts across attachments. Keep native resource IDs and held-button releases qualified by their originating attachment.
* Preserve native retirement received before activation as a typed outcome, not a reconnect failure. Isolate transport cleanup under a child cancellation token so a failed SSH attempt cannot cancel its coordinator generation and hide its error.
* Keep headless identity inspection, persistent JSON-line control and one-shot managed requests. EOF detaches only the client; explicit takeover reports its reason and unexpected loss remains a failure.
* **Breaking:** replace the separate local/remote TUI entrypoints with `bkpr_client::run_sessions` and `SessionTarget`. Named TUI targets now interpret host prefixes; use `local:` for literal local colons. Native family 6, remote control 1 and durable schema 4 are unchanged.
* **Breaking:** `bkpr_tui_framework::Spinner::new` now takes a static frame slice. The framework owns scheduling; clients choose animation glyphs.

### Daemon spawn defaults

* Publish immutable command, cwd, home and isolation defaults beside `SessionInfo`, preserving native path bytes without exporting environment variables or launch credentials.
* Capture current daemon startup configuration once, including headless daemon `SHELL`. Keep missing defaults explicitly unavailable; never infer them from saved or running pane commands, and never execute a program on a headless metadata query.
* Permit authenticated identity/defaults reads before client activation, including after persistence failure. Preserve token, native peer and lifetime checks, mutable-operation gates and takeover ordering without new per-key checks, wire-family changes or durable records.

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
* Distinguish typed `session_closing` admission rejection from requests whose dispatch began. Preserve successful responses, durable failures and unknown mutation outcomes; never infer safety from error text or global closing state. Drain decoded startup-cancelled replies and keep immutable identity available so targeted stop still works after persistence failure.
* Exit TUIs cleanly on verified shutdown of observational requests without treating bare EOF as success. Let accepted mutations drain, retain typed errors through snapshot/input workers and attachment rollback, and handle shutdown arriving during final client cleanup. Exercise multiple attached clients, all final-session closure paths and exact-once accepted mutations with deliberately lost replies.
* Replace the single-layout API with protocol 3 and durable schema 2. Upgrade unambiguous schema-1 metadata once without command replay; reject ambiguous foreign layouts without replacing metadata. Cross-session/SSH workspace routing remains separate work, with normal same-user use through its owner rather than read-only references.

### Bkpr frontend restoration

* Group related keybinding hints through explicit action membership while retaining each concrete action and alias. Share modifier-aware chord formatting, Unicode measurements and whole-chord wrapping through the reusable framework; keep descriptions, priority and availability in Bkpr. Reserve notice rows before layout so narrow popups never cut a group in half.
* Restore the all-workspace sidebar's independent keyboard selection, folding, scrolling, rename/create/close-terminal actions and passive metadata popovers.
* Restore focused name/directory forms, grapheme-boundary editing and paste, identity-bound modal intent, honest asynchronous errors and the metadata-only fuzzy finder with filters and responsive previews.
* Share typed binding dispatch with contextual hints that always appear in passive popups. Preserve native pointer gestures and consume UI keys; normalize raw legacy C0 aliases without dropping their native control bytes.
* Size guide cards to the visible outline with a shared content-sized key/description gutter, centering the whole table rather than the gutter. Wrap aliases and descriptions atomically, preserve lifecycle explanations and apply terminal-derived mode tints.
* Put retained UI surfaces, text editing, window decoration, capability gating and bounded off-thread SVG preparation in the reusable TUI framework. Keep Bkpr-specific models/assets in the client. Restore static bee/hive artwork through the existing scene/image lifecycle with terminal-default colors and readable unsupported-host fallbacks.
* Reuse foreground-identity and project freshness presentation across metadata views. Preserve native clipboard framing for applications while local forms/finder receive ordinary paste, restoring the host's original mode on detach.
* Derive a shared framework theme from confirmed terminal color capabilities and observed foreground/background/ANSI colors. Apply readable surfaces, selection, muted text and semantic accents throughout Bkpr chrome; retain native/ANSI fallbacks and canonical application styles. Refresh on actual private theme notifications without polling, preserve retained field/finder state and tint artwork from its final displayed ink.

### Generic overlays and static chrome

* Put placement, ordered passive/modal layers, opaque return-focus tokens and whole-gesture capture in the reusable framework. Restore main's outside-press cancellation without click-through; retain drafts through outside wheel/resize and keep pending or ambiguous operations noncancellable.
* Project focused-overlay background faint per view, preserving canonical glyphs, colors, bold styles and images. Passive cards and the key guide retain application keyboard ownership.
* Add reusable bounded, deadline-driven toast cards with retained Unicode-safe text, dismiss buttons and complete mouse-gesture ownership. Route transient TUI notices through top-center five-second cards, sharing popup shadows/bevels and rounded text fallbacks without taking keyboard focus. Keep pending operations and decision prompts in their contextual UI.
* Preserve explicit SGR mouse press/release identity alongside maintained Termwiz coordinate/modifier decoding, preventing chorded toast clicks from leaking releases or leaving capture stuck.
* Move Bkpr's palette recipe into client-owned appearance policy. Preserve main's subdued colors instead of inventing contrast-driven replacements; the framework caches supplied styles and accepts explicit graphic materials.
* Add a reusable fuzzy-searchable list with grapheme-safe query editing, keyboard paging, immediate hover selection and clipped content-local hit testing. Use it for the single-card `Ctrl-b t` theme overlay, with live whole-UI preview, commit/cancel rollback and no separate preview pane. Leave its background undimmed for accurate colors without changing other overlays' faint treatment.
* Add client-local Catppuccin, Solarized, Tokyo Night and PaperColor Light/Dark palettes without config persistence. Preview and restore terminal-pane default foreground/background and ANSI16 alongside Bkpr chrome through borrowed surface palette overrides. Preserve application RGB, higher indexed colors and canonical pane frames; keep inferred terminal colors as the default and follow actual terminal color notifications without displacing explicit presets.
* Restore main-aligned unboxed 38-cell sidebar geometry, compact connected project/pane rows and sole-window elision without changing backend identities.
* Reuse the fork's rounded-mask convolution for compact recessed panes and complete raised surfaces with exterior shadows and directional bevels. Restore the light-theme upper/left facet and preserve rounded text fallbacks.
* Restore main's three-card finder, terminal-derived lifted/tinted plates, responsive 40/60 results/preview geometry, one-row field-aware fuzzy results and grouped authoritative metadata instead of debug identity dumps.
* Add borrowed source-local cell clipping and retained background-only spans to the framework, preserving wide glyph boundaries, cursor/hit ownership, selection and semantic inks without cloning full frames.
* Give resolved themes process-unique style revisions so switching an unchanged surface between independent normal and raised palettes invalidates its retained styling correctly.
* Compose sparse transparent graphics layers without erasing native cells, stealing cursor/input ownership or letting pane-local image depth escape its scene layer.
* Share one separator cell between adjacent pane frames in both split orientations. Remove redundant divider glyphs and omit rich-ready text perimeters before retained painting; restore complete plain captions after resize or capability loss.
* Reuse prerendered straight-edge atlas resources through representable viewport and overlay clipping instead of allocating, encoding and uploading derived edge rasters. Keep the existing bounded worker cache and exact phase-preserving fallback for arbitrary native-image scaling.
* Separate physical host background ownership from logical terminal colors. Apply the sidebar matte only with ready pane materials, preserve native pane defaults, filter owned/late query echoes, adopt real theme changes and restore exact original colors on fallback or exit.
* Fill ready graphical frame cells with the original pane background, matching main's cleared-frame underlay. Preserve the atlas's exterior matte while correcting the opened top/bottom gutters without new textures, raster work or viewport changes.
* Restore semantic sidebar hover, pointer-to-popup handoff and scrollable structured metadata; balance host no-button motion tracking on detach. Preserve application mouse-reporting/gesture ownership and use the Codex Nerd Font glyph with its required spacer.
* Join the sidebar anchor and metadata card into one raised contour with convex/concave corners and complete upper/left bevels and shadows. Keep style choices in the client, bounded preparation and pixel geometry in the framework, and rounded text fallbacks in the same scene path.
* Align passive popup content with its sidebar item rather than one row below it. Share main's lifted background across the item, card and borders; omit covered tree connectors and restore them on close. Keep inverse keyboard selection on limited-color clients instead of weakening generic selection precedence.
* Close the remaining sidebar/card seam by projecting every fully opaque joined cell, including the logical framing column at the bridge. Blank graphical frame glyphs through retained popup state and restore complete rounded text borders when graphics become unavailable.
* Publish ephemeral canonical-repository color slots from the backend, sharing linked worktrees and retaining reservations through metadata uncertainty. Apply main's finite terminal-derived project accents to names and markers across sidebar, finder and popups; preserve neutral branches/tree guides and semantic status colors.
* Replace the framework's background-only retained theme method with `apply_theme_with_colors` and generic foreground/background spans. Resolve visible ink correctly through inverse styling without flattening selected backgrounds or wide/combining glyphs.
* Reuse warm joined materials immediately and retain source-valid inactive image projections in the existing renderer, with explicit byte/count bounds and prompt source retirement. Actual dark/light terminal checks reduce fully warmed popup-switch image payloads from 1,731,072 bytes to zero without reducing visible image support.
* Preserve still-valid SVG icons and pane/popup contours while color-set changes prepare missing variants. Harvest matching undrained publications without retaining stale colors, dimensions or styles; keep all image retirement/placement changes inside the synchronized frame.
* Track hidden cursor position independently of visibility. Anchor sidebar focus to its highlighted rendered item, preserve that logical position under passive plates, and restore native/modal cursors without showing hidden cursors or emitting idle output.
* Project shared sidebar artwork above passive joined borders and below modal content, preserving the hive's half-cell gutter ink without cropping, wider margins or cloned cell canvases.
* Add `Ctrl-b g` to reversibly switch fancy/text chrome, including inside focused finders/forms, without losing query/cursor/draft state, disabling native application images, resizing panes or discarding bounded prerender caches.
* Keep provider-dependent forge/PR/CI overlay content unavailable until its backend contract exists. No provider calls, decorative animation or command palette are introduced.


### Development

* Use the pane-first worktree's own incremental Cargo target and native prerequisite directory instead of sharing main's cache. Preparation helpers default to that local target; builds remain limited to eight jobs. Research maintained, widely used dependencies before sizeable custom implementation.

### Native workspace implementation

* Bare `bkpr` connects to or starts `bkprd` and opens the TUI. One retained framework composes the sidebar, terminal projections, forms and choosers, including physical pointer hit testing and per-pane graphics namespaces.
* Use a pinned, source-auditable `libghostty-rs` SDK snapshot for generic terminal ownership and encoders. Local SDK patches preserve the tested native revision, clipboard-offer provenance and a pre-allocation decoded-image budget; exact build provenance remains verified.
* Separate size policy, input and clipboard authority. Register sizing-policy changes without waiting for blocked native work; bound pane/view/control admission and preserve FIFO ordering for accepted input.
* Dispatch attachment/management work asynchronously and isolate native input per pane, with reserved device-response admission and explicit unknown-outcome handling. Keep complete pointer gestures bound to their original pane/view through cross-pane dragging, backpressure and projection retirement.
* On cold named-session startup with current bootstrap argv, restore saved owned terminals in their original workspaces/windows, nested layouts and hidden membership. Start current shells in verified saved directories or resume verified active native conversations through fixed current-configuration adapters with fresh authority; never replay arbitrary old commands. Direct `bkprd` without argv stays headless, and live attachment never repeats restoration.
* Add uppercase `Ctrl-b R` to confirm a fresh configured shell in an unavailable pane's selected-workspace position. Preserve the old record and other workspaces, use revision-guarded atomic replacement, keep failure/unknown outcomes honest and block read-only clients. Retained directories never silently fall back; confirmation offers an explicit current-directory choice. `Ctrl-b n` remains a new split. This does not restore processes, harness conversations or durable terminal history.
* Paint bounded changed-cell spans rather than whole composed rows, avoiding redundant static sidebar output while preserving wide/combining text, styles, links, erasure, graphics and cursor behavior.
* Track sustained-animation CPU efficiency with matched release benchmarks. Inline cell storage reduces measured allocation cost, but the large Zellij gap remains open; no frame throttling or fidelity reduction is introduced to conceal it.

### Native scrollback navigation

* Add attachment-local TUI history navigation: `Shift+PageUp`, `Shift+PageDown` and `Ctrl-b s`; arrows move rows, PageUp/PageDown move overlapping visible pages, Home reaches oldest retained rows, and Esc/Enter/End return live. Keep existing workspace shortcuts unchanged, show history offset and a live-return hint, hide the application cursor, and block unintended typing/paste while inspecting.
* Route vertical wheel input using exact native mouse-reporting and primary/alternate-screen facts. Shift+wheel explicitly navigates history; ordinary wheel retains application ownership when reporting is enabled, the alternate screen is active, or the daemon has not advertised exact-mode support. Alternate-screen history reports unavailable instead of exposing the primary buffer.
* Anchor each view in bounded native in-memory history while output continues, without copying full history per key or changing another client's view. Preserve accepted key/pointer releases, paste FIFO and clipboard grants; allow read-only inspection. Projection retirement, workspace switch or detach starts a later view live; no selection/copy/search or persistent terminal-history recovery is added.

### Native selection and copy

* Add per-view drag, double-click word, triple-click line and Alt-rectangle selection using Ghostty's maintained gesture and formatter APIs. Track history endpoints across output/resize and keep edge autoscroll, highlights and pointer capture local to the originating view.
* Copy completed selections on release and with `Ctrl-b y` through existing terminal clipboard framing. Permit read-only inspection/copy, respect client deny and host permissions, reject oversized copies rather than truncating, and discard stale exports after focus/selection/view retirement.
* Publish selection-only changes as bounded view annotations without cloning/resending canonical scenes. Separate visibility epochs from sizing activity so focus/lease updates do not reject queued selection. Report known selection rejection without stopping application input; ambiguous transport remains non-replayable.

### Native tiled-pane resizing

* Add `Ctrl-b e` resize mode: arrows move a containing divider by one cell, Shift moves five, held arrows repeat, and Enter/Esc finishes without application input. Frame dragging uses main/fork quadrant proximity and frozen grab offsets to move both axes in one revision-guarded layout update.
* Share partition arithmetic across pane projection, divider drawing and hit testing. Preserve unrelated ratios/topology, clamp useful subtree sizes and keep small-window projection separate from saved layout intent.
* Reuse the existing revisioned backend layout API and bounded background pool. Retain only the latest pending pointer position behind one in-flight commit, preserve final release, block read-only mutation and retire foreign/focus/modal/geometry changes without replaying ambiguous updates.
* Make the sidebar's right edge draggable with client-local width, terminal-resize clamping and whole-gesture cancellation. Keep read-only viewport sizing independent of backend layout mutation. Apply one extra right-edge cell to every keybind popup through shared guide layout, without moving its content or overflowing the screen.
* Highlight affected resize edges in both rich and rounded text rendering. Keep cyan/background policy in the client; let framework clipped cell layers place cached chrome beneath edge glyphs without discarding source resources or shadows.
* Resolve pending legacy keys before dispatching complete framed terminal reports, preventing structured key releases from overtaking an earlier Escape.

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

* Persist authenticated native kind/ID/raw-directory descriptors, verified terminal directories and separate active-resume provenance in private registry version 4. Older records retain explicit descriptors without guessing prior native activity. Tokens, launch/inbox authority, old argv, terminal contents and native settings remain absent.
* Add read-only headless `bkpr resume`, exact `--pane` selection and allowlisted shared-MCP `control` listing/resume. Default agent control to live execution-workspace scope; explicit session/all remain daemon-local, with no repository scope or arbitrary command execution.
* Resume through fixed native adapters with current configuration, fresh authority and explicit pending-binding results. Preserve original intent until the selected native conversation binds; reject stale/duplicate/destructive races and never replay saved argv, select a provider or silently fall back to a fresh session.
* Preserve stopped layout/intent after missing-directory or native-resume failure and continue independent restoration; no directory fallback or fresh-conversation substitution. Returning to a shell clears automatic native intent without deleting its explicit historical descriptor. Checkpoint changed observations in the existing off-path sampler and refresh them before graceful teardown.
* Serve authenticated native startup callbacks while gating ordinary and shared-MCP Control mutations until the restore batch finishes. Keep explicit shutdown available; cancellation, closed readiness/error output and early native exit cannot strand listener admission or owned startup processes.
* Recovery does not resurrect processes or terminal contents. A frontend picker and the existing genuine Claude/Antigravity native acceptance gates remain open.

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
