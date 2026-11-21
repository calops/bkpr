# bkpr-terminal-engine

## Boundary

Pane-worker-local normalization and device-authority policy over the pinned libghostty-vt safe SDK. Depend on protocol and SDK, never core/client/framework. SDK owns native objects, encoders, borrowed views and callback FFI. This crate owns canonical scenes, resource budgets, view inspection and clipboard/paste policy. `Terminal` is neither Send nor Sync; create/use/drop it on its owning worker.

## Entry points and invariants

- `lib.rs`: terminal configuration, feed/effect draining, resize, synchronized-output holds and view-local history anchors. Feed failure can follow partial native processing; never replay an ambiguous chunk.
- `render.rs`: reusable SDK render iterators and canonical scene extraction. Preserve exact engine Unicode boundaries, wide-cell spacers and default/palette/RGB identity. Immutable images are generation-keyed `Arc`s; the cache retains weak references, not hidden image lifetimes. Missing animation pixels are not fabricated. Images may carry the program's original payload (`Image::source`) when the native engine retained a still PNG or zlib RGB/RGBA stream no larger than the pixels; animated PNGs, frame edits and composed images carry only pixels, because the original would no longer show what the engine shows. Retained originals are not charged to the native storage limit, but at most double an image's memory, in Ghostty and again in the published scene. Temporary history extraction restores the canonical viewport; held scenes remain independent.
- `input.rs`: native key/mouse/focus/paste encoding, not a parallel terminal-mode parser. Selection/search are view-local inspection, distinct from application input and sizing authority.
- Paste return values contain only PTY effects emitted by that native paste call. Preserve earlier queued device replies and non-PTY effects, including their byte accounting; a resize size report must not become paste content.
- Pane theme publication replaces native palette defaults without resetting application OSC overrides. Scheme reports classify the published default background using Ghostty's perceived-luminance rule, not application OSC overrides. Use the native query callback and encoder; queue one unsolicited report for the first known scheme or a flip only with DEC mode 2031 enabled, under normal device-effect limits.
- `callbacks.rs`/`offers.rs`: synchronous pane-local clipboard policy with exact MIME bytes. Offer correlation comes from validated native provenance, never arbitrary program IDs; original bytes, view authority, expiry and consumption remain separate. No handler means denial. File/shared-memory Kitty image media remain disabled.
- Native request/reply buffers and consumed-offer ownership remain callback-local. Never hold a callback-state `RefCell` borrow across `reply`, which can synchronously emit PTY effects. Nonempty MIME reads consume offers before consent/location validation; targets-only reads retain them. Expiry is rechecked after consent, while retained bytes are released by pruning/cancellation/drop rather than a background timer.
- `status.rs`: OSC 7501 replacement, subtree clearing, ancestor app inheritance and bounded records. The optional synchronous observer receives borrowed canonical records after every replacement, before feed/frame coalescing, without a callback-state borrow held. It does not authorize product status; clear/prompt events are withdrawals. Titles/process presence are not semantic status. Core reports execution changes, exits and actual result visibility explicitly.

## Dependency and build boundary

The safe/sys SDK pair, native revision and the two native contributions (clipboard offers, retained Kitty image sources) are recorded in `vendor/libghostty-rs/UPSTREAM.toml`. Update them together through the documented preparation process, never hand-edit vendor/generated bindings or substitute system Ghostty. `build.rs` verifies matching SDK/native installation roots, source, both patch and profile markers, and prepared header/library hashes. Normal builds consume prepared artifacts and do not regenerate bindings.

The SDK's PNG feature supplies maintained decoding with a pre-expansion quota; preserve the shared protocol image-byte limit. SHA-256 is a build-time provenance dependency, not runtime terminal work. Do not replace SDK/PTY/codec machinery or weaken resource bounds to shrink dependency counts. Dependency changes need actual ABI, feature and compilation evidence.

## Verification

Coordinate Cargo and source ownership with backend/frontend owners, especially active graphics/query investigations. Use this checkout's prepared engine/local target and at most eight jobs; preserve as-found live executable paths. Existing adapter, PNG-budget, selection and search tests cover native mode/Unicode/color/image/history/inspection contracts. Public docs must pass warnings-denied rustdoc. Related changes require grouped workspace tests, strict all-target Clippy, formatting and an actual native consumer smoke; UI behavior changes additionally need actual terminal-surface verification. Retain useful evidence under target/smoke and clean owned fixtures.
