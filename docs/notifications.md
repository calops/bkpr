# Backend-owned notifications

## Status and prerequisite

The backend event/store/subscription foundation and best-effort TUI desktop presentation are implemented. Push delivery remains a design, not implemented acceptance. The lifecycle prerequisite is one long-lived `bkprd` per host/OS user, owning multiple independent sessions. Closing the last workspace closes that session and its attached clients, not the daemon. An empty daemon remains idle and available. Notification handling lives inside `bkprd`, not an additional local daemon. Desktop presentation is tracked in [issue #7](https://github.com/calops/bkpr/issues/7); the future publisher-operated push relay is a separate remote deployment.

Named sessions retain independently leased endpoints and recovery roots inside the host process. Incompatible live daemons are never adopted, replaced or stopped automatically. Explicit private runtime/state roots remain available for isolated fixtures.

## Implemented foundation

Authenticated OSC 7501 observations are captured through the existing canonical
engine status reducer before PTY-read/frame coalescing. Native status enrollment,
binding and foreground process-birth checks retain their existing authority.
Per-pane ordering and feed epochs fence replacement; no status-only process scan
or second terminal parser is introduced.

Routine working reports, duplicate reports and disabled notification kinds do
not write episode state to disk. Episode suppression is volatile and withdrawn
with its source; a restart never restores live authority from historical rows.
Distinct attention categories remain eligible within an episode. Only an
explicit working-to-idle transition produces idle; done requires an explicit
report. The default policy enables attention, error, idle and done. Existing persisted policy overrides are preserved.

Existing stores retain their saved policy, including an earlier default. To explicitly enable every kind on an existing host without restarting it:

```sh
bkpr request --socket "$XDG_RUNTIME_DIR/bkpr/d" \
  '{"version":11,"op":"notifications","request":{"method":"policy_set","policy":{"scope":{},"kinds":["attention","error","idle","done"]}}}'
```

More-specific saved rules still override the host rule. This changes event eligibility, not the TUI's local focused-pane suppression.

One bounded worker owns the host's private `notifications/notifications.sqlite3`.
`rusqlite` 0.42.0 is MIT-licensed and actively maintained
([release metadata](https://crates.io/api/v1/crates/rusqlite/0.42.0)).
Defaults are disabled and bundled SQLite avoids a new system-library dependency
in copied Nix recipes. This adds a native C build, not an async runtime or a
custom database/WAL implementation. A single connection uses SQLite's rollback
journal and full synchronization; subscribers never retain database readers.

Retained notification bodies are limited to 4,096 records and 8 MiB, with
seven-day expiry and a concrete next-expiry deadline rather than periodic
polling. Other bounds are 4,096 volatile episodes, 64 policy rules, 512 queued
work items, 32 in-flight requests and 32 waiting subscribers. Pages contain at
most 64 records and 128 KiB of record bodies. Saturation is rejected/reported,
not a pane-count limit or a successful-looking durable receipt.

Admission is nonblocking and not crash-proof capture: overflow or a crash before
commit can lose an observation. Persisted loss counters and cursor
`loss_generation` make continuity gaps explicit and acknowledgeable. A new
subscription establishes a current baseline without replay; resuming an older
cursor reports loss/pruning, and the returned cursor permits normal waiting
again. Shutdown success means accepted work drained, not lossless historical
capture, provider delivery or user read. Storage failures and unknown drain
outcomes remain errors.

Unsafe paths and conflicting ownership fail closed. Ordinary corrupt/unavailable
notification storage leaves an explicit unhealthy notification service, without
disabling session execution or fabricating a cursor/policy. `Health` and host
`DaemonInfo` metadata expose this state; no automatic repair overwrites the
database. A stalled worker retains its exclusive storage lease after a bounded
shutdown timeout.

The native API and examples are in the
[reference](reference.md#notification-records-and-subscriptions).

## Responsibilities

- `bkpr-core`: authoritative event derivation, notification policy, scoped destinations, bounded durable outbox and independent delivery records.
- `bkpr-server`: host-level service configuration, worker lifetime, startup and explicit daemon shutdown.
- `bkpr-protocol`: typed notification identities, events, subscriptions, cursors, destination management and delivery diagnostics.
- `bkpr-client`: TUI subscription, local desktop presentation, focus/visibility hints and notification activation/navigation. No backend implementation dependency.
- Mobile app: UnifiedPush connector/enrollment, minimal native message reception/presentation and notification-tap navigation. Receiving push never starts SSH or fetches a terminal scene.
- `bkpr-tui-framework`: terminal-native notification capabilities, coordinated output and reply handling where used. No competing output loop.

Reuse these boundaries; do not create an agent-specific spawning or notification subsystem, generic event platform, second terminal emulator, empty crates or a new authentication framework.

## Authoritative events

Status comes only from currently authorized OSC 7501/native observations under the existing launch, binding-generation, process-birth and foreground checks. Titles, CWD, output patterns, process presence and agent receiver readiness are not semantic status.

Current status publication coalesces state and frontend waits expose latest snapshots. Relevant validated transitions must be captured before coalescing can erase attention episodes; do not implement notifications by diffing client snapshots. Repeated reports, status text/progress changes and unrelated registry revisions must not manufacture new attention requests. Source withdrawal, native outlet EOF, foreground loss, binding replacement and clears are not successful task completion.

Initial event/rule candidates:

| Event | Initial policy |
| --- | --- |
| Question/permission/other blocked attention | Notify configured destinations. |
| Explicit harness error | Notify configured destinations. |
| Working to idle | Configurable; describe as idle, never infer task success. |
| Explicit harness-reported done | Eligible according to user preferences; do not invent completion where absent. |
| Execution exit | Separate lifecycle fact; distinguish user-requested closure from unexpected exit. |
| Working/progress | Update/coalesce an existing presentation where supported, not a notification storm. |

CI completion, application bells and explicit OSC application notification requests are later event sources with separate provenance and opt-in policy. Arbitrary OSC 9/99/777 content must not become authenticated harness status.

Existing agent inbox wake `Notification` and `whoami.notification_ready` remain their current native receiver/delivery domain. User alerts never reuse agent inboxes, wake receipts or processed-message acknowledgements.

## Events, notifications and deliveries

Keep distinct records:

1. Event: source identity, observed time, kind and the relevant validated transition.
2. Notification: user-facing meaning, stable identity, grouping/attention-episode key, expiry and structured source references.
3. Delivery: destination-specific attempt, acceptance/error/unknown state, retry deadline and expiry.

Qualify source references by owning endpoint/session and the relevant pane/launch/binding generation. Historical notification records do not restore live harness status, bindings, tokens or authority. Never persist terminal contents. Default mobile content is session name and status, as accepted by the user; no prompts, transcripts or arbitrary status messages.

Coalesce repeated/superseded updates within an attention episode, not unrelated lifecycle or permission events. Define finite byte/retention budgets and observable overflow/gap handling; never silently claim lossless capture or exactly-once delivery. Initial subscription establishes a baseline; reconnect must not replay all old statuses as new alerts.

## Routing and presentation

Destinations select scope (host/session/workspace/pane), event filters, delivery preferences and an independently enabled/error state. Explicit fan-out first. Do not suppress configured phone push merely because a TUI is attached: attachment is not evidence of attention.

Each attached TUI independently presents best-effort desktop notifications from its live session subscriptions. Multiple TUIs may display the same event, including on the same desktop. There are no global desktop claims, durable presentation receipts, retries or physical-desktop deduplication requirements. This supersedes the earlier shared logical desktop grouping proposal; it does not relax backend record durability or future push delivery contracts.

TUI clients suppress an intrusive alert only for their own actually attended source, using observed physical focus and the current owner-qualified pane/launch/binding. Unknown focus is not known attention. A fresh subscription or reconnect establishes a baseline rather than replaying history. Disconnection, unsupported presentation capabilities, source retirement and bounded resource eviction may lose desktop alerts; no terminal write is an acceptance/display acknowledgement. Notification presentation is separate from pane input, sizing and clipboard authority.

Notification activation navigates to a structured source using ordinary client authority, never injects input, executes a provider-supplied command or implicitly creates/replays a pane. Missing/retired sources are reported honestly. User read/dismiss actions and provider acceptance are distinct from application task completion.

Cross-device escalation or delayed phone fallback is not an initial routing requirement; it needs explicit deadlines and acknowledgement semantics.

## Durable outbox and responsiveness

The provider outbox/retry design below applies to daemon-owned push delivery. Best-effort TUI desktop attempts do not acquire durable claims or report delivery receipts.

The host daemon remains alive after zero sessions and owns delivery independently of any viewer or particular session. Capture compact immutable events and hand off after releasing pane/registry/capture locks. Do not run network, provider calls, storage work, process scans or full-scene cloning on status publication or frontend input/render paths.

Use bounded off-path work, destination isolation, cancellable requests and concrete deadline-based scheduling. Idle workers must sleep; no recurring polling, forced refresh or busy reconnect loops. Configuration secrets/outbox state are separate from recovery/layout records, with private ownership/modes and redacted diagnostics.

Once durably accepted, a notification survives session closure and can be retried while the host daemon is running. No successful-looking durable acknowledgement after storage failure. No claim of cross-store atomicity or crash-proof capture before durable acceptance without implementing that contract. Surface notification health/gaps separately from semantic status. Explicit daemon shutdown drains within a defined bound and retains unfinished/unknown work; restart must not infer that ambiguous requests were never accepted.

Delivery records distinguish queued, dispatching, provider-accepted, failed, unknown, expired and disabled/revoked. Provider acceptance is not display/read/action. Retry transient failures with bounded backoff and provider deadlines; preserve ambiguity and deduplicate by notification identity. Do not blindly replay actions or terminal mutations.

## First push provider: UnifiedPush

UnifiedPush is a protocol, not our hosted relay. The Android distributor holds the shared push connection; the Bkpr app registers with it and receives/renders messages. Backend POST support alone does not supply a phone notification UI.

Use standard Web Push: RFC 8030 delivery, RFC 8291 `aes128gcm` encryption, and RFC 8292 VAPID where supported/required. Registration carries the exact endpoint plus receiver public key/authentication material; retrieve the publisher VAPID public key as necessary. Connector libraries generate keys/decrypt automatically, so minimal session/status content can use encryption without manual user key management. Endpoint URLs are bearer capabilities, not public diagnostics.

The app transports registration/rotation/revocation to the backend through the authenticated client boundary. Validate enrollment with a push challenge. Prevent arbitrary HTTP/SSRF forwarding: checked endpoint destinations, no unchecked redirects/DNS rebinding, and narrow explicit allowances for private self-hosted providers. Do not globally forbid legitimate private hosting or silently allow arbitrary private targets.

POST ciphertext to the enrolled endpoint, not a fabricated ntfy topic. Honor TTL, urgency, provider acceptance/rejection and subscription expiry. A successful HTTP submission does not establish device delivery. Keep payloads well below provider limits and avoid any need to fetch terminal content to display a notification. Evaluate maintained Web Push libraries rather than implement encryption or VAPID ourselves; `web-push` is a candidate, not a selected/pinned dependency yet.

Sources:

- https://unifiedpush.org/developers/intro/
- https://unifiedpush.org/developers/spec/definitions/
- https://unifiedpush.org/developers/implementations/
- https://www.rfc-editor.org/rfc/rfc8030
- https://www.rfc-editor.org/rfc/rfc8291
- https://www.rfc-editor.org/rfc/rfc8292

## Desktop providers

The TUI uses the viewing terminal's positively queried [OSC 99 protocol](https://sw.kovidgoyal.net/kitty/desktop-notifications/), including when the TUI or its panes are SSH-hosted. No terminal-name guess enables it. The framework owns discovery, safe UTF-8/base64 framing, reply demultiplexing, physical focus and terminal-resource cleanup; the client owns source policy and navigation. One output owner handles all terminal traffic. Notifications contain the event category, session name and harness family, not arbitrary status messages, prompts or transcripts.

Each admitted session has one cancellable observer outside the management-worker pool, reusing a runtime and a one-page queue. Processing is round-robin and bounded to one page per UI turn. Empty pages advance the cursor without waking the UI unless health or continuity changes. New attachments and replacement stores establish a fresh baseline; observation failures stop that subscription without replay. Reconnecting the owning session starts a new baseline. At most 64 physical IDs/source callbacks are retained; expiry, eviction, attachment retirement and TUI exit release owned resources on a best-effort basis.

Only a positively observed, locally focused application pane with matching current source identity suppresses an alert. Logical session focus is not physical evidence. Click navigation requires confirmed activation reporting and closure support; otherwise the terminal may provide focus-only or non-actionable presentation according to its capabilities. Activation checks attachment generation, host/session runtime, pane ownership, process birth, launch and binding generation against current published metadata, then uses ordinary client navigation. Retired sources and blocking dialogs produce an honest local toast, not input injection, process creation or deferred automatic navigation.

Unconfirmed/unsupported OSC 99 means no OS presentation attempt; a UI toast is not an OS delivery receipt. OSC 99 has no provider-acceptance/display acknowledgement, so successful writes remain unknown outcomes. Multiple TUI instances may independently notify, and the OS may group or discard attempts. No direct native fallback or new dependency is installed: [notify-rust 4.19](https://docs.rs/notify-rust/4.19.0/notify_rust/) was evaluated, but a TUI-local native provider can target the wrong machine when the TUI itself is SSH-hosted. The selected terminal path preserves viewing-machine ownership.

Actual incremental-debug acceptance with Kitty 0.49.2, Dunst 1.13.2, private D-Bus/Xvfb and real loopback SSH is retained under `target/smoke/desktop-notifications/integrated-actual-4/`. It covers non-replaying fresh attachments, independent TUI attempts, physical popup activation to both local and remote sources, local focus suppression, retired-source rejection and device/input separation. `unconfirmed-actual-2/` proves real xterm leaves OSC 99 unconfirmed without emitting unsupported notifications or breaking ordinary pane input. Screenshots were inspected; owned cleanup had no survivors. Coordinated workspace tests (1,339 passed, 19 ignored), strict all-target Clippy, formatting and warnings-denied rustdoc passed for the joined source.

## Later provider: Bkpr publisher relay

Optional accounts explicitly link users' own daemons to a publisher-operated gateway. Daemons submit through outbound HTTPS with revocable pairing-scoped notification capabilities, not terminal credentials. The gateway alone holds the official apps' APNs/FCM provider authority; do not distribute those keys with the app or `bkprd`.

Initial content is session name/status. Rich E2E payloads remain later exploration; pairing may exchange device encryption keys without manual key management. Relay accounts, deployment and official-store provisioning are not authorized by this design document.

## Delivery order and acceptance

1. Implemented: host daemon/session ownership, independent session closure, zero-session idle lifetime, creation/recovery, local/remote routing and explicit host shutdown. See [runtime acceptance](pane-first-mux/engine-proof.md#one-host-daemon-with-independent-local-and-remote-sessions).
2. Implemented: typed transitions, scoped policy, bounded durable notification records and observable health. Provider-specific delivery attempts remain part of their adapters.
3. Implemented: resumable scoped inspection/subscriptions with store identity, sequence and acknowledgeable loss generation.
4. Implemented: independent best-effort TUI desktop presentation and source activation through confirmed OSC 99, with local observed-focus suppression. See desktop provider acceptance above.
5. Implement UnifiedPush publisher and mobile enrollment/receiver. Exercise at least two distributors with isolated registrations and explicit test-device/provider approval.
6. Add the custom publisher relay later as another adapter, not a parallel notification model.

Verification must cover rapid/coalesced status changes, identity replacement/clear/EOF, duplicate reports, focused/unfocused clients, multiple TUI subscribers, disconnected destinations, expiry/revocation, failed durable commits, ambiguous provider outcomes, shutdown/restart and last-session closure. Use fake local providers for deterministic backend tests. A real UnifiedPush reference receiving app can prove publisher/distributor delivery before Bkpr mobile exists, but label that limit; it is not Bkpr app UI acceptance. Never operate on live user sessions, profiles, credentials, approvals or real model services as fixtures.
