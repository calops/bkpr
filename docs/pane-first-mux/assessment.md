# Pane-first mux assessment

Date: 2026-10-29. Status: approved architectural direction; native implementation underway, with measured limitations recorded in the engine report.

## Decision and scope

Bkpr should own its pane-first product model, mux control plane and client frontends, while reusing maintained terminal-emulation and PTY infrastructure. Owning the mux does **not** mean writing another escape-sequence parser, terminal grid, Unicode engine or operating-system PTY layer.

The desired product is a backend that keeps panes alive independently of any display layout, with terminal, web and mobile clients presenting their own views. A layout may include panes from different hosts. Agent identity, execution membership and lifecycle remain backend facts, not consequences of whichever client currently displays a pane.

This branch replaces the old implementation outright. It does not keep a Zellij-backed launch path, old regression obligations or a permanent second backend. Existing installations and live sessions outside this branch are not modified. There is no live-PTY transfer promise.

`main` and the retained Zellij fork are starting material, not specifications. Reuse useful code, keep coherent modules where they fit, refactor or rewrite freely where a better design serves the agreed product. The [implementation plan](implementation-plan.md) records source locations, provenance, dependency decisions and current work. Superseded session/tab-owned remote-workspace proposals have been removed.

## Alternatives

| Approach | Benefit | Structural cost and fit |
| --- | --- | --- |
| Remain a companion to Zellij | Preserve a working runtime, native terminal behavior and existing integration investment. | Product authority remains distributed across native sessions/tabs, plugins and Bkpr. Independent clients and cross-host pane layouts require working around that ownership model. Good current implementation, not the desired end-state boundary. |
| Keep a full upstream-tracking Zellij fork | Reuse a complete terminal mux and continue shipping targeted native hooks. | Pane-first authority would touch central screen, layout, PTY and routing behavior. Tracking upstream does not eliminate the cost of preserving substantially different semantics. Appropriate for bounded additions, less attractive for replacing the product model. |
| Hard-fork an existing mux | Freedom to change invariants without repeated upstream reconciliation. | Bkpr inherits a whole application's terminal, UI, platform and release maintenance. Less merge pressure is not less ownership. It retains architecture that may be unnecessary for the desired backend. |
| Own the mux/control plane; embed terminal and PTY libraries | Place durable panes, endpoint identity, metadata, client projections and permissions at the intended boundary. | Bkpr must build lifecycle, transport, replication, persistence and rendering adapters. This is the approved direction, subject to the proof gates below; it is not the cheapest initial implementation. |

A maintained dependency is preferable for generic infrastructure when its actual public boundary fits. Reusing an entire application merely because it already contains the needed components is not equivalent to embedding a library.

## Why published Zellij crates are not an embedding SDK

Zellij publishes [server](https://docs.rs/zellij-server/0.45.1/zellij_server/), client and utility crates, and exposes a documented [plugin API](https://docs.rs/zellij-tile/0.45.1/zellij_tile/). That does not establish a supported pane-first server SDK. The server's screen, PTY and routing machinery is internal, with session/tab layout authority integrated into it. The plugin boundary is useful for extending the existing application, not replacing its ownership model.

The checked implementation's normal render path produces full rendered ANSI strings; pane subscriptions expose terminal text rather than a rich canonical terminal scene. Neither is a ready-made embedding interface for independent TUI/web/mobile projections. Mirroring rendered ANSI into another parser would add a second terminal state machine and obscure metadata and graphics; it is not the chosen design. A structured export could be added to the fork, but that would be new native integration work, not an existing supported SDK.

These limits do not mean Zellij cannot be modified. They explain why modifying it enough to become the proposed backend competes with owning a smaller product boundary over reusable libraries.

## Reusable infrastructure shortlist

### Terminal engine: investigate libghostty-vt first

The official [libghostty-vt C API](https://libghostty.tip.ghostty.org/) is explicitly an embedding boundary. Its documented scope includes parsing, terminal state, scrollback, wrapping/reflow and input encoding. The [terminal API](https://libghostty.tip.ghostty.org/group__terminal.html) and [incremental render API](https://libghostty.tip.ghostty.org/group__render.html) make it a stronger candidate than extracting an application's already-rendered output.

The current interface exposes resolved Kitty graphics placements/pixels, input encoders, and OSC 7501 program-status callbacks. Bkpr would retain lifecycle/status metadata in its own model; an engine callback is not a harness registry or proof of notification readiness. Snapshot and render-state facilities are useful primitives, not an already-designed authenticated remote replication protocol or complete frontend renderer.

Important constraints:

- The official API is in development and may break; neither an established stable tagged ABI nor an official Rust SDK was found. That does **not** mean Rust bindings are unavailable. Community crates [`libghostty-vt`](https://crates.io/crates/libghostty-vt) and [`libghostty-vt-sys`](https://crates.io/crates/libghostty-vt-sys) provide safe wrappers and generated FFI. Published 0.2.2 lacks some callbacks, but the checked [upstream Rust revision `9ede35b`](https://github.com/Uzaaft/libghostty-rs/tree/9ede35b8ea94a55ed3608408d67e102883129f1e) already wraps clipboard reads/writes, OSC 7501 and render holds. Prefer that maintained dependency over a competing generic Bkpr Rust SDK; retain Bkpr's owned scene, lifecycle, limits and authority adapter.
- Match the Rust bindings to their native source, not an arbitrary installed library: the checked Rust revision pins Ghostty `521cf2c` (October 21), while the initial Bkpr experiment pins `93967b7` (October 29). The remaining measured API extension is validated paste-offer identity: existing safe paste/read APIs do not expose it. Exact retained MIME contents and origin-bound cancellation need a narrow native provenance addition plus corresponding sys/safe-wrapper support, revalidated against a compatible source pair. Prefer upstream contributions, not vendoring a large wrapper or engine fork.
- Ghostty intentionally does not implement application Sixel decoding. Kitty graphics support does not imply Sixel compatibility. Current support is explicitly absent; choose a maintained codec/engine integration if the new product requires it, rather than inherit a parity obligation from the previous implementation. See the [upstream discussion](https://github.com/ghostty-org/ghostty/discussions/2496).
- OSC 52 and Kitty OSC 5522 read/write/paste hooks are promising, but callbacks are synchronous. Prove remote permission/reply handling without stalling unrelated panes or violating callback lifetimes, including pane-local pause and timeout behavior where required. The host terminal's clipboard policy cannot be silently bypassed. See the [terminal effects API](https://libghostty.tip.ghostty.org/group__terminal.html), [OSC 7501 API addition](https://github.com/ghostty-org/ghostty/pull/14560), and [checked upstream revision](https://github.com/ghostty-org/ghostty/commit/93967b7f9478e94dcbb42125c8a90f34feb40d75).
- libghostty-vt supplies neither a mux daemon nor complete Bkpr TUI, browser or mobile renderers. Font shaping, glyph/image delivery, selections, hit testing, IME/accessibility and terminal-client capability handling remain explicit integration concerns.

Herdr and Muxy are evidence that embedding can be made into a product, not evidence of a stable turnkey SDK. The checked [Herdr source pin](https://github.com/herdrdev/herdr/blob/master/vendor/libghostty-vt.vendor.json) is Ghostty revision `44f2a44`, used through C bindings/product adapters. Muxy vendors third-party `libghostty-rs` wrappers and [pins Ghostty itself to `83fb956`](https://github.com/muxy-app/muxy/blob/main/vendor/libghostty-vt-sys/build.rs), plus a [clear-screen patch](https://github.com/muxy-app/muxy/blob/main/vendor/libghostty-vt-sys/patches/clear-screen.patch). Their application assumptions and adaptation work are reasons to study their boundaries, not adopt a complete fork as Bkpr's foundation. These are source assessments, not runtime parity results.

### PTYs and other candidates

[portable-pty](https://docs.rs/portable-pty/latest/portable_pty/) 0.9 is a public library for PTY creation, process spawning, read/write, resize, wait and termination. It is a plausible PTY dependency, not a mux, terminal emulator, process-recovery system or agent identity service.

[alacritty_terminal](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/) 0.26 provides a public Rust terminal/grid/event boundary and deserves comparison if Ghostty's integration gates fail. No inline graphics implementation was established in the checked interface; do not claim graphics parity by assuming that a terminal crate includes an entire application.

WezTerm has rich terminal and mux code, but its [mux crate](https://github.com/wezterm/wezterm/blob/main/mux/Cargo.toml) is `publish = false` and coupled to application/internal dependencies. A full WezTerm fork would inherit much more product surface than Bkpr needs. Studying terminal-core interfaces is different from declaring the whole application an embeddable maintained SDK.

[tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode) is documented and useful, but exposes raw pane VT output while tmux retains layout/session authority. It does not supply the desired canonical rich terminal scene. Ratatui and Termwiz can help with chrome or rendering utilities; neither is a mux or a substitute for terminal-state ownership. Termwiz's own compatibility warning also matters when choosing a long-lived boundary.

The [plan](implementation-plan.md) owns the detailed dependency and version-selection work. This shortlist is a recommendation, not a commitment that every named library will ship.

## Product model and authority

### Pane, layout and view are distinct

A **pane** is a backend-owned process/PTY and canonical terminal state, identified by endpoint-qualified identity. It survives closing a client or removing its representation from a layout. A **layout/session document** references panes and records split intent and weights, not one globally mandatory pixel arrangement. A **view** is a particular client's projection: a desktop split tree, mobile single-pane navigation, or another presentation over the same references.

Different endpoints may contribute panes to one layout. The layout does not adopt their processes into its host or turn its owner into their execution authority. Remove-view and terminate-pane must be separate, explicit operations. Disconnect, pane exit and deliberate removal must also remain distinguishable.

### One size, several independent permissions

A PTY still has one current row/column size. The latest eligible visible interactive view receives the pane's dimension lease unless a policy pins it. Read-only observers and background connections must not steal that lease. Other views can project or letterbox the authoritative dimensions instead of imposing the smallest connected window on everyone.

Dimension authority is not input authority, and neither implies clipboard authority. Arbitration, reconnects, owner loss, pinning and user-visible indication require explicit protocol semantics. A small mobile screen becoming visible may legitimately win the lease; mere connection is insufficient. Performance or usability advantages remain hypotheses until exercised.

### Execution membership and extensible metadata

The backend owns authoritative process, endpoint, workspace and launch identity facts. Moving a pane between views must not change execution workspace membership or silently widen agent discovery. Harness-native session IDs remain distinct from Bkpr launch IDs and backend context IDs. A new layout must not make an unrelated launch appear to belong to another execution scope.

Frontends may enrich metadata through namespaced annotations, such as per-client presentation labels or bookmarks. Those annotations must be distinguishable from backend facts, with ownership and conflict semantics; an annotation cannot fabricate a working directory, live process, harness status or authenticated association. OSC 7501 remains the semantic status source for reporting harnesses; non-reporting harnesses remain unknown, not inferred from titles or screen text.

## What Bkpr can retain, and what must change

Reusable product assets include harness launch/binding knowledge, authenticated per-launch identities, durable broker messaging, idempotency and explicit read/processed acknowledgements, native wake adapters, Git/CI enrichment, control schema/receipts, recovery policies and the UI's interaction vocabulary. Their contracts are more valuable than preserving an implementation-specific interface.

The current [README](../../README.md) documents important boundaries that the replacement must preserve: registration is not readiness; durable send, wake acceptance, inbox read and processed acknowledgement are distinct; unknown input/mutation outcomes are not blindly retried; peer messages are untrusted context; clipboard permissions remain scoped; SSH-agent routing must not fall back to an unrelated identity. These are requirements, not claims that the existing code is already backend-independent.

Zellij-specific parts require replacement or adaptation: native plugin/WASI integration, pane/session/tab identifiers and context lookup, foreground inspection and registry-change notifications, native layout/geometry control, render transport, sidebar/overlay placement, native clipboard mediation and session attach/agent-forwarding plumbing. Existing UI behavior may be reused conceptually, but frontend chrome cannot remain an authoritative hidden terminal pane shared by every client. Each frontend owns its own chrome.

## Risk and focused proof of concept

The main engineering risk is not basic PTY spawning. It is preserving terminal fidelity and authority boundaries across multiple clients while replacing a mature mux's lifecycle, recovery and platform integration. Graphics, asynchronous remote clipboard permissions, Unicode/reflow, interactive resize, disconnect/reconnect consistency and per-client rendering are substantive product work. Source availability and a successful library build do not prove these properties.

The original assessment proposed the following proof criteria. Current delivery sequencing prioritizes TUI/harness parity with `main`; independently implemented frontends and multi-endpoint proofs gate those future capabilities, not restoration of the local TUI.

1. **Durable ownership:** a real process/PTY survives view removal and client disconnect; explicit terminate ends it. Removing one view cannot terminate a pane still referenced elsewhere.
2. **Independent projections:** two clients display the same canonical pane, and one layout references panes on two endpoints. A narrow single-pane projection does not rewrite another client's layout or execution membership.
3. **Dimension arbitration:** eligible visible interactive views acquire the documented lease, pinning works, and read-only/background/reconnected observers cannot unexpectedly resize the process. Input authorization is tested separately.
4. **Terminal fidelity:** actual application output exercises alternate screen, Unicode widths, scrolling/reflow, colors, cursor/mouse/key encoding and Kitty images through the candidate engine and the intended client path. Unsupported Sixel is explicitly reported, not advertised as supported.
5. **Clipboard correctness:** an OSC 52/5522 request reaches only the authorized originating client, supports the required text/MIME/image flow, handles denial/disconnect, and returns a valid response without blocking unrelated panes, violating callback lifetimes or reusing permission grants. Document any pane-local pause and enforce its timeout.
6. **Recoverable state:** snapshots/deltas converge after reconnect or missed updates, with explicit generation/identity handling. This is recovery of a client view; daemon-restart survival of a running PTY requires a separate demonstrated process-ownership design.
7. **Agent semantics:** a real managed launch remains correctly bound/discoverable as views move, non-reporting status stays unknown, and messaging still distinguishes durable receipt, native wake, read and processed acknowledgement. No terminal-input substitute is introduced for peer messaging.
8. **Bounded ownership:** exact dependency pins, licensing/build integration and the amount of necessary engine patching are recorded. A requirement for broad ongoing engine modifications is a decision point, not hidden scaffolding.

These are capability acceptance requirements, not verification results from this assessment or a requirement to stop TUI migration until every future capability exists. The proof should isolate failures so individual design decisions can be revisited. The approved boundary is Bkpr-owned product/control-plane plus reused terminal/PTY infrastructure; the terminal-library choice remains conditional on relevant evidence.
