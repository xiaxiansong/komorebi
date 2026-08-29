# Window / Container / Workspace Model Implementation Plan

This document is the source of truth for the multi-turn implementation of the managed-window,
container-slot, per-monitor workspace, and AutoHotkey workflow described in the project task. Read
and update it at the beginning and end of every implementation turn. A phase is not complete until
its focused tests and the workspace compile check pass and the phase has its own commit.

## Scope and non-negotiable constraints

- Core ownership, lifecycle, geometry, merge, focus-history, minimize-history, and floating state
  live in the Rust window manager. AutoHotkey is only a `komorebic` command launcher.
- Ignored, temporarily unmanaged, and managed-floating windows are distinct states and code paths.
- A managed window belongs to exactly one container. There will be no workspace-owned floating
  window list in the completed model.
- Stable IDs, not list indices, are persistent identity. Indices remain transient UI/order inputs.
- Slot algorithms operate on gap-free logical rectangles. Padding and gaps are applied only when
  producing render rectangles.
- Mutating compound operations validate their complete input and geometry before committing state.
- Existing user changes are preserved. Each phase should normally change about 150-450 handwritten
  lines; generated schema changes are counted separately. If a phase would exceed that range, split
  it before coding.
- Do not add polling for window state and do not create a whkd configuration.

## Repository baseline

Recorded on 2026-08-29 (Asia/Shanghai), before task changes:

| Item | Baseline |
| --- | --- |
| Branch | `master` |
| Commit | `3348a95b38e1f7055cc9636688b57d7a9751684a` |
| Describe | `nightly-1-g3348a95b` |
| Commit subject | `删除readme` |
| komorebi / komorebic version | `0.1.42` |
| Worktree | clean |
| Repository instructions | no `AGENTS.md`; root README is absent at this commit |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)` |
| Cargo | `cargo 1.98.0 (797e8a9bc 2026-08-05)` |
| rustup | unavailable (`rustup` is not installed/on PATH) |
| Clippy | unavailable (`cargo clippy`: no such command) |
| Format check | unavailable: installed rustfmt is a deprecated pre-2018 version; it rejects `crate::` and raw identifiers and does not support `--check` |
| `cargo check --workspace` | passed; existing future-incompatibility warning for `net2 v0.2.39` |
| `cargo test --workspace --no-run` | passed; existing MSVC linker informational warnings |
| `cargo test --workspace` | passed: komorebi 98 passed/1 ignored; layouts 128 passed; bar 3 passed; remaining targets/doc-tests passed with zero tests |

The Clippy limitation is an installed-toolchain mismatch, not a claim that Windows cannot run
Clippy. If a rustup-managed toolchain is later installed, run `rustup component add clippy` for the
repository's stable toolchain and then `cargo clippy --workspace --all-targets`.

## Current architecture findings

- `Container` already has a generated stable string ID, but owns `Ring<Window>` directly and has no
  independent state, slot, or window MRU.
- `Workspace` is ordered inside each `Monitor`, but has no stable ID. It currently owns separate
  tiled containers, `floating_windows`, `maximized_window`, and `monocle_container`; these alternate
  ownership paths are the main migration hazard.
- Container geometry is currently positional (`latest_layout` and parallel `resize_dimensions`),
  keyed by container index. It must move to an ID-keyed logical slot map.
- Ring focus indices currently stand in for focus history. Explicit MRU lists are required.
- `UnmanageFocusedWindow` currently emits the same removal event path used by ordinary lifecycle
  removal and has no suppression set, so a later Show event can re-manage the HWND.
- `ManageFocusedWindow` currently sends a force-manage event. Resume-after-temporary-unmanage must
  become a distinct command and must still respect ignore rules.
- `SocketMessage` is shared through `komorebi-client`; command handling is in `process_command.rs`,
  CLI parsing is in `komorebic/src/main.rs`, and events are coordinated in `process_event.rs`.
- Runtime state output is assembled in `state.rs`; static configuration conversion and defaults are
  concentrated in `static_config.rs` and `core/mod.rs`.
- Win32 create/show/hide/minimize/destroy/focus coordination is event-driven through
  `winevent_listener`, `WindowManagerEvent`, and `process_event`; this infrastructure will be reused.

## Planned model

New core types will be introduced without reusing the existing floating-window placement enum:

- `WorkspaceId` and `ContainerId`: serde-transparent stable newtypes.
- `ManagedWindow`: `Window`, owning `ContainerId`, `ManagedPlacement`, `Visibility`, `Presentation`,
  optional floating rectangle, and optional restore rectangle.
- `ContainerState`: derived `Active` or `Hidden`.
- `LogicalRect`: gap-free slot geometry, distinct by type/field from final `Rect` rendering.
- `HiddenSlotRestore`: old rectangle, absorption direction and participants, their prior rectangles,
  generation, validity, and a center anchor for fallback placement.
- Workspace-owned `HashMap<ContainerId, LogicalRect>`, container/window MRUs, minimize MRU, and a
  monotonically increasing geometry generation.

The existing user-facing floating placement policy (`None`, `Center`, `CenterAndResize`) will be
renamed or kept as a separately named policy type so it cannot be confused with managed window
placement (`Stored`, `Floating`).

## Phase plan and commit boundaries

Every checkbox is updated only after the named verification succeeds. Because a commit cannot embed
its own final hash, the previous phase's hash is appended when the next phase starts.

### Phase 0 - Baseline and plan

- [x] Inspect contribution guidance, docs, manifests, core types, event path, command path, tests.
- [x] Record commit, branch, version, worktree, toolchain, Clippy, format, build, and test baseline.
- [x] Commit this plan as `docs: plan managed window model migration`.

Commit: `987475d3`.

Expected files: this document only.

### Phase 1 - Temporary-unmanage classification and event suppression

- [x] Add runtime-only `temporarily_unmanaged_hwnds` ownership to `WindowManager`.
- [x] Separate temporary suspend/resume operations from force-manage semantics at the core method
  and event boundary.
- [x] On suspend, remove the HWND from every current ownership path and normal indexes; destroy an
  emptied container through the existing local lifecycle and retile path.
- [x] Ignore ordinary show/uncloak/name/focus/move events for suspended HWNDs.
- [x] Clear the suppression entry on destroy so normal HWND reuse is not suppressed.
- [x] Resume by removing suppression first, rejecting ignored windows, and processing the HWND
  through the new-window path without restoring former ownership. Initial visibility/presentation
  capture is intentionally completed with the multidimensional state in Phase 2.
- [x] Add classification and idempotency unit tests.
- [x] Run focused tests, `cargo check --workspace`, and `cargo test --workspace`.
- [x] Commit as `feat: separate temporary window suspension`.

Expected handwritten change: 250-450 lines. Likely files: `window_manager.rs`, `process_event.rs`,
`window_manager_event.rs`, `workspace.rs`, plus focused tests. The public socket/CLI spellings may be
added here if needed for end-to-end testability; otherwise they are finalized in Phase 12.

Actual files: `process_event.rs`, `static_config.rs`, `window.rs`, `window_manager.rs`,
`window_manager_event.rs`, and `workspace.rs`. Actual Rust diff before this plan update: 427 added,
19 removed lines. `cargo test -p komorebi --lib`: 106 passed, 1 pre-existing ignored. Full workspace
check and test passed; layouts remained 128/128 and bar remained 3/3. Existing linker messages and
the `net2` future-incompatibility warning are unchanged. Format/Clippy remain unavailable for the
baseline toolchain reasons above.

### Phase 2A - Managed window state types and transitions

- [x] Add `ManagedWindow`, `ManagedPlacement`, `Visibility`, and `Presentation` with serde defaults.
- [x] Derive initial visibility and presentation from Win32 queries while keeping maximized and
  fullscreen classification distinct.
- [x] Keep maximize, fullscreen, minimize, and placement independent in pure transition methods.
- [x] Accept both the new managed-window JSON shape and legacy serialized `Window` objects.
- [x] Add serialization/backward-compatibility and transition tests.
- [x] Commit as `feat: add managed window state transitions`.

Expected handwritten change: 250-400 lines. Likely files: new `managed_window.rs`, `window.rs`,
`windows_api.rs`, `lib.rs`.

Actual files: new `managed_window.rs`, `windows_api.rs`, and `lib.rs`. Actual Rust change: 397
added lines. Nine focused state/serde tests passed and `cargo check --workspace` passed. A normal
`cargo test --workspace` ran all new tests successfully but two pre-existing monitor movement tests
failed because this desktop session's `GetForegroundWindow()` returned null (`ERROR_INVALID_PARAMETER
(87)` at `WindowsApi::foreground_window`); both fail identically when run alone and serially. The
pre-change full-suite run in the same turn passed before the desktop lost its foreground window.
Running the workspace suite with exactly those two environment-dependent tests skipped passed:
komorebi 113 passed/1 ignored/2 filtered, layouts 128 passed, bar 3 passed, all other targets and
doc-tests passed. No production workaround was mixed into this phase; the foreground dependency is
recorded for the workspace migration/event reconciliation phases. Format/Clippy remain unavailable
for the baseline toolchain reasons above. Two normal and one PTY-backed signed commit attempts
failed in `op-ssh-sign` with `1Password: failed to fill whole buffer`; the phase commit was therefore
created unsigned with a one-command `commit.gpgsign=false` override, without changing repository or
global Git configuration. It can be replaced by a signed amend after 1Password signing recovers.

### Phase 2B - Container managed-window storage migration

- [x] Convert container storage from `Window` to `ManagedWindow` while preserving convenient Win32
  accessors and legacy container deserialization.
- [x] Assign and update the owning container ID on every add, move, stack, and removal path.
- [x] Capture initial state when a new or resumed HWND first enters a container.
- [x] Preserve multidimensional state across container-to-container stack/split operations. The
  legacy workspace-owned floating/maximized paths still explicitly unwrap to `Window`; routing
  those transitions without discarding state moves to Phase 5, where alternate ownership is
  removed atomically rather than temporarily giving a detached window a stale container ID.
- [x] Add container ownership, legacy/current serde, state-preserving move, and capture-path tests.
- [x] Commit as `feat: store managed window state in containers`.

Expected handwritten change: 300-500 lines. Likely files: `container.rs`, `workspace.rs`,
`window_manager.rs`, `process_event.rs`, `windows_callbacks.rs`, `state.rs`.

Actual files: `container.rs`, `workspace.rs`, `window_manager.rs`, `windows_callbacks.rs`,
`stackbar_manager/stackbar.rs`, and `komorebi-bar/src/widgets/komorebi.rs`. Actual source/test diff
before this plan update: 276 added, 26 removed lines. `Container` now serializes
`Ring<ManagedWindow>`, repairs legacy or stale owner IDs on deserialize, captures observed Win32
state for raw-window insertion, and preserves state while reassigning ownership for stack/split
operations. Focused `Window` compatibility accessors limited unrelated churn. `stack_all` now
rewrites every window to the new container ID instead of copying stale owners. Focused tests passed;
`cargo check --workspace` passed; full workspace tests passed (komorebi 119 passed/1 ignored,
layouts 128 passed, bar 3 passed, all remaining targets/doc-tests passed). Format/Clippy remain
unavailable for the recorded toolchain reasons.

### Phase 3A - Stable workspace and container identity

- [x] Add typed stable `WorkspaceId`/`ContainerId`; migrate the existing container string ID.
- [x] Preserve IDs through ordering, cloning, state output, and ownership changes.
- [x] Accept legacy workspace JSON without an ID and existing string container IDs.
- [x] Add stable-ID serde and ordering tests.
- [x] Commit as `feat: add stable workspace and container identities`.

Expected handwritten change: 200-350 lines. Likely files: new `model.rs`, `lib.rs`,
`managed_window.rs`, `container.rs`, `workspace.rs`, `state.rs`, and `stackbar_manager/mod.rs`.

Actual files: new `model.rs`, plus `lib.rs`, `managed_window.rs`, `container.rs`, `workspace.rs`,
`state.rs`, `border_manager/mod.rs`, `stackbar_manager/mod.rs`, and test-only cache keys in
`monitor_reconciliator/mod.rs`. Actual source/test change before this plan update: 93 lines in the
new ID module plus 79 added and 42 removed lines elsewhere. Transparent serde retains the existing
JSON string shape for container IDs; legacy workspaces without an ID receive a new stable ID.
`cargo check --workspace` and the schemars feature check passed. The serial full workspace suite
passed: komorebi 123 passed/1 ignored, layouts 128 passed, bar 3 passed, and all other targets and
doc-tests passed. Parallel runs exposed pre-existing shared-global test isolation in the monitor
cache/channel tests; unique cache keys were added, while the channel tests remain reliably covered
by the passing serial run. Format/Clippy remain unavailable for the recorded toolchain reasons.

### Phase 3B - Focus histories and ownership invariants

- [ ] Add workspace container MRU, container window MRU, and per-workspace minimize MRU.
- [ ] Centralize record, selection, deduplication, and deletion cleanup.
- [ ] Add `validate_invariants()` ownership/history checks enabled by tests and debug assertions.
- [ ] Add focus, deletion, minimize-history, and invariant tests.
- [ ] Commit as `feat: add focus histories and ownership invariants`.

Expected handwritten change: 300-500 lines. Likely files: `container.rs`, `workspace.rs`,
`monitor.rs`, `window_manager.rs`, `process_event.rs`, and `state.rs`.

### Phase 4 - Logical slots and render rectangles

- [ ] Add ID-keyed logical slots as workspace geometry authority.
- [ ] Move adjacency, swap, resize, split, and coverage validation to logical rectangles.
- [ ] Apply workspace padding/container gaps/borders only in render conversion.
- [ ] Preserve integer-pixel coverage; odd 50:50 remainder belongs to the old container.
- [ ] Add gap independence, adjacency, split, overlap, and full-coverage tests.
- [ ] Commit as `feat: separate logical slots from window rendering`.

Expected handwritten change: 350-500 lines. Likely files: new `geometry.rs`, `workspace.rs`,
`container.rs`, `set_window_position.rs`, `komorebi-layouts` only if a generic helper truly belongs
there.

### Phase 5 - Derived Active/Hidden container state

- [ ] Add derived `ContainerState` and active-container selectors.
- [ ] Make only containers with a visible stored window occupy a logical slot.
- [ ] Migrate floating windows from the workspace list into their owning containers.
- [ ] Remove alternate ownership through maximized/monocle storage; presentation becomes window state.
- [ ] Route minimize/restore, maximize/fullscreen, and stored/floating operations through the
  multidimensional transition methods once windows no longer leave their owning container.
- [ ] Make state transitions idempotent and extend invariant validation.
- [ ] Add all basic Hidden classification and ownership tests.
- [ ] Commit as `feat: derive active and hidden container state`.

Expected handwritten change: 350-500 lines. Likely files: `managed_window.rs`, `container.rs`,
`workspace.rs`, `window_manager.rs`, `process_event.rs`, `state.rs`.

### Phase 6 - Hidden slot absorption and restoration

- [ ] Implement complete-edge neighbor group selection in left/right/up/down order.
- [ ] Implement local absorption plus `HiddenSlotRestore` snapshots and geometry generations.
- [ ] Implement exact reverse restoration with existence, geometry, generation, and min-size checks.
- [ ] Invalidate restores on all named topology/geometry operations and full-relayout fallback.
- [ ] Add single/multiple neighbor, only-active, consecutive hide/restore, exact/fallback tests.
- [ ] Commit as `feat: restore hidden container slots safely`.

Expected handwritten change: 350-500 lines. Likely files: `geometry.rs`, `container.rs`,
`workspace.rs`, `window_manager.rs`.

### Phase 7 - New-window threshold placement and manual split

- [ ] Implement active-count N=0, N<=2, and N>2 allocation rules.
- [ ] Implement deterministic neighbor selection and diagnostic fallback.
- [ ] Add atomic auto/horizontal/vertical manual container creation from an eligible donor.
- [ ] Route donor/recipient state changes through the Hidden transition engine.
- [ ] Add N=0/1/2/3, long-edge split, odd-pixel, neighbor-order, and atomic-failure tests.
- [ ] Commit as `feat: add threshold based container allocation`.

Expected handwritten change: 300-500 lines. Likely files: `geometry.rs`, `workspace.rs`,
`window_manager.rs`, `process_event.rs`.

### Phase 8 - Container deletion, distribution, and multi-neighbor resize

- [ ] Reuse complete-edge groups for Active deletion expansion.
- [ ] Implement Hidden explicit deletion recipient order and atomic refusal.
- [ ] Distribute top-to-bottom source windows round-robin to recipient bottoms.
- [ ] Implement shared-edge resize with multi-container opposite sides and clamped delta.
- [ ] Invalidate impacted hidden restores; select post-delete focus from expansion recipients.
- [ ] Add deletion/distribution/focus/resize/failure-rollback tests.
- [ ] Commit as `feat: make container deletion and resize topology safe`.

Expected handwritten change: 350-500 lines. Likely files: `geometry.rs`, `workspace.rs`,
`window_manager.rs`.

### Phase 9 - Floating move and edge resize

- [ ] Add DPI-aware move and independent edge-resize core operations.
- [ ] Validate visible + floating + normal state and return typed success/no-op reasons.
- [ ] Clamp movement to a draggable visible area and sizing to Win32/system minimums.
- [ ] Read back the accepted Win32 rectangle after a resize and store it.
- [ ] Add defaulted `floating_move_delta` and `floating_resize_delta` configuration.
- [ ] Add isolated-geometry, state rejection, clamp, DPI, and Hidden-container tests.
- [ ] Commit as `feat: add independent floating window geometry commands`.

Expected handwritten change: 300-500 lines. Likely files: `core/mod.rs`, `static_config.rs`,
`managed_window.rs`, `windows_api.rs`, `window_manager.rs`, `process_command.rs`.

### Phase 10 - Workspace ordering, deletion, merge, and minimized restore

- [ ] Implement stable-ID reorder/swap APIs without changing names or rules.
- [ ] Implement delete-direction selection and atomic source-to-target merge.
- [ ] Merge all containers and histories, preserve states, invalidate hidden exact restores, relayout
  only Active containers, and inherit source focus.
- [ ] Implement current-workspace last-minimized restore through state transitions and MRUs.
- [ ] Add only-workspace refusal, first/middle/last merge, history, focus, and rollback tests.
- [ ] Commit as `feat: merge and reorder stable workspaces`.

Expected handwritten change: 350-500 lines. Likely files: `monitor.rs`, `workspace.rs`,
`window_manager.rs`, `process_command.rs`, `state.rs`.

### Phase 11 - Cross-monitor container/workspace migration

- [ ] Move/swap complete containers while recomputing target slots and DPI render geometry.
- [ ] Translate and clamp floating rectangles between monitor work areas.
- [ ] Move workspaces without leaving a monitor empty; retain workspace ID/name.
- [ ] Preserve Hidden state without creating active slots and implement explicit focus-follow rules.
- [ ] Add mixed-DPI, empty/occupied target, Hidden, atomic-failure, and focus tests.
- [ ] Commit as `feat: preserve model across monitor migrations`.

Expected handwritten change: 300-500 lines. Likely files: `monitor.rs`, `workspace.rs`,
`window_manager.rs`, `monitor_reconciliator/mod.rs`, `windows_api.rs`.

### Phase 12 - Socket protocol and komorebic CLI

- [ ] Finalize distinct commands for global pause, suspend/resume manage, placement, floating move and
  resize, active resize, maximize, fullscreen, minimize/restore, container lifecycle, stable-ID
  transfers, workspace ordering/merge, and monitor transfers.
- [ ] Add typed command outcomes: success, no-op, non-floating, minimized, no target, ignored,
  suspended, and invariant failure. Preserve compatibility for existing commands where practical.
- [ ] Add CLI parsing/serialization tests and generated command docs/schema updates.
- [ ] Commit as `feat: expose managed model commands in komorebic`.

Expected handwritten change: 350-500 lines plus generated docs/schema. Likely files:
`core/mod.rs`, `process_command.rs`, `komorebic/src/main.rs`, `komorebi-client/src/lib.rs`, `docs/cli`,
`schema.json`, `schema.asc.json`.

### Phase 13 - AutoHotkey v2 workflow

- [ ] Add a directly runnable AHK v2 example using top-level executable/config/delta variables and
  helper functions around `Run`/`RunWait`.
- [ ] Cover every shortcut group in the task with Chinese comments; never emit whkd config.
- [ ] Use safe stop/start restart and the version-correct static configuration replacement command.
- [ ] Prefer existing `komorebic gui`; otherwise add a small AHK v2 shortcut panel.
- [ ] Validate generated command lines against `komorebic --help`.
- [ ] Commit as `docs: add complete AutoHotkey v2 workflow`.

Expected handwritten change: 200-400 lines. Likely files: new `docs/common-workflows/komorebi-model.ahk`,
`docs/common-workflows/autohotkey.md`, possibly `mkdocs.yml`.

### Phase 14 - Event reconciliation, serialization, documentation, and final verification

- [ ] Audit create/show/hide/destroy/minimize/restore/maximize/fullscreen, HWND reuse/crash, monitor
  hotplug, DPI/work-area change, workspace-switch races, and suspended HWND events.
- [ ] Make duplicate and out-of-order transitions converge idempotently.
- [ ] Complete state output fields and migration/version policy with serde defaults.
- [ ] Add randomized operation/property tests if current dependencies permit it without an outsized
  dependency; otherwise add a deterministic seeded operation harness.
- [ ] Map all 16 invariants to implementation and tests in final documentation.
- [ ] Regenerate schemas/docs and run all available checks.
- [ ] Commit as `feat: validate and document managed window model`.

Expected handwritten change: 350-500 lines plus generated artifacts. Likely files:
`process_event.rs`, `window_manager_event.rs`, `monitor_reconciliator/mod.rs`, `workspace.rs`,
`window_manager.rs`, `state.rs`, `static_config.rs`, tests, and docs.

## Provisional affected-file inventory

This list is updated from actual diffs, not treated as permission to change every file.

- Core/model: `komorebi/src/container.rs`, `workspace.rs`, `monitor.rs`, `window_manager.rs`,
  `window.rs`, `ring.rs`, new `managed_window.rs`, new `model.rs`, new `geometry.rs`.
- Win32/event flow: `windows_api.rs`, `window_manager_event.rs`, `winevent_listener.rs`,
  `process_event.rs`, `monitor_reconciliator/mod.rs`, `reaper.rs`, `set_window_position.rs`.
- Command/config/state: `core/mod.rs`, `process_command.rs`, `static_config.rs`, `state.rs`, `lib.rs`.
- Client/CLI: `komorebi-client/src/lib.rs`, `komorebic/src/main.rs`.
- Tests: colocated unit tests first; integration/property harness only when it reduces Win32 coupling.
- Docs/examples/schema: `docs/cli/`, `docs/common-workflows/`, `docs/design.md`, `mkdocs.yml`,
  `schema.json`, `schema.asc.json`.

## Verification policy for every phase

1. Re-read this plan and inspect the current worktree before editing.
2. Run focused unit tests for the changed module(s).
3. Run `cargo check --workspace`.
4. Run `cargo test --workspace` unless the phase is documentation-only.
5. Review `git diff --check`, `git diff --stat`, and the complete phase diff.
6. Because the installed rustfmt is incompatible, format touched Rust using a compatible formatter if
   one becomes available; otherwise follow existing style manually and record the limitation.
7. Run Clippy if a compatible component becomes available; never report it as passed otherwise.
8. Update phase status, actual files, test counts/results, decisions, and commit hash in this plan.
9. Commit only the phase's intended files. Re-check the worktree after committing.

## Decision and risk log

- 2026-08-29: Keep the existing event-driven Win32 architecture. No polling fallback is planned.
- 2026-08-29: Treat the current workspace-level floating/maximized/monocle ownership as transitional
  debt. It cannot remain in the completed ownership model.
- 2026-08-29: Introduce a new name for managed `Stored/Floating` placement because `core::Placement`
  already means center/resize policy.
- 2026-08-29: Prefer typed IDs in core APIs while retaining index-based compatibility commands until
  the CLI migration phase.
- 2026-08-29: The biggest merge-conflict areas with upstream will be `workspace.rs`,
  `window_manager.rs`, `process_event.rs`, `process_command.rs`, `core/mod.rs`, and static config/state
  serialization.
- Open: confirm whether changing socket command replies is compatible with the existing one-way
  client transport; if not, expose command outcomes through query/notification without breaking old
  callers.
- Open: determine whether fullscreen can be reliably distinguished from borderless maximize for all
  target applications with existing Win32 helpers; application-specific exceptions may be needed.
- Open: `ApplyState` must be made transactionally aware of the runtime suspension set when state
  migration is implemented; ordinary Win32 reconciliation is already suppressed in Phase 1.
- Open: `Monitor::move_container_to_workspace` currently requires a non-null foreground HWND even
  when moving an empty test container. Its two unit tests fail with Win32 error 87 whenever the
  desktop session has no foreground window; remove that incidental dependency in the workspace
  migration phase rather than hiding the production behavior in a state-model commit.

## Progress log

- 2026-08-29: Phase 0 baseline captured. No source changes existed at start. Full workspace tests
  passed; formatter and Clippy limitations recorded above. Next phase: temporary-unmanage
  classification and event suppression.
- 2026-08-29: Phase 1 implemented runtime suspension, no-side-effect detach, event suppression,
  destroy cleanup, ignore-respecting resume, rollback of failed in-memory detach/retile, and focused
  lifecycle tests. Full workspace check and tests passed. Next phase: managed window
  multidimensional state.
- 2026-08-29: Phase 2 was split before coding because converting every `Ring<Window>` container
  caller together with the new state model would exceed the per-phase review limit. Phase 2A owns
  the types, Win32 observation, serde compatibility, and pure transitions; Phase 2B owns the
  container/call-site migration. Pre-2A workspace check and tests passed (komorebi 106 passed/1
  ignored; layouts 128; bar 3). Toolchain limitations for rustup, rustfmt, and Clippy are unchanged.
- 2026-08-29: Phase 2A implemented independent placement, visibility, and presentation state,
  legacy/new serde representations, atomic pure transitions, and conservative Win32 fullscreen
  observation distinct from `IsZoomed`. Focused tests and workspace compile passed. All tests except
  two existing foreground-dependent monitor tests passed; exact failure and filtered verification
  are recorded above. Next phase: migrate container storage and all ownership-changing call sites to
  `ManagedWindow`.
- 2026-08-29: Phase 2B migrated container rings and insertion/removal APIs to `ManagedWindow`,
  repaired ownership during deserialization and every current stack/split insertion, captured
  observed state for new/resumed raw HWNDs, preserved state across container moves, and adapted the
  bar/stackbar readers. The full workspace check and test suite passed. The planned legacy
  float/maximize/minimize transition routing was moved to Phase 5 after inspection showed it cannot
  preserve state until those workspace-owned alternate paths are removed. Next phase: typed stable
  workspace/container identities and explicit focus/minimize histories.
- 2026-08-29: Phase 3 was split into 3A identity and 3B histories/invariants after call-site review
  showed that doing both together would exceed the phase review-size limit. Phase 3A added
  transparent typed workspace/container IDs, migrated managed ownership and UI integration
  boundaries, preserved workspace IDs in state snapshots, and maintained legacy JSON compatibility.
  Compile, schema, focused serial tests, and the full serial workspace suite passed. Next phase:
  explicit focus/minimize histories and ownership/history invariant validation.
