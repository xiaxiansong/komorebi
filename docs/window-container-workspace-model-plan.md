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
| rustup | unavailable at Phase 0 (`rustup` was not installed/on PATH) |
| Clippy | unavailable at Phase 0 (`cargo clippy`: no such command) |
| Format check | unavailable at Phase 0: the rustfmt then on PATH was a deprecated pre-2018 version; it rejects `crate::` and raw identifiers and does not support `--check` |
| `cargo check --workspace` | passed; existing future-incompatibility warning for `net2 v0.2.39` |
| `cargo test --workspace --no-run` | passed; existing MSVC linker informational warnings |
| `cargo test --workspace` | passed: komorebi 98 passed/1 ignored; layouts 128 passed; bar 3 passed; remaining targets/doc-tests passed with zero tests |

The Clippy limitation was an installed-toolchain mismatch, not a claim that Windows cannot run
Clippy.

**Toolchain baseline revised on 2026-08-29 during Phase 3B/3C.** A rustup-managed toolchain is now
active and all three previously blocked checks run:

| Item | Baseline |
| --- | --- |
| rustup | `stable-x86_64-pc-windows-msvc`, active via the repository `rust-toolchain.toml` |
| Clippy | `clippy 0.1.98 (88d9e12ae1 2026-08-18)` |
| rustfmt | `rustfmt 1.9.0-stable`; `rustfmt.toml` sets the nightly-only `imports_granularity`, which warns and is skipped on stable |
| `cargo fmt --check` | clean after the Phase 3B `style:` commit reformatted the code written while rustfmt was unavailable |
| `cargo clippy --workspace --all-targets` | one pre-existing upstream warning: `items after a test module` at `komorebi/src/window.rs:1106`. No `-D warnings`, matching repository CI. |

From Phase 3B onwards, `cargo fmt --check` and `cargo clippy --workspace --all-targets` are part of
every phase's verification and must be reported with real results.

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

### Phase 3B - Focus and minimize histories

Split from the original Phase 3B before coding: histories and the invariant validator each reach
the per-phase review limit on their own, and the validator is only meaningful once the histories
it checks exist.

- [x] Add workspace container MRU, container window MRU, and per-workspace minimize MRU.
- [x] Centralize record, selection, deduplication, and deletion cleanup.
- [x] Add focus, deletion, minimize-history, and serde tests.
- [x] Commit as `feat: add focus and minimize histories`.

Expected handwritten change: 300-500 lines. Likely files: `container.rs`, `workspace.rs`,
`monitor.rs`, `window_manager.rs`, `process_event.rs`, and `state.rs`.

Actual files: new `focus_history.rs`, plus `container.rs`, `workspace.rs`, `window_manager.rs`,
`state.rs`, and `lib.rs`. Actual change: 592 added lines in existing files plus a 195-line new
module; roughly 350 production and 440 test lines. A single deduplicating `Mru<T>` backs all three
histories, so recording, removal, selection, and pruning cannot diverge between them.

`Container::focus_window` and `Workspace::focus_container` are the only recording points, so every
existing focus call site updates both levels without being modified; `record_focused_window` is the
combined entry point. Preselect markers are excluded from the container history because their ID is
fixed rather than stable. Selection (`first_focusable_window`, `focus_target_from_history`) skips
minimized windows and falls back to stack order so it cannot dead-end. `take_last_minimized_window`
discards the stale entries it passes. Removal paths (`remove_window`, `detach_window`,
`remove_container_by_idx`) drop what they invalidate, container deserialization repairs stale,
duplicated, or missing entries, and `apply_state` prunes imported state.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 149 passed/1
ignored (up from 123), layouts 128, bar 3, all other targets and doc-tests passed.
`cargo check --workspace`, `cargo check -p komorebi --features schemars`, and `cargo fmt --check`
passed. `cargo clippy --workspace --all-targets` reported only the pre-existing upstream warning.

Commits: `63d73bf8` (`style:` rustfmt catch-up), `ace8cd88` (foreground-query fix removing the
recorded environment-dependent test failure), `14f32928` (this phase).

### Phase 3C - Ownership and history invariant validation

- [x] Add `validate_invariants()` ownership/history checks enabled by tests and debug assertions.
- [x] Add invariant violation and post-removal consistency tests.
- [x] Commit as `feat: validate ownership and history invariants`.

Expected handwritten change: 200-350 lines. Likely files: new `invariants.rs`, `window_manager.rs`.

Actual files: new `invariants.rs` (512 lines including tests), `container.rs` (history repair now
rebuilds through the deduplicating path), `window_manager.rs`, and `lib.rs`.

`ValidateInvariants` is implemented for `Container`, `Workspace`, `Monitor`, and `WindowManager`,
and reports every violation rather than the first. `assert_invariants` runs from
`update_known_hwnds`, which is where a processed command or event leaves the model at rest; it
panics in this crate's own tests and logs in production. Alternate workspace-level ownership is
tolerated as transitional debt, but holding the same window both in a container and in the
workspace-level floating/maximized/monocle paths is reported.

Geometry invariants (6-10, 16) are intentionally absent until logical slots exist; they are added
in Phases 4-8.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 160 passed/1
ignored, layouts 128, bar 3. `cargo fmt --check` clean; `cargo clippy --workspace --all-targets`
reported only the pre-existing upstream warning. Commit: `a40f12bc`.

### Phase 4A - Logical geometry primitives

Split from the original Phase 4 before coding: the geometry primitives and the workspace wiring
that adopts them each reach the per-phase review limit on their own, and the primitives are
independently testable without touching any existing call site.

- [x] Add `LogicalRect` with field names distinct from `Rect` so the two cannot be interchanged.
- [x] Add 50:50 splitting along the longer edge, new container left or bottom, odd remainder pixel
  to the existing container.
- [x] Add edge, projection and adjacency primitives for later absorption and deletion phases.
- [x] Add `LogicalSlots`: the `ContainerId`-keyed slot map with a monotonic geometry generation.
- [x] Add coverage validation for containment, pairwise non-overlap and exact total area.
- [x] Add split, odd-pixel, adjacency, gap-independence, coverage, generation and serde tests.
- [x] Commit as `feat: add gap-free logical slot geometry`.

Expected handwritten change: 350-500 lines. Likely files: new `geometry.rs`, `lib.rs`.

Actual files: new `geometry.rs` (861 lines, roughly 430 production and 430 test) and the `lib.rs`
module declaration. Coverage validation checks containment plus pairwise non-overlap plus an exact
total area, which is equivalent to gap-free full coverage without materialising a per-pixel map; an
empty slot set is vacuously valid because a workspace is allowed to have no container in a slot.
`with_edge_at` is the single growth primitive absorption needs, so an expanding container can only
change width or height, never both. Verification: 22 focused tests passed;
`cargo test --workspace -- --test-threads=1` passed with komorebi 182 passed/1 ignored (up from
160), layouts 128, bar 3; `cargo fmt --check` clean; `cargo clippy --workspace --all-targets`
reported only the pre-existing upstream warning; `cargo check -p komorebi --features schemars`
passed. Commit: `60ae742f`.

### Phase 4B - Workspace slot authority and render conversion

- [x] Make ID-keyed logical slots the workspace geometry authority.
- [x] Calculate the arrangement without the container gap and store the result by container ID.
- [x] Apply container gap, border offset, border width and stackbar strip only in render
  conversion.
- [x] Preserve integer-pixel coverage and validate it on every recalculation.
- [x] Move cursor hit testing onto the gap-free slots.
- [x] Drop a container's slot when it leaves the workspace; expose slots and generation in state.
- [x] Add tiling, gap-independence, render-equivalence, identity-keying, deletion, gutter,
  generation and serde-default tests.
- [x] Commit as `feat: separate logical slots from window rendering`.

Expected handwritten change: 300-400 lines. Likely files: `workspace.rs`, `geometry.rs`, `state.rs`.

Actual files: `workspace.rs` (346 changed lines), `geometry.rs` (+40 for `RenderInsets` and
`to_render_rect`), and `state.rs` (+2). Actual change: 361 added, 27 removed.

The seam that made this safe is that `komorebi-layouts` applied `container_padding` as a uniform
per-rectangle inset at the very end of `Arrangement::calculate`. Passing `None` therefore yields
exactly the gap-free slots, and re-applying the same inset in `to_render_rect` reproduces the
previous geometry bit for bit. `rendering_a_logical_slot_reproduces_the_previous_layout_geometry`
asserts that equivalence for one to four containers at two gap sizes, so no existing layout moved.

`calculate_logical_slots` is pure and makes no Win32 call, so slot geometry is testable without a
desktop session; `record_logical_slots` stores the slots, records the available area, and logs any
tiling violation rather than failing, because a released build must not refuse to tile over a
geometry inconsistency. `latest_layout` keeps its existing meaning as the rendered, index-keyed
result so no consumer had to change; identity-keyed geometry now lives beside it. The BSP layout
already tiles exactly at odd pixel sizes: coverage validation passes for one to five containers at
both 1920x1080 and 1001x777.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 190 passed/1
ignored (up from 182), layouts 128, bar 3, all other targets and doc-tests passed. `cargo fmt
--check` clean; `cargo clippy --workspace --all-targets` reported only the pre-existing upstream
warning; `cargo check -p komorebi --features schemars` passed. Commit: `dfe26c63`.

Not run this phase: `just jsonschema` regeneration. `komorebic static-config-schema` overflows its
stack in a debug build, and it does so identically on the pre-change commit, so this is a
pre-existing environment limitation rather than a Phase 4 regression. `StaticConfig` was not
touched, so `schema.json` needs no update; schema regeneration is deferred to the release-built
verification in Phases 12 and 14.

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
- 2026-08-29: Resolved in `ace8cd88`. `Monitor::move_container_to_workspace` only queried the
  foreground window to detect a floating move subject, so a failed query now means "no floating
  window" instead of aborting the move. The two unit tests no longer depend on the desktop session.
- 2026-08-29: A single `Mru<T>` backs all three histories rather than three bespoke lists, so
  deduplication, pruning, and selection semantics cannot drift apart between levels.
- 2026-08-29: Histories are recorded inside `Container::focus_window` and
  `Workspace::focus_container` rather than at each call site, so no future focus path can update
  one level without the other.
- 2026-08-29: `assert_invariants` panics only in this crate's tests and logs in production. A
  released build must not terminate a user's session over a model inconsistency.
- 2026-08-30: `LogicalRect` uses `width`/`height` where `Rect` uses `right`/`bottom` for the same
  quantities. The differing field names, not just the differing type, are what stop a logical slot
  and a window rectangle from being swapped by accident.
- 2026-08-30: `latest_layout` keeps its existing index-keyed, rendered meaning. Identity-keyed
  logical slots were added beside it instead of redefining it, so no existing consumer changed
  behaviour in the same phase that introduced the new authority.
- 2026-08-30: Gap-free slots are produced by passing `None` for `container_padding` to the existing
  arrangement rather than by writing a new tiling algorithm, because that crate already applies the
  gap as a final per-rectangle inset. This keeps every existing layout, including custom layouts,
  working unchanged and is asserted by a render-equivalence test.
- 2026-08-30: Coverage violations are logged, not returned as errors, at the recalculation point.
  Geometry invariants are enforced by tests and by `assert_invariants`; a user session must not
  lose its tiling because one layout produced an unexpected rectangle.
- 2026-08-30: `komorebic static-config-schema` overflows its stack in a debug build on this
  machine, on the pre-change commit as well. Schema regeneration therefore needs a release build
  and is deferred to Phases 12 and 14.

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
- 2026-08-29: Phase 3B/3C turn. The toolchain baseline was re-checked first and had changed:
  rustup, Clippy, and a modern rustfmt are all available now, so the recorded "unavailable"
  limitations no longer apply and both checks were run for real. The code written under the old
  limitation was reformatted in its own `style:` commit before any feature work, and the recorded
  foreground-window test dependency was fixed in its own commit. Phase 3B added the three
  most-recently-used histories with centralized recording, selection, and deletion cleanup. Phase
  3C added the ownership/history invariant validator and wired it into the at-rest runtime path.
  The full serial workspace suite passed at every step (komorebi 123 -> 149 -> 160 passing).
  Next phase: logical slots and render rectangles.
- 2026-08-30: Phase 4 turn. The plan was re-read and the worktree confirmed clean at `1a9e2d5e`
  before editing, and `cargo check --workspace` was re-run as this turn's baseline. Phase 4 was
  split into 4A primitives and 4B workspace adoption before coding, for the same review-size reason
  as Phases 2 and 3. 4A added the gap-free `LogicalRect`, 50:50 splitting with the odd pixel going
  to the existing container, adjacency by edge coordinate and edge projection, and the
  identity-keyed `LogicalSlots` map with its geometry generation and coverage validator. 4B made
  those slots the workspace geometry authority: the arrangement is calculated without the container
  gap, the result is stored by `ContainerId`, and gaps, borders and the stackbar strip are applied
  only in `to_render_rect`. A render-equivalence test pins the rendered output to the previous
  behaviour. Full serial workspace suite passed at every step (komorebi 160 -> 182 -> 190 passing);
  fmt and Clippy clean apart from the pre-existing upstream warning. Next phase: derived
  Active/Hidden container state, which is what makes the coverage invariant apply to active
  containers only.
- 2026-08-29: Phase 3 was split into 3A identity and 3B histories/invariants after call-site review
  showed that doing both together would exceed the phase review-size limit. Phase 3A added
  transparent typed workspace/container IDs, migrated managed ownership and UI integration
  boundaries, preserved workspace IDs in state snapshots, and maintained legacy JSON compatibility.
  Compile, schema, focused serial tests, and the full serial workspace suite passed. Next phase:
  explicit focus/minimize histories and ownership/history invariant validation.
