# Maintenance notes

## Boundaries

- `crates/shephrd` owns the native launcher, Git/CI, SSH forwarding and typed state contract. `crates/shephrd-zellij` owns WASI sidebar, finder and passive overlays. Zellij owns PTYs, input, native panes/tabs and session persistence; never mirror terminal output through the plugin.
- Keep workspace-first grouping and stock Zellij controls. Application input starts Locked; Ctrl+Space focuses sidebar controls. Passive details and shortcut cards never take keyboard focus or move the application cursor. Finder alone is a focused modal. Native splits/tabs must remain discoverable; close terminates, not hides.
- Recognized harness status comes only from OSC 7501. Process/CWD recognition and OSC 0/2 titles are separate. Generic programs do not get harness-status badges; Git/CI remains available to them. Do not infer semantic state from titles, progress, output or process presence.
- Do not change a user's harness profiles, persistent third-party settings, credentials or GitHub account. Claude launch-local hooks supplement titles only; Antigravity is recognition-only.
- Prefer Shephrd-owned APIs and push events. Add a Zellij patch only when its plugin/launcher API cannot provide the behavior; keep patches minimal.

## Git, provider and agent data

- Associate repositories with foreground CWD and the canonical common Git directory; linked worktrees have independent HEADs. Committed whole-repository totals compare HEAD to a merge-base. A PR target commit overrides guessed local bases; a missing target does not fall back to main. Never fetch for UI.
- Git watches invalidate cached state and trigger new-commit/push refresh. Cold forge data loads eagerly and is shared/batched. Only published, non-green checks use the 60-second refresh loop; complete green/skipped/no-check and unpublished results idle. Respect shared spacing, provider limits and backoff.
- Persist per-canonical-CWD metadata in XDG state. Validate repository, branch, HEAD and refs on restore. Linear associations come from canonical links in rendered PR comments, not personal mappings or speculative parsing. Popups/finder share typed metadata and link only verified destinations.
- SSH forwarding is launcher-owned: the latest live managed attachment selects the upstream agent, unset/no client fails closed. Keep the stable pane socket across helper restarts, preserve raw Unix path bytes, serialize startup/cleanup and guard in-flight requests by generation.

## WASI and UI invariants

- Render complete frames: Zellij clears the plugin grid each render. WASM stdout also transports host commands; emit commands before frame text and flush every render. Acknowledge every CLI-pipe input, including bootstrap/EOF. Cache cursor commands and avoid redundant redraws.
- All plugin roles request the shared `PERMISSIONS` set; permission grants are cached per WASM path and role requests replace them. Sidebar startup applies creation-scoped snapshot/guide/view only after grant; live `STATE` remains authoritative. Keymap publications must be immutable, session/client-scoped and byte-validated.
- `mode_info_format="json"` plus `ModeUpdate`/`InitialKeybinds` preserves native key identities. Cache the keymap separately from scalar mode updates. Strip the host flag before strict application-config validation.
- Sidebar rendered columns exclude its resize gutter; outer pane geometry does not. Preserve outer width when creating tabs. The details card may cover exactly one separator cell, never painted item cells; the separator remains mouse-resizable.
- Popup helpers are client/launch scoped. Ignore foreign/stale frames. Present a cleared or exact-size frame before acknowledging READY, accepting pointer actions or showing content; native show resets suppressed coordinates, so place after show. Genuine helper faults belong in the sidebar, not a transient red card.
- Sidebar hover and popup hover cannot both own the physical pointer. Keyboard navigation invalidates an earlier mouse highlight; do not let stale native pointer coordinates pin a focus-only popup. Probe the helper at handoffs without polling, and let verified popup hover retain details after sidebar leave.
- Locked shortcut hints stay inline. Wider cards are used only when keys/descriptions would wrap at sidebar width; a widened card sits at the far-right edge in sidebar context. Plan content height and geometry before display, not when READY arrives. Finder coordinates cover the whole display.

## Development

Use `nix develop` and incremental debug Cargo artifacts in `target/`. Run coordinated checks once after edits: `cargo test --locked --workspace`, `cargo build --locked -p shephrd-zellij --target wasm32-wasip1`, host/WASI `cargo clippy --locked ... -- -D warnings`. Use `nix build .#shephrd` only for packaging validation, not the edit/run loop.

### Patched Zellij runtime

The pinned host is Zellij 0.45.1. Stock Zellij lacks required OSC/plugin event contracts. `flake.nix` determines patch order; a local debug runtime lives in `target/zellij-dev`. For a fresh checkout, clone tag `v0.45.1`, apply the ordered `nix/zellij-*.patch` files **in flake order** with `patch --fuzz=0 --no-backup-if-mismatch -p1`, then build with `--no-default-features` and `$SHEPHRD_ZELLIJ_VENDOR` source replacement. Do not blindly reapply patches or stage generated `.orig` files. Existing native servers keep old code: start a fresh session after host changes.

```sh
cargo build --locked --manifest-path target/zellij-dev/Cargo.toml --no-default-features \
  --config 'source.crates-io.replace-with="nix-vendor"' \
  --config "source.nix-vendor.directory=\"$SHEPHRD_ZELLIJ_VENDOR/source-registry-0\""
```

Test focus, cursor, physical mouse, resize and hyperlinks in an actual isolated terminal, not just unit tests or injected escapes. Smoke scripts go under `target/smoke/`, using devshell Python+pyte and `sys.executable`; isolate HOME/XDG, inherited multiplexer and credential variables, and private native sessions. Never touch live sessions or call real model/provider services for a fixture. Clean up owned processes/files, retain useful evidence, and keep README user-facing.
