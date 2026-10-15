# Maintenance guide

## Project contracts

- `crates/shephrd` contains the native launcher/helper and shared typed snapshot/request contract. `crates/shephrd-zellij` contains the Rust/WASI plugin roles. Zellij owns PTYs, input/output, scrollback, native panes/tabs and detach persistence. Don't add a second PTY owner or replay terminal output through WASM.
- Keep workspace-first grouping and stock Zellij controls. Application input defaults to Locked mode; Ctrl+Space explicitly focuses sidebar controls. Passive hover/context popups must not steal keyboard focus; the explicitly opened finder owns input. Sidebar-created terminals use separate full-application-area tabs; native splits and tab changes remain visible in the model. Close means confirmed termination, not hide/unhide.
- Program status comes only from native OSC 7501. Process/CWD recognition and OSC 0/2 display titles are separate signals. Never infer lifecycle state from titles, progress, rendered text or process presence. Only recognized harnesses contribute status/attention to rows and workspace summaries; generic OSC app IDs don't create harness identities. Git/CI is independent.
- Don't change persistent third-party settings, credentials, account selection or harness profiles implicitly. Claude's launch-local hook supplements titles only. Antigravity is recognition-only. Upstream Pi isn't omp.
- Prefer pushed events and filesystem watches. CI alone has an approved batched, deduplicated, adaptive polling schedule with shared request limits and frozen complete results. Don't introduce recurring inspection/UI timers or provider calls per keystroke.
- Git identity follows actual foreground CWD and canonical common-directory identity, with independent worktree HEADs. Branch totals cover committed `HEAD` versus merge-base for the whole repository, not working-tree edits. Don't fetch to paint UI. Unknown, unavailable, incomplete and stale aren't zero or passing.
- Context popups and finder previews share typed content. Search uses collected metadata, not PTY scraping. PR/issue links require metadata-backed destinations, not guesses from arbitrary text.
- Prefer straightforward Shephrd-owned implementations. Add a Zellij patch only where supported plugin/launcher APIs cannot implement required behavior; keep host changes minimal.
- SSH forwarding is Shephrd-owned, not a native patch. Launcher-held control leases select the latest live managed attachment; unset/none fail closed. Preserve the stable pane socket across helper restart, raw Unix path bytes and generation-safe in-flight delivery. Keep bootstrap/final cleanup serialized by the session startup lock.

## Development workflow

Use `nix develop` and incremental debug Cargo builds. Keep `target/`; Nix release builds are for packaging validation, not normal iteration. The devshell supplies the toolchain, WASI target, Git/GitHub CLI and Python with `pyte`. It selects the debug WASM artifact and an existing local debug Zellij binary.

```sh
cargo build --locked -p shephrd
cargo build --locked -p shephrd-zellij --target wasm32-wasip1
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked -p shephrd-zellij --target wasm32-wasip1 -- -D warnings
```

Use `nix build .#shephrd` for the bundled package, or `.#shephrd-native` / `.#shephrd-zellij` for its components. Run checks once after coordinated edits, not against another worker's unfinished changes.

### Patched Zellij runtime

The runtime is pinned to Zellij 0.45.1. Stock Zellij lacks the required program-status and plugin-event contract. `flake.nix` is the source of truth for patch order. For a new local checkout, inside the devshell:

```sh
git clone --depth 1 --branch v0.45.1 https://github.com/zellij-org/zellij target/zellij-dev
(
  cd target/zellij-dev
  for patch_file in \
    zellij-cursor.patch \
    zellij-program-status.patch \
    zellij-program-status-events.patch \
    zellij-host-replies.patch \
    zellij-cli-session.patch \
    zellij-popup-clicks.patch \
    zellij-overlay-geometry.patch \
    zellij-plugin-messages.patch
  do
    patch --fuzz=0 --no-backup-if-mismatch -p1 < "../../nix/$patch_file" || exit 1
  done
)
cargo build --locked --manifest-path target/zellij-dev/Cargo.toml --no-default-features \
  --config 'source.crates-io.replace-with="nix-vendor"' \
  --config "source.nix-vendor.directory=\"$SHEPHRD_ZELLIJ_VENDOR/source-registry-0\""
export SHEPHRD_ZELLIJ="$PWD/target/zellij-dev/target/debug/zellij"
```

`--no-default-features` avoids rebuilding bundled plugin assets and the optional web stack. The patches optimize Wasmi dependencies in the development profile; workspace and guest code stay debug builds. Don't reapply patches blindly to an already patched checkout. Validate the full ordered chain against pristine pinned source with zero fuzz, and compare the patched files with the runtime used for testing. Never stage generated `.orig` backups.

Existing native servers keep old host code. Use a fresh session after rebuilding native patches; plugin reload doesn't replace geometry, input routing or protocol code. Subsequent devshell entries select the local debug runtime automatically.

## Non-obvious runtime constraints

- WASM stdout carries both encoded host commands and ANSI frames. Emit host commands before render text and explicitly flush every frame. Memoize cursor updates. Acknowledge every CLI-pipe input message, including bootstrap/EOF, or publishers can stall.
- Zellij resets the plugin grid for each render, so render complete frames rather than row diffs. Background-preserving empty rows use `EL0` from column one; `EL2` doesn't preserve the background in the pinned runtime.
- Permission caching is keyed by the WASM path, and role requests replace cached grants. All roles share the full `PERMISSIONS` constant; a smaller overlay request can revoke permissions needed on the next tab or attach.
- Native protobuf drops `KeybindPipe` identity. Sidebar and owner roles opt into `mode_info_format="json"` and subscribe to `ModeUpdate` and `InitialKeybinds`: `mode_info` carries scalar changes, while `keybinds_info` carries lossless keys on subscription/reconfiguration. Cache keys independently. Strip the host flag before strict app-config validation.
- New sidebars receive creation-scoped snapshot/guide/view projections. Apply them only after permission grant, strip them before owner identity/configuration handling, and keep live `STATE` as the authority for acknowledgement effects.
- Bootstrap keymaps reference immutable, session/client-scoped WASI `/cache` publications. Update them only on changed authoritative payload bytes; validate identity, bounds and full-payload integrity after permission grant.
- Sidebar outer geometry includes the right resize gutter; plugin paintable columns exclude it. Preserve outer width when creating another tab. Frameless separators must remain visible and mouse-resizable without focusing the sidebar. Don't disable global hover delivery to remove separator highlights.
- Passive context overlays must retain application focus/cursor ownership, and outside dismissal must preserve the application's original click. Host commands emitted during render and the matching frame have ordering requirements in the native plugin-message patch.
- The unlocked shortcut guide has its own passive overlay/controller; it must not replace persistent context details. Keep Locked hints inline, retain inline hints until the wider frame is ready, and restore suppressed overlay coordinates after showing it.
- Focused finder overlays use full-display coordinates, including reserved sidebar space. Passive-overlay input rules must not be broadened to make a focused modal fit.
- Floating `is_focused` survives hiding the floating layer; combine it with active-tab visibility and arm dismissal only after real focus. Restoring a suppressed modal resets its geometry: reapply coordinates on every open, but cache unchanged host commands while visible.
- Host terminal-query replies aren't application input. Preserve batch ownership and fragmented replies when changing startup, resize or keyboard routing.

## Runtime evidence

Use actual attached PTYs or an isolated GUI terminal for focus, cursor, mouse, keyboard negotiation, rendering and hyperlink behavior. Headless snapshots and injected escape bytes don't prove physical key encoding or click routing. Separate controlled protocol/provider fixtures from genuine harness turns and live provider queries in reports.

Keep smoke scripts under `target/smoke/` and short-path socket fixtures under `target/`. Use devshell Python and `sys.executable`, not hardcoded store paths. Isolate inherited multiplexer/credential environment, HOME/XDG directories and native sessions; preserve only the launch context a test intentionally exercises. Don't touch live sessions or persistent user configuration. Don't make real model/provider calls without user intent. Remove owned temporary processes and fixtures after the run; retain useful evidence without turning the README into a test diary.
