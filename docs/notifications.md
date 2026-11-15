# Backend-owned notifications

## Status and prerequisite

Notification handling is an agreed design, not implemented acceptance. Its lifecycle prerequisite is now implemented: one long-lived `bkprd` per host/OS user, owning multiple independent sessions. Closing the last workspace closes that session and its attached clients, not the daemon. An empty daemon remains idle and available. Notification handling is a module/worker inside `bkprd`, not an additional local daemon. The future publisher-operated push relay is a separate remote deployment.

Named sessions retain independently leased endpoints and recovery roots inside the host process. Incompatible live daemons are never adopted, replaced or stopped automatically. Explicit private runtime/state roots remain available for isolated fixtures.

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

TUI clients provide observed focus/visibility hints and decide local presentation. Suppress intrusive desktop alerts for an actually attended target where evidence permits; unknown focus must not count as known attention. Coordinate presentation ownership to avoid duplicate alerts for the same desktop destination when multiple TUI instances subscribe. Notification ownership is separate from pane input, sizing and clipboard authority.

Notification activation navigates to a structured source using ordinary client authority, never injects input, executes a provider-supplied command or implicitly creates/replays a pane. Missing/retired sources are reported honestly. User read/dismiss actions and provider acceptance are distinct from application task completion.

Cross-device escalation or delayed phone fallback is not an initial routing requirement; it needs explicit deadlines and acknowledgement semantics.

## Durable outbox and responsiveness

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

Present on the viewing machine, not the remote session's desktop. Evaluate `notify-rust` for native desktop integration and confirmed terminal protocols such as OSC 99 for suitable physical/SSH-hosted terminals. Choose one path per destination, not both. Route terminal writes and activation replies through the existing framework path. Sanitize/encode untrusted text; no escape injection, shell notification commands or model messages typed into panes.

Unsupported desktop capabilities must remain explicit. A local UI toast is not proof of an OS desktop notification. Native OS presentation/activation needs actual isolated desktop/terminal surface verification.

## Later provider: Bkpr publisher relay

Optional accounts explicitly link users' own daemons to a publisher-operated gateway. Daemons submit through outbound HTTPS with revocable pairing-scoped notification capabilities, not terminal credentials. The gateway alone holds the official apps' APNs/FCM provider authority; do not distribute those keys with the app or `bkprd`.

Initial content is session name/status. Rich E2E payloads remain later exploration; pairing may exchange device encryption keys without manual key management. Relay accounts, deployment and official-store provisioning are not authorized by this design document.

## Delivery order and acceptance

1. Implemented: host daemon/session ownership, independent session closure, zero-session idle lifetime, creation/recovery, local/remote routing and explicit host shutdown. See [runtime acceptance](pane-first-mux/engine-proof.md#one-host-daemon-with-independent-local-and-remote-sessions).
2. Implement typed events, scoped policy, bounded outbox/delivery state and observable health.
3. Add resumable client subscriptions and delivery inspection without exposing endpoint secrets.
4. Coordinate TUI desktop presentation with the frontend owner.
5. Implement UnifiedPush publisher and mobile enrollment/receiver. Exercise at least two distributors with isolated registrations and explicit test-device/provider approval.
6. Add the custom publisher relay later as another adapter, not a parallel notification model.

Verification must cover rapid/coalesced status changes, identity replacement/clear/EOF, duplicate reports, focused/unfocused clients, multiple TUI subscribers, disconnected destinations, expiry/revocation, failed durable commits, ambiguous provider outcomes, shutdown/restart and last-session closure. Use fake local providers for deterministic backend tests. A real UnifiedPush reference receiving app can prove publisher/distributor delivery before Bkpr mobile exists, but label that limit; it is not Bkpr app UI acceptance. Never operate on live user sessions, profiles, credentials, approvals or real model services as fixtures.
