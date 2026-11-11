# bkpr-protocol

## Boundary

Shared domain types, pure validation, control schemas and bounded codecs. Every other first-party crate consumes this crate; never depend on their implementations. Authorization, process ownership, persistence, provider I/O and UI policy belong to consumers. Blocking framing helpers do not own connections.

## Working entry points

- `lib.rs` re-exports domain types and defines native family, resource limits and pane/view requests. Public API documentation is enforced with `deny(missing_docs)`.
- `registry.rs` carries revision-checked registry operations and typed failure outcomes. `control.rs` generates CLI/MCP argument decoding, discovery and schemas from one allowlisted catalog; core still checks scope/ownership.
- `wire.rs` and `wire/framing.rs` encode/decode native requests, responses and client-lifetime events. `remote_wire.rs` is a separate SSH attachment handshake/lease protocol, not a second native transport.
- `scene_wire.rs` encodes canonical frames and complete changed-row replacements. `DecodedScene::apply` checks fallible baseline prerequisites before mutating the consumer's frame/revision.
- `path_wire.rs` preserves non-UTF-8 Unix path bytes at JSON boundaries. `Payload`/`PanePayload` keep binary scenes separate from metadata; JSON conversion is diagnostic, not a native fallback.

## Contracts and pitfalls

- `PROTOCOL_VERSION` is the native family (currently 8); `remote_wire::VERSION` is independent (currently 1). Package versions do not establish compatibility. Preserve schema ordinals, enum tags, required-capability rejection, presence checks and aggregate expansion budgets: aliased offsets can expand beyond wire size.
- The three `.fbs` files own generated bindings. Use `planus-cli =1.3.0`: `planus rust --format false -o OUTPUT SCHEMA`. Normal builds need only the runtime. Never hand-edit/format generated Rust; fix generator-owned docs at the schema/generator boundary.
- Codec output borrows reusable builder storage. Queued writes need owned framing. Owned response decoding retains scene byte ranges through `bytes::Bytes`; converting scenes to JSON or cloning canonical frames is not the hot receive path.
- Scene revisions, matching baselines and image `(id, generation)` receipts are distinct prerequisites. Do not advance consumer cursors/resources on a rejected update. Cell widths and UTF-8 boundaries are engine facts, not an invitation to re-segment Unicode.
- JSON path decoding uses a direct Serde string/map/sequence visitor. Keep `PathSchema`'s untagged shape for schema generation only: deriving untagged deserialization would buffer the whole value before enforcing the byte-path limit. Caller-owned JSON values and native transport allocation budgets remain separate.
- Owner revision requirements differ by operation; control help identifies the workspace/window revision. View policy separates input, sizing eligibility and clipboard permissions.
- Typed `NotDispatched` and `Unknown` outcomes determine dispatch certainty; diagnostic wording never grants replay permission.
- Launch/conversation/receiver identities and OSC status remain separate facts; recovery descriptors carry no live authority. Preserve credential/path redaction in `Debug`.

## Dependency decisions

Retain Planus's reusable builder caches and `extra-validation`, `bytes` shared response storage, `compact_str` cell strings, Serde/JSON metadata, `schemars` decoder-derived schemas, `base64` diagnostic pixels and `anyhow` validation. `serde/rc` supports shared images. The schema compiler is not a normal build dependency.

The first review found no duplicate package versions in `cargo tree --locked -p bkpr-protocol -d`; workspace duplicates originate elsewhere. Reviewed non-yanked upstream releases: [Planus 1.3.0](https://crates.io/api/v1/crates/planus/1.3.0) (MIT OR Apache-2.0), [compact_str 0.10.0](https://crates.io/api/v1/crates/compact_str/0.10.0) and [schemars 1.2.3](https://crates.io/api/v1/crates/schemars/1.2.3) (MIT). No feature removal/library replacement is justified by dependency count alone; cold-build costs were not measured.

## Verification

Protocol-only tests/rustdoc need no Ghostty build: `nix develop --command cargo test --locked -p bkpr-protocol -j 8` and `nix develop --command cargo doc --locked -p bkpr-protocol --no-deps -j 8`. Existing tests cover native framing/typed requests, malformed/aliased scenes, atomic rejected apply, raw paths and remote handshakes. Public docs also become `schemars` descriptions: exercise actual CLI `api schema` output. Apply the root coordinated workspace verification and live-artifact safety rules for related code changes.
