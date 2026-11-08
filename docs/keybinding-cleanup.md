# Keybinding policy and historical cleanup inventory

Status: the sidebar/prefix separation below is implemented policy. The original
audit is retained as a historical inventory, not a current shortcut reference.
Historical baseline: main at `30c467f`, including focus-loss fix `a82aec4`.

## Implemented sidebar/prefix policy

- `Ctrl-Space` is the sidebar entry point. Prefix `b` and `S` are removed.
- The sidebar owns local navigation and selected-entity management: `n` creates
  a workspace, `N` creates a separate terminal window, `r` renames, and
  `x`/Delete closes the selected entity after confirmation. Sidebar `S` selects
  the session-root context; session-row `R` reconnects its attachment.
- Prefix `c`/`N`, `r`/`p` and `!`/`X` are removed; creation, rename and close
  use the sidebar instead. A separate terminal window is not a split.
- The `Ctrl-b` prefix retains global pane/layout/client actions: `w`/`W`/`o`
  choosers, `n`/`%` and `"` splits, workspace/pane navigation, directional
  focus/exchange, resize, search/history, copy and project inspection.
  Prefix `x` removes only a layout reference; its process survives.
- Prefix `R` reopens an unavailable pane as a shell; `g`/`t` control client
  presentation; `d` detaches while leaving programs running. `Ctrl-]` no longer
  detaches and follows ordinary input ownership instead.

The CLI help binding list is synchronized with this policy. A full structural
and readability rework remains an unchecked item in `TODO.md`; this change does
not implement that overhaul or the other proposals recorded below.

## Cutover verification

Incremental debug build, bounded to eight jobs. Workspace unit/integration test
executables pass as the actual host user; all-target strict Clippy and formatting
pass. The headless creation ownership regression now exercises sidebar `n`.
Public debug/release executable paths used by live sessions were preserved.

An isolated real Kitty client/backend smoke verifies application receipt of
`Ctrl-]` as byte `0x1d`, no accidental forms/mutations from removed prefix
commands, sidebar create/rename/confirmed close, session-root `n` creation on
the exact owner, and `Ctrl-b d` detach with programs surviving. Physical root
creation used an existing session; empty-headless routing is covered by the
ownership regression, not claimed as separate terminal acceptance.
Evidence and cleanup receipts: `target/smoke/keybinding-cleanup/`,
especially `final-actual-kitty-ready/result.json` and `build/host-tests.json`.


## Historical audit boundary

Everything from “Scope and evidence” through the original acceptance plan below
describes the baseline audit and its proposals. Removed chords in those tables
are historical evidence only. Source line numbers and the executed CLI help
comparison refer to that baseline, not fresh verification of this change.

## Scope and evidence

We should preserve familiar bindings first, make effective ownership and hints agree, then decide any ergonomic changes explicitly. This is not a command-palette implementation or a new configuration system.

Unqualified Rust source paths below are under `crates/bkpr-client/src/`. Framework widget paths are under `crates/bkpr-tui-framework/src/`.

Historical source inspected: `pre-native-rewrite:crates/bkpr-zellij/src/keyguide.rs` at `e9ee40261bf25b929799d231602573818a0af243`. Its useful principle was to describe effective merged bindings and suppress local hints intercepted by native actions. Keep that principle, not Zellij's mode plumbing or its old shortcut assignments. No historical code was copied.

At the baseline, the inventory followed `crates/bkpr-client/src/keyguide.rs` (`Context`, `Action`, `BINDINGS`, `lookup_with`, `hints_with`), actual `app.rs` dispatch, the session coordinator, and the context-specific modules cited below. The then-current debug CLI's `--help` was executed and compared with source. This was a source/dispatch audit plus an executed CLI advertisement check, not physical-terminal acceptance of every chord. No Cargo builds, live-session mutations or executable replacements were performed for that audit.

## Ownership and matching

- `keyguide::ContextState` orders modal, resize, prefix, search, sidebar, history, then application. `App::guide_context` additionally distinguishes a session-root tree selection. `App::key` contains early interceptors, so the context table alone is not the complete dispatch policy.
- Owed key releases are processed against their original owner before normal local-context handling. Focus loss now settles held keys through the original view and generation. Preserve that ordering and the existing late-release fencing.
- Shared physical bindings require exact modifiers after ignoring Caps/Num Lock. Text bindings match produced text and permit Shift, but reject Ctrl/Alt/Super combinations. Prefix commands are single-press; navigation/edit/resize repetition is individually enabled.
- Unknown presses in an owning local context are consumed, not forwarded to the application. Application input otherwise goes through the native encoder. Do not turn an unavailable command into accidental terminal input.
- Guides are capability-filtered; backend availability, read-only state, selected entity and pending/ambiguous forms affect availability. An action being listed below does not imply every target can execute it.

Sources: `keyguide.rs:42-82,355-393,445-565,954-988`; `app.rs:731-881,3400-3830`; `sessions.rs` release-owner routing.

## Historical application and command prefix

`P` below means press `Ctrl-b`, then the listed key, not a simultaneous chord.

| Input | Action | Ownership / effect |
| --- | --- | --- |
| `Ctrl-b` | Enter command prefix | Application/sidebar/session/history/search contexts; resize has its own capture rules |
| `Ctrl-b Ctrl-b` | Send Ctrl-b to application | Explicit press/release, only when writable and no sidebar/search owns input |
| `Ctrl-]` | Detach client | Early App interceptor; does not stop programs |
| `Ctrl-Space` | Toggle sidebar | Application, sidebar, session tree and history |
| `Shift-PgUp` / `Shift-PgDn` | Page terminal history | Application context; alternate-screen history is refused |
| `P Esc` | Cancel prefix | Does not send Escape to application |
| `P b` / `P S` | Enter sidebar / session tree | Also `S` while tree owns input |
| `P c` / `P N` | Create workspace / separate terminal window | Separate terminal is not a split |
| `P n` or `P %` / `P "` | Split side by side / above-below | Existing aliases, not duplicate processes |
| `P r` / `P p` | Rename current workspace / pane | Opens edit form |
| `P w` / `P W` / `P o` | Choose workspace / window / pane | Pane chooser includes hidden/unassigned executions |
| `P [` / `P ]` | Previous / next workspace | Current attachment's native navigation |
| `P Tab` | Next pane | Not a repeated create/navigation macro |
| `P arrows` | Focus directional pane | Application geometry |
| `P Shift-arrows` | Exchange directional panes | Layout mutation, not focus only |
| `P /` / `P s` | Search / history | Distinct query and scrolling modes |
| `P e` / `P y` / `P i` | Resize / copy selection / project details | Copy depends on clipboard policy |
| `P g` / `P t` | Toggle fancy/plain chrome / theme chooser | Client-local presentation |
| `P x` | Remove layout leaf | Process survives; not terminal close |
| `P !` / `P X` | Terminate pane / close workspace | Confirmation; terminates owned programs |
| `P R` | Reopen unavailable pane as a shell | Explicit directory chooser/confirmation, not native conversation resume |
| `P d` | Detach client | Daemon/programs survive |

Sources: `keyguide.rs:450-565`; `app.rs:3511-3516,3671-3830`.

## Historical tree, inspection and terminal modes

| Context | Effective keys | Notes |
| --- | --- | --- |
| Sidebar / session tree | `Esc` return to app; arrows or `j/k` navigate; `PgUp/PgDn` half-page; `Home/g`, `End/G` first/last; left/right/Space fold/unfold/toggle; Enter activate; `S` session context | Targets retain owner attachment/generation. Session activation is not process creation |
| Native sidebar row | `n` create workspace; `N` create terminal; `r` rename selected; `x` or Delete close selected | Pane close terminates, window/workspace close includes their owned terminals; confirmation required |
| Session-root row | `R` force reconnect | Reconnect attachment only; no restart of remote programs. No stop-session action |
| History | Up/down line; PgUp/PgDn with or without Shift page; Home oldest; Esc/Enter/End return live | Enter does not reach shell; alternate screen has no scrollback navigation |
| Search | Type query; Backspace; Ctrl-u clear; Enter next; Shift-Enter previous; Esc close | Append/backspace/clear only, not a full editable field. Match navigation repeats |
| Selection | Eligible left drag; Shift forces selection over app mouse; Esc clears; `P y` copies | Successful selection release also exports according to clipboard policy; gestures stay with original view |
| Resize | Arrows 1 cell; Shift-arrows 5; Enter or Esc finish | Esc stops resizing, not rollback. Accepted layout changes remain accepted |

Sources: `keyguide.rs` sidebar/session/history/search/resize rows; `sidebar.rs`, `tree.rs:360-570`, `sessions.rs:918-1005`, `scrollback.rs:3-112`, `search.rs:145-210`, `selection.rs:123-250,303-543`, `resizing.rs:120-332`.

Pointer behavior is part of the action surface: tree wheel scrolls three rows; a matching press/release activates a tree target; double click also toggles folding; hover is passive. Pane wheel becomes history navigation with Shift, primary-screen search, or appropriate native non-mouse-reporting state. Divider drag changes backend layout; sidebar-boundary drag changes client-local width. Captured gestures, overlays and toasts precede ordinary pane routing. Preserve capture and late-release tombstones during cleanup.

## Historical modals and editing

| Context | Effective keys | Behavior / omissions |
| --- | --- | --- |
| Create | Enter create, Esc cancel, Tab/Shift-Tab switch field; left/right/Home/End/Backspace/Delete/Ctrl-u; printable text | Two fields toggle in either Tab direction; Shift-Tab is not a distinct reverse action |
| Rename/edit | Enter save, Esc cancel; same character editing | Tab is swallowed; no word-wise movement/deletion |
| Destructive confirmation | Enter/y/Y confirm; Esc/n/N cancel | Consequential operations are gated on explicit confirmation |
| Reopen shell | Enter apply, Esc cancel; up/down/Home/End choose directory | Classified as Chooser, not Confirmation; no y/n shortcut |
| Workspace/window/pane chooser | Enter activate, Esc cancel; up/down/Home/End select | Dispatcher can page, but table currently gives choosers no PgUp/PgDn |
| Project details | Enter/Esc close; up/down/PgUp/PgDn/Home/End scroll | Not an editable input field |
| Finder | Enter open, Esc cancel; arrows or Ctrl-p/n select; PgUp/PgDn scroll preview; Tab cycles Input/List/Preview; character editing and Ctrl-u | Page keys affect preview, not result-page selection |
| Theme picker | Enter keep, Esc rollback; arrows/Ctrl-p/n select; PgUp/PgDn page selection; character editing, Ctrl-a/e/f/h/d/u aliases | Uses framework SearchList rather than keyguide dispatch. SearchList also offers Ctrl-b, but App intercepts it as prefix before widget dispatch |
| Clipboard consent | y/Y allow, n/N deny | Esc and Enter do nothing. Owed releases and early detach are separate from this local ownership |

Forms, finder and theme consume unmatched local presses. Do not confuse this with swallowing releases already owed to a pane: App handles those earlier. In forms/pickers `Ctrl-b` starts a special prefix path where only `g` toggles chrome; other prefix commands are swallowed. Clipboard consent precedes that modal prefix path.

The shared guide is hidden while a modal is open (`app.rs:953-968`), despite modal contexts existing in the action table. Users see only modal-local instructions, which are incomplete in several surfaces.

Sources: `app.rs:3530-3657`; `controller.rs:62-72,1800-2058`; `form_fields.rs:34-50,139-157`; `finder.rs:804-855,1080-1083`; `theme_switcher.rs:22-59`; `bkpr-tui-framework/src/search_list.rs:98-153`; `text_input.rs:52-99`.

## Historical lifecycle entrypoints and lifetime distinctions

| Action | Baseline entrypoint | Recovery/lifetime meaning |
| --- | --- | --- |
| Detach client | `Ctrl-]`, `P d` | Leaves programs running |
| Take over attachment | CLI `-d` | Revokes other clients, not programs |
| Force reconnect | Session-root `R` | Replaces transport attachment, not daemon/programs |
| Stop session, keep recovery | CLI `bkpr stop -s NAME` | Stops daemon and programs; preserves layout/CWD and eligible harness recovery metadata |
| Restart stopped session | CLI `bkpr -s NAME` | Current shells or supported native harness resumption; not memory/scrollback checkpoint restoration |
| Resume native conversation | CLI `bkpr resume`, explicit `--pane` | Uses current adapter/configuration and fresh authority; not arbitrary argv replay |
| Remove layout reference | `P x` | Keeps execution alive, possibly hidden |
| Terminate / close selected native entity | `P !`, `P X`, sidebar x/Delete | Removes owned entities; can cascade to session end |

`Action` has no StopSession or ResumeConversation entry. Tree close is native workspace/window/pane management, not session-root shutdown. These are real discovery gaps, not missing backend shutdown/recovery machinery. See `crates/bkpr/src/main.rs:167-178`, `recovery.rs`, `app.rs:731-811,2917-3170`, `client_lifetime.rs:673-770`.

## Historical findings and proposed decisions

| Priority | Baseline finding | Proposed cleanup at audit time |
| --- | --- | --- |
| A | Shared physical matching is exact, but App intercepts Ctrl-b and Ctrl-] using `modifiers & CTRL != 0`; Ctrl-] also accepts repeats. Thus extra-modifier chords can be captured despite not matching the guide table | Make dispatch and guide use the same binding eligibility, with explicit exceptions for lifecycle ownership. Confirm policy for extra-modifier chords before changing interception |
| A | Theme dispatch has a second framework keymap, including aliases absent from shared hints; Ctrl-b in that widget is shadowed by App | Keep a single inspectable source per effective action surface. Reuse or expose widget editing actions through the existing vocabulary rather than maintaining copied maps; don't advertise unreachable Ctrl-b editing |
| A | Shared guide is not displayed in modals; confirmation/details/chooser/theme copy omits working keys | Render complete context-sensitive instructions from existing action metadata, preferably inside the modal. Do not add a second hand-maintained help registry |
| A | Executed CLI help omits prefix N/W/g/t and Ctrl-Space; history copy omits Enter/End live-return aliases | Synchronize user-facing help with effective actions. This is documentation/discovery work, not a new keymap |
| A | No stop-session action is exposed in the TUI | Add an owner-qualified session-row action labeled "Stop session, keep recovery", with explicit program-termination confirmation. Candidate: unbound `s` on a session root only. Binding and confirmation wording require approval; it must not become a global destructive shortcut |
| B | x means nonterminating layout removal in prefix, but terminating close in native sidebar | Retain existing keys initially; state effects and exact target in hints/confirmation. Any remap needs a separate decision |
| B | R means reconnect at session root but reopen shell after prefix; r renames selected entity vs current workspace | Keep contextual reuse, but label reconnect attachment, reopen shell and rename target precisely |
| B | Esc does nothing in clipboard consent, unlike other modal cancellation | Propose Esc = deny, never allow; preserve original offer/pane binding. Add only after agreement |
| B | Chooser page actions are implemented in dispatch but not bound | Propose additive PgUp/PgDn for long chooser lists. Keep Home/End and one-shot activation |
| B | Search editing is narrower than other query fields; forms also lack word editing | Decide a shared text-editing contract before adding per-context aliases. Cursor movement in search needs a real editor, not fake bindings |
| C | Tab and Shift-Tab have the same two-field toggle | Call it "switch field" now. Introduce directional field cycling only if more fields or UX needs justify it |

No recommendation to remove n/% split aliases, g/G tree navigation, or existing history paging aliases. No blanket Ctrl-c/Ctrl-d takeover: applications and permission prompts have distinct semantics. No configuration format or command palette is needed to complete this cleanup.

## Original proposed implementation order

1. Agree modifier interception and lifecycle terminology. Keep existing familiar chords unless explicitly approved otherwise.
2. Align effective dispatch, capability gating and hint extraction. Preserve release-before-modal ownership, pointer capture, repeats and pending mutation semantics. Reuse the existing Action/Context framework and maintained text widgets.
3. Complete modal and CLI instructions without remapping keys.
4. Add confirmed StopSession presentation through the existing backend Shutdown path. For multiple local/remote attachments, show the exact owner and preserve unrelated sessions. Do not claim arbitrary-program checkpoint recovery.
5. Apply approved additive bindings (consent Esc denial, chooser paging) and optional shared search-editor improvements.

## Original acceptance plan (not a completion claim)

Exercise actual Kitty enhanced input and a legacy terminal with QWERTY/AZERTY layouts: produced punctuation, non-ASCII text, Ctrl/Shift/Alt combinations, Caps/Num Lock, repeat and release. Check every local context's ownership, unknown keys, read-only/offline targets, modal close/rollback, unavailable operations and narrow-window hints. Include focus loss, held-key settlement, late real releases, cross-pane and multi-session generation routing. Do not replace the existing focus-loss regression with a binding-table test.

Stop-session acceptance must prove confirmed shutdown, retained recovery records, no surviving owned daemon/programs, clean attached-client retirement and untouched unrelated sessions. Cancellation must make no mutation. An unknown dispatched outcome must be inspected, not replayed.

The historical inventory did not authorize remapping. The implemented policy at the top records the subsequently approved removal and sidebar/prefix separation. Modifier matching, stop-session presentation, consent denial, chooser paging and shared editing improvements remain separate proposals; none is implied complete by this binding change.
