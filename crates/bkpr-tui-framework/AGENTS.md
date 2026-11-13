# bkpr-tui-framework

## Boundary

Reusable terminal composition, host input/reply framing and physical output infrastructure. Depend on protocol, not core/client; terminal-engine is test-only. Application navigation/content/policy belongs to client. Consume canonical pane scenes, never reparse pane VT into another emulator. Keep one coordinated output owner for cells, graphics, cursor and terminal modes.

## Entry points and contracts

- `render.rs` owns host modes, input/reply demultiplexing, device probes and clipboard framing. `scene.rs` is included in that renderer scope and supplies composition, clipping, damage, resources and hit maps. Separate device replies from keys before maintained input decoding; bracketed paste stays literal even when it contains escape/reply-looking bytes.
- Scene surfaces are borrowed and ordered. Hit maps describe the exact submitted cell geometry; graphics-only layers do not acquire input ownership. Preserve complete wide/combining glyphs, source-local clips and canonical frame/image immutability. Selection/search/palette/faint annotations are client-local projection, not baseline mutation.
- Image identities combine ownership/generation and stable surface identities; native image IDs alone are not globally unique. Derived projection resources have bounded inactive residency and lazy upload. Plan composition/occlusion before writes; a failed output is not proof that the host remained unchanged.
- `graphics.rs`, `decoration.rs` and `joined.rs` prepare ordinary scene resources off-thread. Worker caches, wake descriptors and readiness do not create another terminal writer. Missing decorative readiness retains text fallback; do not weaken meaningful content or lower fidelity for speed.
- `surface.rs`/`theme.rs` retain trusted UI text/styles and observed default-color roles. `animation.rs` schedules sparse visible effects; idle or hidden content must not gain periodic repaint. `chord.rs` owns shared modifier/group geometry and Unicode cell widths; application descriptions/priorities remain client-owned.
- Trusted UI text uses the allocation-free root `text_width` API: sum `unicode-width` widths of whole extended graphemes. `text`, `window::fit`, inputs, keycaps, lists and toasts share that geometry. Sanitize before measurement; native canonical frame widths remain authoritative. UI graphemes wider than canonical cells allow use equal-extent scalar cells when representable, otherwise a visible replacement plus padding preserves their measured extent and source strings.
- Navigation, list/input, overlay, toast and window modules provide reusable interaction/layout primitives, not Bkpr workspace authority.

## Host ownership and evidence

`TerminalGuard` restores owned image resources, keyboard/mouse/theme/paste modes, raw mode and the original background. Background query mediation retains original XParseColor spelling/precision and handles replies crossing matte restoration. Unknown capability state is not unsupported. Preserve positive truecolor evidence independently from palette/theme observations; graphics queries bind exact IDs and expire independently. No TERM-name guessing.

Clipboard consent/transfer state is caller-owned. Preserve exact MIME bytes, selection and original offer correlation. Routing identity does not itself validate payloads or grant consent. Malformed/expired transfers must never become keyboard input or fabricated contents.

## Dependencies and verification

Retain maintained infrastructure: termwiz input parsing, Crossterm terminal mechanics, resvg/image/PNG graphics, Unicode width/segmentation and nucleo matching. Default features are disabled for termwiz/Crossterm/image/resvg; inspect actual enabled features and real usage before pruning. Large dependency counts alone do not justify custom parsers/rasterizers or widget frameworks. Re-evaluate an upstream widget foundation when concrete shared behavior benefits, not to add a second render loop.

Follow root shared Cargo/artifact ownership. Framework code changes need grouped workspace tests, strict all-target Clippy, formatting, warnings-denied rustdoc and actual changed-path smoke. Exercise native terminal surface when changing visible output/input/mode restoration; use isolated HOME/XDG, immutable binaries, one input reader, real device replies and owned cleanup. Existing renderer/geometry regressions cover occlusion, wide cells, resource generations, frame damage, clipboard framing and capability evidence. Retain sanitized evidence under target/smoke; no live user-session fixtures.
