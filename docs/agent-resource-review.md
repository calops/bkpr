# Agent costs and resource lifetime review

Source: `/home/calops/Downloads/bkpr Agent Costs.zip`, SHA-256
`2c922826a22439e198263ff551fd568162207837f299688b7b4c2255fe1bc988`.
The archive's prototypes target an older revision. They were reference material,
not applied patches; its model/provider rigs were not executed. The separate
capacity cutover, `2318408`, already removed the fixed pane/attachment ceilings.

## Decisions

- Agent-facing APIs return compact identities, revisions and receipts by default.
  Full context, layouts and bodies require explicit expansion. The TUI's native
  full-registry interface is unchanged.
- Bound every MCP tool result, including errors and explicitly expanded results.
  Do not truncate meaningful data silently or repeat a mutation to recover output.
- Fix actual process/claim/cleanup retention. Preserve recoverable history and
  referenced delivery receipts rather than invent an automatic deletion policy.
- Cut over cleanly to **native wire family 8**. Existing family-7 sessions keep
  running with matching old binaries; no automatic daemon replacement/restart.
  Remote framing, scene encoding, engine ABI and recovery schema are unchanged.

## Measured output

Actual CLI, private daemon and authenticated stdio MCP, not formatter-only tests.
Both pairs use the Cargo dev profile with debug symbols and the same Ghostty
`93967b7` engine. Linux x86-64; 120×35 pane viewports; three compiled synthetic
managed agents and forty ordinary panes beside an initial shell. Ten messages
contain 64 KiB each, including escaped control characters. No real model calls.

These are **bytes, not provider-token estimates**. The stress inbox is deliberately
adversarial; ordinary prose will have different ratios. Default summaries and full
lossless retrieval are different operations, not equivalent-content compression.

| Surface | Before bytes | After bytes |
|---|---:|---:|
| `whoami`, no repository metadata in this fixture | 668 | 499 |
| Default `session.inspect` | 30,362 | 3,783 |
| `pane.rename` acknowledgement | 30,372 | 263 |
| `send_message` receipt | 525 | 111 |
| Default inbox, ten maximum-size escaped bodies | 3,523,874 | 4,008 |
| Repeated default inbox read | 3,523,874 | 49 |
| Complete MCP inbox wire frame | 7,703,257 | 8,663 |
| MCP initialization instructions | 1,476 | 734 |
| Complete advertised tool catalog | 4,651 | 5,126 |
| Packaged Omp control skill | 3,147 | 1,402 |

The catalog grows slightly because it now describes filters, pagination and two
expansion tools. Instruction text shrinks; it would be misleading to claim the
whole catalog shrank. Claude receives per-tool `anthropic/alwaysLoad` metadata for
send/read/ack only, not a server-wide eager-load setting. Actual model-context
loading/token costs were not measured.

Text plus `structuredContent` remain for MCP compatibility. The archive's Claude
observations did not establish double model-context charging; some clients can
print both representations. Reducing payloads solves the real default-output
problem without assuming that removing one representation halves token cost.

Raw comparisons: `target/smoke/agent-costs/actual-native/comparison.json`.

## Interface and limits

- `control` and `bkpr api call` default to compact mutation receipts. Explicit
  `reply:"full"` / `--reply full` requests full native output. Receipts preserve
  affected IDs, created reference-to-ID mappings and the owning revisions.
- `session.inspect` defaults to ten flat items, supports workspace/window/pane
  filters, cursors, `detail:true` and explicit retired-record inclusion. Scope
  filtering precedes pagination. Default pages omit layouts, projects and titles.
- `whoami` exposes exact launch/binding/ownership/revision/readiness facts.
  Discovery adds exact launch/pane filters and cursor pages. Full project/context
  requires `detail:true`; obsolete duplicate identity/readiness aliases are gone.
- `pane.read` defaults to 2048 UTF-8 bytes, trims only trailing row padding,
  preserves indentation/interior spaces and supports tail rows. Continuations
  require the returned canonical scene revision. Temporary-view publication does
  not invalidate its own cursor; actual scene changes reject it.
- Inbox summaries limit each preview to 192 **encoded JSON** bytes. Full bodies
  use recipient-only `read_message` pages: default 2048 raw bytes, at most 4096
  requested raw bytes and 4096 encoded text bytes. UTF-8 boundaries and exact raw
  continuation offsets preserve every byte, including control characters.
- Default inbox reads return unread summaries. `include_read:true` revisits
  read-but-unacknowledged messages. A summary read is not full-body consumption.
  Explicit `ack_ids` can combine acknowledgement of previously read, processed
  IDs with fetching the next page; newly returned messages are never auto-acked.
- Logical MCP tool content is at most 8 KiB of serialized JSON. Oversized results
  return an opaque ID for `read_result`, whose UTF-8 fragments reconstruct the
  original JSON. The compatible wire envelope can be larger than 8 KiB.
- Presentation storage is a weighted 32 MiB LRU **per MCP server process**, with
  five-minute sliding expiry and event-driven cleanup on tool activity. This is
  not a process-wide heap bound; native transport/expansion budgets remain separate.
  No periodic cache thread or forced wake was added.
- Cached output binds to its pre-dispatch launch/conversation/source/generation.
  Fresh identity must still match before storage and expansion; background
  launches cannot use it. Rebind, retirement, eviction or expiry never authorizes
  replay. An uncacheable result reports presentation failure separately from the
  known native operation outcome. Failed/unknown native commits stay failed/unknown.

Pagination deliberately trades extra calls for selective context. The stress
fixture reconstructed one whole escaped 64 KiB body through 87 small pages. If a
caller needs everything, it must ask for it; these defaults do not claim to make
full transfer free. Binding-safe cache presentation also uses local authenticated
identity reads, not guessed environment IDs or a cached startup assertion.

## Resource handling

**Adopted zombies:** the existing off-path process sampler observes registered,
birth-bound trees while panes live. It reaps only verified adopted children by
exact PID, never `waitpid(-1)`, pane roots or independent wait owners. A real
96-orphan regression keeps another pane/provider wait owner alive. Detached
new-session children escaping all observations are not guessed to belong to a pane.

**Root lifetime:** integration checks exposed a stale-sample failure: observing a
cached table from before spawn could erase an owned root and leave shutdown unable
to signal it. Root birth ownership now survives sampled absence for the owning
child's lifetime. Explicit cleanup verifies/signals/counts that root independently;
its owning child retains exclusive wait/reap responsibility. Failure diagnostics
retain observation errors and distinguish root exit from surviving children.

**Fault retry:** finished faulted panes retain their real cleanup handle. Explicit
close retries cleanup and refuses success while owned processes survive. Successful
cleanup releases sampler ownership while preserving the original diagnostic.

**Close concurrency:** window-specific reservations no longer reserve the whole
workspace/session against unrelated changes. Last-window/workspace/session closure
and CAS guards remain. Proven contention uses typed Busy metadata, not an inferred
unknown mutation.

**Retired bookkeeping:** lazily prune unreferenced exited-agent records after the
existing seven-day retention, protecting message/receiver/waiter references. Saved
recovery descriptors, final scenes and referenced receipt history keep their
existing explicit retention policy. They are not globally memory-bounded by this
change. Configurable retention and bulk-forget policy are tracked in `TODO.md`.

**Diagnostics:** a private body-free daemon mutation/lifecycle trail uses a
nonblocking 256-event queue and four slots of at most 1 MiB each. Descriptor-relative
no-follow opens, ownership/mode/hard-link checks and bounded rotation protect storage.
No argv, prompts, message/terminal bodies, tokens or provider stderr are recorded.
Overflow/storage failures increment loss counters and drop diagnostics, never alter
durable acknowledgements. Shutdown waits at most 100 ms for this optional sink;
blocked storage may leave pending records.

## Archive finding dispositions

| Finding | Resolution / deliberate boundary |
|---|---|
| I01 — mutation registry echoes | Compact canonical receipts; full reply explicitly requested. |
| I02 — full-session inspection | Flat scoped/filterable cursor pages and explicit detail. |
| I03 — padded terminal reads | Right-trimmed UTF-8 pages, tail rows and canonical revision fencing. |
| I04 — no first-prompt fact | Body-free once-per-launch native submission observations for Claude, Omp and Codex. AGY remains unknown; no model-acceptance claim. |
| I05 — no status age | Backend timestamp for selected OSC status-record changes; duplicates/title-only changes do not fabricate a transition. |
| I06 — abbreviated IDs | Exact IDs required, bounded in-scope candidate hints; never auto-resolve/replay. |
| I07 — retired ownership/history | Explicit retired/unowned errors, excluded from default inspection. Destructive history expiry deliberately not introduced. |
| I08 — combined processed acks/batch hints | Explicit prior-read `ack_ids`; body-free batch and unread counts captured at durable claim creation. |
| I09 — Claude idle wake appears as hook error | Concise truthful `asyncRewake` wording; native exit-2 wake mechanism intentionally retained rather than falsely reporting success. |
| I10 — quiet delivery | Opt-in `delivery:"quiet"`; durable, never joins a wake claim, part of idempotency equivalence. |
| I11 — repeated inbox bodies | Unread-only default; explicit revisiting and complete body expansion. |
| I12 — timed-out claim blocks later wakes | Release expired claim slot; later distinct messages can wake without replaying old ambiguous effects. |
| I13 — official Claude installer unrecognized | Strict `claude/versions/<semver>` kernel executable recognition, preserving OS birth/ancestry/foreground rules. Irrelevant deeper classifier faults cannot invalidate an already selected shallower native root. |
| I14 — repeated help/schema prelude | Short shared guidance, generated common signatures and on-demand schema lookup. |
| I15 — pane-create placement ambiguity | Schema/runtime all-present/all-absent ownership, revision and complete-layout contract. Same generic atomic spawn path for every command. |
| I16 — broad window-close reservation | Target-window reservation plus real last-parent guards and typed Busy. |
| I17 — faulted close never retries | Retained owned-child cleanup is retried, not merely its cached error. |
| I18 — adopted zombie accumulation | Verified live-pane adopted-child reaping through the existing shared sampler. |
| I19 — full project in whoami | Elementary facts by default; explicit detail for project/context. |
| I20 — unfiltered discovery | Exact filters and bounded cursor pages, scope before pagination. |
| I21 — named keys need all wire fields | Safe defaults in the control API only; strict native wire and layout-aware physical encoding unchanged. |
| I22 — Claude messaging tools not readily loaded | Per-tool native always-load metadata for send/read/ack; no persistent user settings or all-tool eager load. |
| I23 — inherited daemon environment | Intentional startup policy documented. No silent client-environment import, account switching or variable stripping. Restart from a stable environment to change defaults. |
| I24 — retired-recipient dead end | Authorized current same-pane successor hint only; never redirect or replay a message. |

Historical ENOENT classifier incidents and the archive's environment hypothesis
are not retroactively claimed reproduced. Large user recovery/receipt history
still needs an agreed policy, not deletion disguised as an optimization.

## Verification and evidence

- Protected incremental debug build; local Cargo artifacts, eight build/test jobs.
- Host-user workspace: **1139 top-level tests and two nested subprocess tests
  passed**, 18 default-ignored entries. Strict all-target Clippy and formatting pass.
- The isolated Bun 1.4.2 Omp fixture passed: root-only prompt facts, revive/subagent
  exclusion, once-per-launch behavior, private bodies and truthful void-API wake outcome.
- Actual private native CLI/daemon/MCP: compact receipts/inspection, filtered
  discovery, ten escaped bodies, exact complete-body recovery, explicit processed
  ack plus durable outbox observation, quiet suppression, lossless result expansion,
  background rejection/foreground restoration, rebind rejection, prefix hints,
  native prompt observation, live Unicode viewport continuations, real input and
  stale-revision rejection, old-family rejection without daemon restart.
- Actual daemon trail has valid bounded private records; body/title canaries absent.
  Exact-owned-process cleanup records have no survivors. Public live executable
  hashes/inodes are preserved; no live user session or persistent profile changed.
- Separate source reviews covered lifecycle/concurrency and authority/privacy.
  Findings about self-invalidating viewport cursors and post-dispatch cache-owner
  assignment were corrected before acceptance.

Evidence root: `target/smoke/agent-costs/`; final pair:
`pair-f1d81c03-acce3ce1/bin/{bkpr,bkprd}`.

- CLI SHA-256: `f1d81c0331b073537605824fd436604b605c99bae2a114a4ca62a3ffdb7174f7`
- daemon SHA-256: `acce3ce15c01149170a7ab673090cbfc86e17f36ab421a2172e671dc6eeb5181`
- coordinated receipt: `build/protected-191183294245466/result.json`
- workspace details: `build/host-tests.json` and individual logs
- native details: `actual-native/{before,after}/acceptance.json`, MCP frame metrics,
  cleanup and diagnostic receipts; comparison JSON alongside them
- earlier failed integration/fixture attempts remain labeled separately, not passes

This is native helper/integration proof, **not genuine model processing or a new
CPU benchmark**. The fixtures use compiled synthetic harnesses and fake local
providers; no external model services, approval bypasses or installation changes.

## Dependency decisions and primary contracts

- Maintained [Hashlink 0.12.2](https://docs.rs/hashlink/0.12.2/hashlink/linked_hash_map/struct.LinkedHashMap.html)
  provides ordered cache storage; Bkpr owns weights, expiry and authority policy.
  MIT/Apache-2.0; already locked. No parallel generic cache implementation.
- Maintained [semver](https://docs.rs/semver/) supplies native installer version
  parsing rather than another handwritten version grammar.
- Existing Crossbeam-channel, libc and Serde supply diagnostics infrastructure.
  [tracing-appender 0.2.5](https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/)
  and [file-rotate 0.8.0](https://docs.rs/file-rotate/0.8.0/file_rotate/) were evaluated;
  neither supplies this private no-follow storage contract, and the former lacks
  retained-byte bounds. The narrow secure-storage boundary stays product-owned.
- Claude [per-tool alwaysLoad](https://code.claude.com/docs/en/mcp#per-tool-alwaysload)
  and [UserPromptSubmit](https://code.claude.com/docs/en/hooks#userpromptsubmit).
- Omp [native extension lifecycle](https://github.com/can1357/oh-my-pi/blob/main/docs/extensions.md#agentcontext-lifecycle).
- Codex [native app-server turn/start](https://developers.openai.com/codex/app-server):
  observation before forwarding is submission, not server/model acceptance.

Reproduction controllers are archived in
`target/smoke/agent-costs/controllers.zip` with a CRC-checked, SHA-256 manifest;
loose throwaway controllers were removed. Restore its paths under the same
evidence root before rerunning the recorded commands.

The separate extension regression runs with
`bun test integration/omp/index.test.ts` (verified with the repository-pinned
nixpkgs Bun 1.4.2). Bun is a test tool here, not a new Bkpr runtime dependency.
