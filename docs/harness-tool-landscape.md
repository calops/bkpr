# Harness tooling landscape and opportunities for Bkpr

Research snapshot: **2026-11-05**. Bkpr baseline: native replacement branch `feat/pane-first-mux`, published revision `62be190e2467`, plus the roadmap and agreed multi-session design at the time of research. This is a product/architecture survey, not an implementation plan. **Subsequent user decision:** add attention inbox, task-linked communication/handoffs, advisory resource reservations, reviewable result artifacts and coherent delivery diagnostics to the roadmap. **ACP hosting is rejected:** Bkpr remains terminal-first across TUI and future web/mobile clients, preserving each harness's native interface and bespoke features through rich per-harness integration.

## Conclusion

The useful frontier is no longer just putting several agent terminals in a sidebar. It is making independent agents' work **actionable, reviewable and transferable**: who needs a decision, who owns the next step, what evidence supports a result, and how another agent can safely take over.

Bkpr can supply the durable execution and control layer underneath those workflows without becoming another model harness, project-management system, provider router or account manager. The strongest new opportunities are an attention plane, task-correlated communication, explicit handoff/review artifacts, and narrowly scoped integrations with external task tools. Sandboxed execution remains a larger optional product expansion. Structured harness hosting through ACP is an explicit non-goal, not a future interoperability recommendation.

## Reading guide and evidence limits

- **D — documented:** capability described by the project's primary documentation. Not exercised here.
- **S — source-inspected:** selected implementation or test source was inspected in addition to documentation. This does not mean its suite passed, its security was audited, or its product works end to end.
- **H — discussion-only:** an HN user's description, not independently established product behavior.
- **Proposal:** a possible Bkpr design or integration, not an implemented capability.

Discovery started with [Ask HN: What's your current AI agentic setup?](https://news.ycombinator.com/item?id=50366117), then followed primary project sources. A full [HN API snapshot](https://hn.algolia.com/api/v1/items/50366117) contained 418 comment nodes and 89 unique outbound URLs when fetched. Those are snapshot counts, not an adoption sample. Much of the thread is self-promotion, personal workflow, model pricing or unsupported effectiveness claims. No tool was installed or run, no model/provider service was invoked, and no throughput/security comparison was performed.

Selected linked tools and named adjacent systems are covered below; this is not an exhaustive global census. Repository default branches and documentation change. Claims about hundreds of workers, token savings, fastest memory, autonomous correctness or production readiness are not accepted as measured evidence. Research inputs are retained locally under `target/smoke/harness-landscape/`; citations in this document are the portable sources.

## What Bkpr already covers

The [native implementation report](pane-first-mux/engine-proof.md) and [TODO](../TODO.md) distinguish implemented work from future scope:

- Native daemon-owned panes/processes/PTYs, workspace/window/layout ownership, canonical terminal state and independent clients.
- Typed CLI/MCP control, atomic pane creation/final placement, explicit terminal input and visible-viewport reads.
- Process/launch-authenticated harness integration, sender-scoped message idempotency, durable inbox/outbox, distinct wake/read/processed-ack evidence and native notifications.
- Persistent session identities, fresh daemon incarnations, named-session recovery, input/size/clipboard authority, and direct SSH attachment to existing remote sessions.
- Main-branch TUI parity, local Git metadata, selection/search/history and terminal-native/rich presentation.

**Under active implementation:** concurrent local/remote session attachment, independently asynchronous connections, session-grouped sidebar feedback, reconnect and force-reconnect. Do not count this survey as acceptance of that work.

**Already planned:** worktree workflows, command palette, forge/PR/CI enrichment, opt-in persisted terminal history, configuration/preferences and local cross-daemon agent discovery/messaging. GUI/web/mobile remain deferred. Displaying several sessions does not itself broaden agent scope; `discover_agents` still defaults to the current live workspace.

## Landscape: different layers, not one competitor list

### Terminal/session companions and agent workbenches

| Project | Distinctive capability and evidence | Relevance to Bkpr |
| --- | --- | --- |
| [Hangar](https://github.com/jeffer1312/hangar) | Native desktop plus phone/PWA control tower; structured conversations beside original terminals, pairs/groups, approvals/questions, orchestration and handoffs. Rust server with Python adapters; terminal sessions commonly use tmux. **D/S.** | Deepest initial case study below. Borrow human-attention and continuity objects, not another terminal ownership layer. |
| [Herdr/core/TUI](https://herdr.dev/core/) | Persistent terminal server and independent clients; OSC 7501 semantic program status, scriptable helpers, generated [OpenRPC/stream contracts](https://raw.githubusercontent.com/herdrdev/herdr-core/master/schema/README.md). **D/S.** | Closest existing architectural reference; much is already Bkpr scope. Study compatibility, stream backpressure and helper-result semantics. No reason here to replace the native mux again. |
| [Muster](https://github.com/amterp/muster) | macOS native terminal/session daemon with libghostty, local/SSH panes, agent messaging and a [versioned protobuf API](https://github.com/amterp/muster/blob/main/docs/cli/api.md) with operation identities, subscriptions and explicit unknown outcomes. **D/S.** | A close protocol-design peer. Many guarantees overlap ours; different encoding is not evidence that ours needs replacing. Compare semantic contracts, not unmeasured performance. |
| [Codealong](https://github.com/codealong/codealong) | Reads harness-owned session records and presents a project/worktree/branch/PR attention board. “Needs you” and “Your turn” are product concepts; [Adapter/Reader](https://github.com/codealong/codealong/blob/main/src-tauri/src/harness.rs) separates observation from available controls. **S.** | Strong attention/adapter reference. A Bkpr adapter could expose authoritative state to this kind of board without it owning our processes. Avoid universal terminal-injection fallbacks. |
| [Belfry](https://belfry.robgough.net/) | macOS/iPad front-end for real tmux; separate control/data paths and hooks for [agent status](https://github.com/robgough/belfry/blob/main/docs/agent-status.md). **D/S.** | Concise “needs you / finished / working” presentation and per-host state locality. Hook facts and coarse process/title fallback must remain distinguishable; Bkpr must not adopt inferred semantic status. |
| [Conductor](https://www.conductor.build/docs/concepts/workflow) | Workspace/branch delegation with [diff comments](https://www.conductor.build/docs/reference/diff-viewer), review and cloud workspaces; [remote control](https://www.conductor.build/docs/reference/remote-control) and a [cloud API](https://www.conductor.build/docs/api). **D.** | Valuable review UX: feedback refers to a concrete diff/worktree, not a vague conversation. Possible external service adapter; not an execution model to import. API/remote features are version-sensitive. |
| [Paseo](https://github.com/getpaseo/paseo) | Daemon with desktop/web/mobile/CLI clients, harness adapters, typed timeline and [connectivity](https://paseo.sh/docs/connectivity) via direct connection, SSH, Tailscale or consented relay. [Architecture](https://github.com/getpaseo/paseo/blob/main/docs/architecture.md) describes capability/sequence/subscription ownership. **S.** | Strong future cross-device reference. Borrow explicit catch-up, pairing/revocation and operation ownership, not its whole provider/browser/voice platform. |
| [Juggler](https://github.com/juggler-ai/juggler) | Persistent synchronized structured conversations; model transactions expose prompts, tool schemas/results, usage and timing. [Extension/security model](https://github.com/juggler-ai/juggler/blob/main/SECURITY.md). **S.** | Inspectable provenance is useful for future review clients. Bkpr should link harness-native artifacts rather than reconstruct or own model transactions. App is AGPL; SDK has different licensing. |
| [Pendant](https://github.com/cdervis/pendant) | VS Code/VSCodium presentation layer for Pi with rich tools/diffs/plans, session controls and editor context. **S.** | A complementary editor surface, not a remote daemon. Preserve the underlying harness's settings/skills/session authority; exchanging artifact/context references may be more useful than integrating application code. License not established in this survey. |

### Launchers, orchestration and task systems

| Project | Distinctive capability and evidence | Relevance to Bkpr |
| --- | --- | --- |
| [Remuda](https://github.com/yendo-eng/remuda) | One-command disposable workspaces and agent launch, profiles, reusable prompts, issue/thread context, Docker and multiple multiplexer backends. **D; detailed case study below.** | Natural complementary layer. A Bkpr session-manager backend is a candidate, not an implemented integration; assess its actual adapter interface before promising compatibility. |
| [Nova Bus](https://github.com/mas-bandwidth/nova-tools/tree/main/cmd/nova-bus) | Redis-stream agent bus with tokenized send deduplication, at-least-once receive, correlation, non-consuming wait and operational receipts. **D/S; detailed case study below.** | Existing Bkpr messaging already covers much of its foundation. Borrow metadata/diagnostic semantics rather than install Redis as a new mandatory authority. |
| [Nova Sprint](https://github.com/mas-bandwidth/nova-sprint) | Cards, ordered streams, dependencies, sentinel gates, capacity/capability-aware assignment, review/landing and fleet coordination; [coordinator specification](https://github.com/mas-bandwidth/nova-sprint/blob/main/docs/SPRINT-COORDINATOR.md). **D/S.** | Optional orchestrator above Bkpr. Gates that do not consume workers and preflight overlap/dependency checks are useful patterns. The HN author's fleet/throughput report is anecdotal, not a benchmark. |
| [Obeya](https://github.com/sadilek/obeya) | Task canvas, per-task workspaces/agents, [workspace leases](https://github.com/sadilek/obeya/blob/main/docs/design/agents-and-workspaces.md), collision-aware queuing, and human approval of narrated demo artifacts. **D/S.** | Review evidence and resource-slot visibility are more interesting than another canvas. Adapting execution would be substantial because it currently owns its own agent/workspace lifecycle. |
| [Spool](https://github.com/enes-alatas/spool) | Persistent Claude loops around a SQLite hub: tick/message wakes, paused/capped workers, Docker or host runtimes and account-plan constraints. [Architecture](https://github.com/enes-alatas/spool/blob/main/docs/ARCHITECTURE.md). **D/S.** | Useful distinction between durable workstation and sleeping agent process. Borrow bounded wake/loop policy if needed; do not absorb Claude-only account/login/provider machinery. |
| [Kan](https://github.com/amterp/kan) | Small repo-resident board: TOML configuration and JSON cards, CLI/agent guidance. [Cross-board dependencies and ready-query design](https://github.com/amterp/kan/blob/main/docs/design/work-across-boards.md) is explicitly not yet built. **D/S.** | Attractive external task-store candidate. Integrate implemented card operations first; do not depend on planned `ready`/cross-board behavior or build a second issue tracker in Bkpr. |
| [Beads](https://github.com/gastownhall/beads) | Dependency-aware persistent work graph, atomic task claiming and memory links; current [README](https://github.com/gastownhall/beads/blob/main/README.md) uses Dolt, not merely a JSONL file in Git. **D.** | Stronger task-graph integration candidate when dependencies/claiming are required now. Keep task data in Beads and record Bkpr launch/result references. Dolt deployment/schema migrations are real costs; initialization can modify harness/project configuration. |
| [Gas Town / Gas City](https://yegge.ai/gastown) | Beads-backed orchestration with workers, monitors, merge/refinery roles and reusable orchestration direction. **D, author's architecture overview.** | Study serial integration/merge gates and durable work surviving ephemeral agents. Better an external controller using Bkpr than a wholesale factory inside our daemon. No claim here that its advertised concurrency yields better code. |
| [Optio](https://github.com/jonwiggins/optio) | Local/Kubernetes execution, event/cron/ticket triggers and persistent agents. [Reconciliation](https://github.com/jonwiggins/optio/blob/main/docs/reconciliation.md) separates pure decisions, CAS actions, run leases and durable output offsets. **D/S.** | Useful failure-recovery mechanisms, but an entire platform would duplicate execution, queues, credentials and provider integration. A task/run connector is a future possibility, not drop-in reuse. |
| [Quarterdeck](https://github.com/oscscull/quarterdeck) | Local-first scheduler that records next-turn owner (`you`, `agent`, `others`, `later`), clocks, triggers and owning machine; [specification](https://github.com/oscscull/quarterdeck/blob/main/docs/SPEC.md). **D/S.** | Good attention/scheduling client above a task/event interface. Borrow bounded event chains and approved-script identity. GPL-3.0-or-later; embedding requires license review. |
| [Coterie](https://github.com/ASIXicle/Coterie) | Persistent multi-agent Linux environment, Maildir, boot manifests and reviewed/hash-checked memory changes; [security limits](https://github.com/ASIXicle/Coterie/blob/main/docs/SECURITY.md). **D/S.** | Auditable handoff/policy artifacts are useful. Do not copy shared-secret/cooperative caller identity or terminal doorbells as a substitute for Bkpr's authenticated native delivery. |
| [Munder Difflin](https://github.com/HarnessMD/munder-difflin) | Office-style desktop supervisor over many real CLIs, shared memory/mailboxes, blackboard and human escalation. [README](https://github.com/HarnessMD/munder-difflin/blob/main/README.md) describes a single Git committer for its hive. **D.** | A task ledger/approval queue is relevant; animated avatars and a universal autonomous “boss” are a different product choice. Memory speed/autonomy claims are unverified. Its hive repository is not the user's source repository. |
| [Ruflo](https://github.com/ruvnet/ruflo) | Broad meta-harness with CLI/MCP, Claude/Codex integrations, hooks, routing, memory, federation and visual console. **D, primary README.** | Possible external workflow tool, not a Bkpr dependency. Portability varies by host: generic MCP exposure does not make Claude-specific hooks/mods available in every harness. Large tool count is not evidence of better ergonomics or correctness. |
| [SpurCode](https://github.com/getspur/SpurCode) | Task/DAG orchestration with worktrees, review lane and staging integration; [ACP-out/MCP-in split](https://github.com/getspur/SpurCode/blob/main/docs/architecture-spur-mcp.md). **S.** | Good division between host-controlled execution and agent-requested delegation, plus explicit review gates. This is a much broader orchestration product; GPL-3.0-only and documented default telemetry merit separate review. |

### Harnesses and structured interoperability

These are usually the software Bkpr should host or integrate with, not replace.

| Project | Supported boundary found | Bkpr opportunity and limit |
| --- | --- | --- |
| [Pi](https://pi.dev/) | First-party [JSONL RPC](https://pi.dev/docs/latest/rpc), streaming events and [extension UI callbacks](https://pi.dev/docs/latest/rpc-extension-ui). **D.** | Integrate the ordinary harness richly through its native extensions/hooks and suitable APIs while preserving its interface. RPC is an architectural reference, not a reason to replace the TUI. The ACP wrapper located here is community code; Bkpr ACP hosting is out of scope. Extensions execute with user permissions. |
| [OMP / oh-my-pi](https://github.com/can1357/oh-my-pi) | First-party [ACP](https://omp.sh/docs/acp), [RPC](https://omp.sh/docs/rpc), extensions and collaboration. **D/S.** | Continue rich native plugin/extension/API integration that preserves OMP's bespoke features and interface. ACP availability is a fact about OMP, not a Bkpr integration recommendation. |
| [OpenCode](https://github.com/anomalyco/opencode) | [HTTP/OpenAPI server and events](https://opencode.ai/docs/server/), plugins and [ACP stdio mode](https://opencode.ai/v2/docs/cli/acp/). **D/S.** | Investigate native plugins/server events only where they enrich the original harness experience. Its ACP process owns a private server rather than attaching to the shared service; that mode is not a Bkpr integration target. |
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | Plugin-oriented harness; [automation ACP profile](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/acp/acp/README.md) with persistent sessions, permissions, cancellation and settlement rules. **D/S.** | Good accepted-versus-settled lifecycle reference. Any Bkpr integration must preserve the harness's own interface through native capabilities; its developer-preview ACP profile is not our execution path. |
| [ECA](https://eca.dev/) | Its own [LSP-inspired JSON-RPC protocol](https://eca.dev/protocol/), editor context, hooks, questions/approvals and remote clients. **D/S.** | Potentially complementary editor context or review integration, not a replacement harness interface or a normalized Bkpr chat frontend. |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Personal-assistant platform plus [ACP mode](https://hermes-agent.nousresearch.com/docs/developer-guide/acp-internals), messaging gateway, scheduling and persistence. **D/S.** | Native harness integration may be considered if useful; ACP hosting is not a target. Its multi-channel gateway, long-term learning, model loop and account setup should remain external. |
| [OpenClaw](https://github.com/openclaw/openclaw) | Existing multi-client [WebSocket gateway](https://docs.openclaw.ai/concepts/architecture), device pairing, tools and channel sessions. **D/S.** | A future external assistant can consume constrained Bkpr tools/events. It is not an ACP attach protocol or reason to give chat ingress unrestricted terminal input. |
| [3code](https://github.com/capocasa/3code) | Terminal-native agent and embeddable Nim runtime with readable session artifacts. **D/S.** | Demonstrates a clean runtime/library seam. Owning its model/provider loop is outside Bkpr's purpose; preserve it as an external harness if demand warrants. |

**ACP/RPC boundary:** the inspected OMP, Pi, OpenCode, DSH and Hermes stdio modes launch separately controlled harness processes. They are not drop-in observers of an already-running original TUI, and their dedicated protocol pipes are not terminal streams. The user has rejected adopting ACP hosting: preserve the ordinary native harness interface, including bespoke features, and enrich it one harness at a time through hooks, plugins, extensions and supported APIs. The protocol inventory above documents other projects, not a Bkpr adoption plan.

### Isolation, context, artifacts and collaborative surfaces

| Project | Mechanism and evidence | Bkpr relationship |
| --- | --- | --- |
| [jail](https://github.com/andrei-dubovik/jail) | Linux bubblewrap+pasta wrapper, project-local Claude state and selected host mounts. **S.** | Useful simplicity/state-scoping reference, explicitly not audited/production-safe. Credentials are still mounted; same-kernel namespaces and enabled networking are not a secret-free execution environment. |
| [YAAC](https://github.com/bsklaroff/yaac) | Kubernetes/gVisor workspaces, egress policy and credential-swapping proxy; distinct [containerless mode](https://github.com/bsklaroff/yaac/blob/main/docs/containerless-driver.md). **S, not security-audited.** | Strong isolation-policy reference, not a platform to vendor. Containerless checkouts are explicitly not isolated. Its rootful control infrastructure and custom proxy remain trusted, powerful components. |
| [Filestash Agents](https://www.filestash.app/agents/) / [FDrive](https://github.com/mickael-kerjean/fdrive) | Policy-controlled storage front-end plus local sync/cache/watch and platform filesystem adapters. **D/S.** | Integrate approved artifact roots/URIs instead of implementing filesystem synchronization. An authorized mount is not process isolation. FDrive is AGPL-3.0. |
| [Technical Project Plans](https://photostructure.com/coding/claude-code-tpp/) | Living repository artifacts with phase state, rationale, rejected options, required reading and acceptance criteria. **D.** | Low-infrastructure handoff format. Store and link curated task context rather than make every new agent consume a transcript or global memory dump. Claimed quality improvements are not independently measured here. |
| [Key](https://github.com/resolveworks/key) | Durable Pi-based host with stored/queryable prior sessions and compact successor contexts. **S.** | Continuity and explicit predecessor/successor artifacts are useful. Its read-only-root bubblewrap layout is not full network/PID/user isolation; do not adopt it as a security template. |
| [pi_group / Beezy](https://github.com/PAndreew/pi_group) | Mobile-first shared/private chats around Pi/Pi Durable, archives, project memory and shared browser. **S.** | Good future multiplayer/observer reference. Its own boundary is application authorization; agents and jobs still run with server-user host access. |
| [SpurLab](https://getspur.dev/spurlab) | Agent notebook with Python/SQL/chart/notes and local notebook/kernel artifacts. **D.** | Useful reminder that result artifacts can be more valuable than chat. A notebook client could be adjacent to Bkpr, but implementing a data notebook is not our mux's job. |

Additional HN mentions such as private “Lab”/“Hivemind” setups, emerging personal agents, Muse/Dots/Grok Bot and model/provider gateways are leads rather than established integration targets in this report. An unlinked personal setup or commercial claim was not treated as a validated API. Subscription/model prices are intentionally excluded: they change rapidly and are not Bkpr's core engineering boundary.

## Three initial projects in depth

### Hangar: take the human-attention and continuity layer

Hangar's [product/README](https://github.com/jeffer1312/hangar/blob/main/README.md) describes a control tower over existing agents. Its architecture explicitly separates structured transcript content from terminal state/input. That is more interesting than its choice of tmux.

The strongest concrete source example is [question capture](https://github.com/jeffer1312/hangar/blob/main/backend/app/askquestion.py): a question sidecar is reconciled with native transcript evidence rather than treated as permanently pending because an old screen contains a prompt. For Bkpr, a first-class attention item should retain its native request identity, originating launch/conversation generation, available actions, current state and reply route. A stale approval card must not authorize a new tool request after reconnect.

Its [bounded handoff dossier](https://github.com/jeffer1312/hangar/blob/main/backend/app/bastao.py) is another useful pattern: limited extractive context assembled from structured history and repository/sidecar facts. A Bkpr version could link the goal, constraints, worktree/commit, uncommitted work, decisions, pending messages and verification evidence. It should state missing facts and provenance, not invent continuity.

Distinguish that from native cross-provider transcript conversion. Hangar's [Claude-to-Codex transfer](https://github.com/jeffer1312/hangar/blob/main/backend/app/adapters/codex/transfer.py) is a stricter, version-dependent import path; its [guide](https://github.com/jeffer1312/hangar/blob/main/docs/USAGE.md) marks it as under validation. Do not promise universal conversation migration or silently rotate accounts. A new agent with a transparent handoff artifact is an honest initial feature.

Its phone UI validates the value of resolving a blocked agent away from the desk. For Bkpr's later companion client, prioritize notification, inspect decision context, answer/reject, interrupt and queued prompt before building a full mobile terminal, Git GUI, browser and voice assistant. Pairing, revocation and host permissions require their own design. Hangar itself describes owner-token possession as remote-shell-equivalent authority; that is not a narrowly scoped observer capability.

### Nova Bus: take precise observation contracts, not another broker

The [specification](https://github.com/mas-bandwidth/nova-tools/blob/main/docs/SPEC-BUS.md) is more valuable than “sent once, delivered until acked” as a slogan:

- Receive is at least once; a crashed/stalled consumer's pending message can be reclaimed after a 15-minute claim window.
- Send deduplication is optional through a sender token. Identical normalized content returns the original receipt; changed content is refused. Retry life defaults to 24 hours and token cleanup to seven days. After cleanup, token reuse can produce a new message. A plain send has no such retry guarantee.
- `wait` observes without consuming or acknowledging. An explicit cursor and “armed” result close disarm/re-arm gaps. Observation progress is not model consumption.
- `subject`, `re`, `card`, `head` and `kind` give messages lightweight routing/work context. `re` is a correlation edge, not a complete task state machine.
- Separate delivered/read/acted receipts, overdue views and ask deadlines are operational records. Neither a reply nor a completed handler proves the underlying task was done correctly.

Bkpr already requires sender idempotency and distinguishes durable acceptance, wake, inbox read and processed acknowledgement. That is not a missing feature to buy with Redis. Useful additions would be reply/task correlation, non-mutating metadata inspection and a resumable CLI event/wait interface for external tools. If a new semantic field is introduced, it must also participate in the idempotency fingerprint.

A practical product opportunity is a message-path diagnostic view: launch recognized, authority live, receiver subscribed, wake capability, queued notification, native acceptance, inbox read and processed acknowledgement, with reasons/ages. Existing Bkpr primitives already expose parts of this; the gap is coherent diagnosis, not another “ready” boolean.

There are important nonfits. Nova uses fleet names/Redis identities, and its spec acknowledges a Redis ACL limitation in which cooperative client policy is part of recipient confidentiality. Bkpr should retain backend-enforced recipient/scope checks. Nova's global body-bearing log is also unlike Bkpr's bounded body-retention policy; audit metadata should not silently become permanent conversation storage.

There is documentation drift: early spec prose calls push proof advisory, while current [hook test source](https://github.com/mas-bandwidth/nova-tools/blob/main/cmd/nova-bus/hook_test.go) expects acting identities to be hooked and verifies gating. These test expectations were inspected, not executed. Resolve exact version/policy before treating prose as an integration contract. TLA+ models are useful design evidence, not proof that the deployed CLI and all harness adapters satisfy the model.

### Remuda: compose the work, don't own another mux

Remuda's [commands](https://github.com/yendo-eng/remuda/blob/master/docs/commands.md) combine disposable workspaces, agent selection, explicit profiles, reusable prompt snippets and Jira/Slack/GitHub issue context. `vibe-check` is a branch/PR-review launch workflow, not a model benchmark framework. Its [experiments](https://github.com/yendo-eng/remuda/blob/master/docs/experiments.md) include copy-on-write clones and launch manifests; “experiments” here does not mean automatic comparison of agents.

This expands our planned worktree workflow: not only “create a worktree,” but “launch this named recipe in it with this explicit task context.” Bkpr could own a small launch recipe, or expose a session-manager adapter so Remuda keeps that policy. Reusing an external launcher is attractive if it can target our typed creation/control interface without falling back to PTY typing or duplicating capture/recovery authority. Compatibility has not been demonstrated.

Recipe design must keep trust boundaries visible: preview expanded argv/context; preserve unrelated native harness settings; mark fetched issue/chat content as untrusted; do not execute repository setup hooks merely because a worktree exists. Persisted launch manifests must not become automatic replay of old arbitrary commands/settings during recovery.

Its [container documentation](https://github.com/yendo-eng/remuda/blob/master/docs/container-mode.md) is explicit about exposure: writable workspace, forwarded API/GitHub credentials, SSH agent access and, for some account modes, writable host harness configuration/auth directories. That may suit its workflow, but “containerized” is not equivalent to “cannot read or alter my account.” Do not copy those defaults into Bkpr's ordinary launch path.

One narrow worktree lesson is worth retaining: Remuda locks linked worktrees to prevent Git pruning their records when container paths differ from host paths. If Bkpr later combines container execution and linked worktrees, model host/container path identity and cleanup explicitly rather than assume Git sees the same filesystem.

## Scope opportunities, ranked for Bkpr

The user accepted the attention inbox, task-linked communication/handoffs, resource reservations, reviewable artifacts and delivery diagnostics as separate TODO items. External task/orchestrator adapters and sandboxed execution remain proposals, not approved scope. ACP hosting is an explicit non-goal.

| Priority | Opportunity | Status relative to current scope | Smallest coherent addition / important boundary |
| --- | --- | --- | --- |
| 1 | **Human-attention inbox** | New product layer; semantic status alone does not provide questions/approval actions. | Correlated native questions, approvals, blockers and review requests, with source/generation, available actions and resolution. Use hooks/extensions/native APIs where available; never infer permission from terminal text. Unsupported harnesses remain explicitly unsupported. |
| 2 | **Task-linked messages and portable handoffs** | New beyond reliable free-text peer messaging and same-harness recovery. | Add reply/task/artifact references and a bounded, inspectable handoff packet. Separate agent-declared task completion, reviewed acceptance and processed message acknowledgement. Keep scope defaults unchanged. |
| 3 | **External task/orchestrator adapters** | New integration direction, complementary to planned worktrees/cross-session messaging. | Prototype one implemented task store, e.g. Beads or Kan cards, mapping external task/run IDs to Bkpr launch identities and receipts. Keep dependency graphs/schedules in their owning tool unless a clear Bkpr-specific gap emerges. |
| 4 | **Explicit resource reservations** | Not currently a first-class user/agent feature. | Advertise advisory ownership of a worktree, file set, Cargo slot or Git index with owner, generation and expiry/release. This addresses real shared-worktree coordination. An advisory reservation is not OS-enforced access control or a distributed transaction. Avoid building a universal lock service. |
| 5 | **Delivery diagnostics and event subscriptions** | Existing receipt/capability foundation; missing unified user-facing workflow. | Non-consuming inspection, queue/wake/read/ack reasons, and cursor-based waits for external consumers. Preserve seven-day exit-body policy and metadata-only audit by default. A timer expiring is not proof a harness is dead or deaf. |
| 6 | **Reviewable result artifacts** | Extends, rather than duplicates, planned Git/CI metadata. | Bind a result to worktree, exact base/head, diff/test/log/demo artifacts, reviewer decision and accepted revision. A successful tool/turn must not automatically mark a task accepted or authorize merging. |
| 7 | **Execution profiles with explicit isolation/credentials** | New substantial backend/execution scope. | Reference a maintained external container/VM/sandbox runtime, mounted paths, network policy and approved credential path. Keep normal trusted local launches unchanged. Worktrees, private HOME and a remote machine are not by themselves security isolation. |

A sensible initial discussion order after current multi-session work is: local cross-session communication already on the roadmap; coherent diagnostics and task/reply references; then attention/result workflows. This does not reinstate deferred CPU optimization or prioritize finishing-level forge enrichment over usable agent workflows.

## Scope boundaries and separate proposals

### Explicit non-goal: ACP hosting or a replacement harness interface

Bkpr remains terminal-first even when web and mobile clients are implemented. Harnesses own their interfaces; their bespoke tools, interactions and presentation are part of what Bkpr must preserve, not flatten into a common chat protocol.

Rich integration stays per harness: native hooks, plugins, extensions and supported APIs can supply attention, diagnostics, artifacts and control without replacing the original interface. Capability differences should remain explicit rather than constrain every harness to a lowest-common-denominator surface.

ACP-based structured harness hosting and a Bkpr-owned replacement chat interface are therefore rejected directions, not deferred experiments. MCP remains the shared tool/control surface; this decision does not remove native hooks, plugins or harness-specific APIs.

### Mobile attention and authenticated observers

Hangar, Paseo, Belfry and Beezy show demand for answering agents from another device. Bkpr's independent client model is compatible with that direction, but mobile is still deferred. The narrow useful first client would show trusted attention/status/artifact data and allow explicitly scoped actions. Observation, steering, terminal input, clipboard and approval capabilities should be distinct. Do not casually add a public unauthenticated relay or assume a secret link is a low-authority viewer.

### Long-lived agents and scheduled work

Spool, Quarterdeck, Optio and personal-assistant systems illustrate work that persists while its process sleeps, with event/cron triggers and next-turn ownership. A Bkpr launch/inbox is currently launch-scoped; it must not silently become a persistent named “person” that inherits a retired launch's mailbox. Durable task identity and explicitly assigned new workers are the safer seam. Loop budgets, trigger-chain limits and human escalation belong in the policy layer.

### Shared artifacts and editor/notebook clients

Artifact references can precede storage infrastructure: producer, owning endpoint, URI/path, content/commit identity, intended consumer, access and retention. That would let an editor, notebook, artifact viewer or Filestash installation interact with Bkpr without making the daemon an IDE, sync engine or document store. URI/path access must still be explicit; merely linking a remote artifact must not copy its secrets to every attached client.

## Implementation and reuse boundaries

- **Own Bkpr's distinctive control facts:** process/launch/workspace identity, lifetime, authority, exact request outcomes, native harness binding, terminal correctness and efficient rendering.
- **Integrate generic policy/data layers:** task ledgers, issue trackers, sandbox runtimes, artifact stores and optional notification clients. External task engines should consume one typed Bkpr API, not clone its process engine.
- **Keep provenance/capabilities explicit:** transcript reading, wake, input, approvals, usage, artifacts and recovery are independent capabilities. An installed plugin, bridge registration or process title does not prove all of them work.
- **Do not create competing authorities:** no second mux state, message broker or task database just because another product embeds one. Where an adapter crosses two systems, name the authoritative owner and failure/reconciliation contract.
- **Preserve performance:** update attention/metadata off the input/render path; subscribe/coalesce instead of rescanning transcript files or providers on every frame; bound per-endpoint buffers and background work. Unknown or stale data must be labeled, not silently treated as current.
- **Treat plugin manifests as disclosure unless confinement exists:** many surveyed extension systems run arbitrary user-level code. Per-tool approval strings do not isolate an installed extension.
- **Review licenses at the intended integration boundary:** Hangar/Remuda and many harnesses are MIT; Codealong/Herdr/Paseo are Apache-2.0; jail is BSD-3-Clause. Juggler's app and FDrive are AGPL, SpurCode is GPL-3.0-only, Quarterdeck GPL-3.0-or-later; licenses may differ for SDKs/assets. Pendant/SpurLab licensing was not established here. Architectural learning does not require importing implementation code; embedding/distribution needs a current license check.

## Concrete follow-up experiments before adopting anything

1. **Adapter usability:** ask an external task tool to create one Bkpr worker, receive a correlated result and record a review decision without PTY injection or a second process registry.
2. **Attention lifecycle:** exercise one native question/approval from creation through answer, cancellation, agent exit and reconnect. Prove stale UI cannot authorize the wrong request; show an honest unsupported state on a harness lacking the capability.
3. **Handoff fidelity:** transfer an unfinished task to a fresh different harness using a bounded artifact; verify exact worktree/commit, constraints, unresolved work and evidence survive without importing provider-private transcripts or credentials.
4. **Subscription recovery:** interrupt a diagnostic subscriber between event delivery and cursor persistence; show replay/dedup or explicit snapshot repair, never a lost event claimed as processed.
5. **Isolation proof:** if sandbox work is approved, test host-file, network and credential boundaries independently. A successful Docker launch or a valid image name is not acceptance.
6. **Failure economics:** compare human interruptions, review corrections, abandoned/stalled tasks, whole-system resource use and time-to-accepted-result, not agent count or tokens generated. No performance/quality ranking of these projects is established by this survey.

No source changes to Bkpr's runtime or active remote feature were made for this report. No external application was executed or installer/configuration change performed. A subsequent explicit user decision added the five accepted workflow items to `TODO.md` and recorded terminal-first, harness-native integration with ACP hosting as a non-goal.
