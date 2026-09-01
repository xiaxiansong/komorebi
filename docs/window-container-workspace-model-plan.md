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

### Phase 5A - Derived Active/Hidden container state

Split from the original Phase 5 before coding. A call-site census showed 309 references to the
three alternate ownership paths, so removing them together with the state derivation would be
several times the per-phase review limit. 5A owns the derived state and the slot authority, 5B
owns floating ownership, 5C owns minimize ownership, and 5D owns the maximized/monocle paths.

- [x] Add derived `ContainerState` and active-container selectors.
- [x] Make only containers with a visible stored window occupy a logical slot.
- [x] Add the geometry start-container rule for a focus which sits in a hidden container.
- [x] Extend invariant validation with the stale-safe slot rules.
- [x] Add Hidden classification, selector, and slot-release tests.
- [x] Commit as `feat: derive active and hidden container state`.

Expected handwritten change: 300-450 lines. Likely files: `container.rs`, `workspace.rs`,
`invariants.rs`.

Actual files: `container.rs`, `workspace.rs`, `invariants.rs`. Actual change: 491 added, 49
removed, roughly half of it tests.

`Container::state()` is derived on every read rather than stored, so it cannot drift from the
windows it describes. A preselect marker reports Active because it is a reserved place in the
arrangement rather than a container of the model. `calculate_logical_slots` now arranges the
active containers alone and projects the focused index and the resize dimensions onto that
subset, which is what makes the remaining containers cover a hidden container's area. The render
loop looks its rectangle up by `ContainerId`, and `latest_layout` is documented as the rendered
rectangles of the containers which own a slot rather than a per-container index.

The monocle and maximize paths take a container out of the ring without going through
`remove_container_by_idx`, so both now drop that container's slot explicitly; without that they
leave an orphan slot behind.

Only the stale-safe slot rules are validated at every point of rest: a slot belongs to a
container the workspace still owns, and recorded slots do not overlap. Exact coverage and "a
hidden container owns no slot" are properties of a freshly recorded arrangement - a background
workspace which has not been retiled since its last structural change would report them
spuriously - so they are checked where the arrangement is recorded and by the phase's tests, and
they become model invariants in Phase 6, which introduces the transition point that keeps them
true continuously.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 204 passed/1
ignored (up from 190), layouts 128, bar 3. `cargo fmt --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing upstream warning; `cargo check -p komorebi
--features schemars` passed. Commits: `1d034348` (test race fix, below), `7c723fe4` (this phase).

A pre-existing race had to be fixed first, in its own commit. `test_listen_for_notifications`
starts a listener thread which lives for the rest of the test binary and consumes the global
notification channel, so `test_send_notification` could lose its message to that thread; the
outcome depended on how long the earlier tests took, and this phase's new tests changed that
timing. The clean tree passed five serial runs and the changed tree failed three of four. The
test now sends until one of its own notifications comes back, which restores five clean serial
runs without weakening what it asserts.

### Phase 5B - Container-owned floating windows

- [x] Remove `Workspace::floating_windows` and derive the floating window list from containers.
- [x] Make float and unfloat placement changes which keep container membership, stack order and
  both histories.
- [x] Make container visibility placement-aware so a floating window is not hidden by its stack
  position and is not positioned into its container's slot.
- [x] Carry the complete managed state across workspace and monitor moves.
- [x] Stop reporting floating windows as alternate ownership in the invariant validator.
- [x] Add ownership, hide/restore, listing order, transfer, and atomic-failure tests.
- [x] Commit as `feat: own floating windows through their containers`.

Expected handwritten change: 400-600 lines across ten files; the storage swap cannot be split
further because every call site must compile in the same commit.

Actual files: `workspace.rs`, `container.rs`, `window_manager.rs`, `process_command.rs`,
`process_event.rs`, `monitor.rs`, `monitor_reconciliator/mod.rs`, `invariants.rs`, `state.rs`,
and `komorebi-bar/src/widgets/komorebi.rs`. Actual change: 591 added, 278 removed, roughly half
of it tests.

`floating_windows()` returns an owned `Vec<Window>` derived in container order and then stack
order. It is a listing, not storage, so a floating window cannot be lost when its container
changes and cannot be held twice. `float_window`, `unfloat_window` and `set_floating_rect` are
the whole mutation surface; `take_window`/`adopt_managed_window` are the transfer pair, and they
carry placement, visibility, presentation and the floating rectangle with the window.

Two upstream behaviours changed as a direct consequence, and both are what the model requires.
Floating no longer removes a window from its container, so it no longer destroys an emptied
container or shifts the indices of locked containers - `test_locked_containers_toggle_float`
asserts the new contract. Unfloating no longer creates a container; the window returns to the
control of the one which owned it all along.

`Container::restore` and `Container::load_focused_window` became placement-aware: only one stored
window of a stack is on screen at a time, but a floating window keeps its own rectangle and stays
visible next to it, and a minimized window is never restored by either. `Workspace::update`
positions only `visible_stored_windows`, so a floating window in an active container is not
dragged into the container's slot.

Old runtime-state JSON which still carries a workspace-level `floating_windows` array
deserializes, but that array is ignored rather than migrated. `StaticConfig` is unaffected.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 213 passed/1
ignored (up from 204), layouts 128, bar 3. `cargo fmt --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing upstream warning; `cargo check -p komorebi
--features schemars` passed. Commit: `f478d47d`.

### Phase 5C - Container-owned minimized windows

- [x] Make minimize a visibility change which keeps container membership instead of a removal.
- [x] Record and prune the workspace minimize history around that transition.
- [x] Restore the last minimized window through the transition methods and both MRUs.
- [x] Keep duplicate and out-of-order minimize/restore events idempotent.
- [x] Add minimize-hides-container, restore-reactivates, and history tests.
- [x] Commit as `feat: keep minimized windows in their containers`.

Expected handwritten change: 300-450 lines. Likely files: `workspace.rs`, `container.rs`,
`window_manager.rs`, `process_event.rs`, `process_command.rs`.

Actual files: `workspace.rs`, `window_manager.rs`, `process_event.rs`. Actual change: 412 added,
5 removed, roughly 200 production and 210 test lines.

Upstream removed a minimized window from its workspace, so it lost its container, its stack
position and both of its history entries, and returning from the taskbar re-managed it as a
brand-new window. `Workspace::minimize_window` and `unminimize_window` change only visibility;
the container keeps everything else and simply stops occupying a slot once it has no visible
stored window left. Container focus moves off a window as it is minimized, because a minimized
window is not a focus target, but its stack position and its place in the container's window
count are untouched.

`restore_last_minimized_window` reverses exactly that through `take_last_minimized_window`, which
already discards stale entries, and returns the window with the placement and presentation it was
minimized with.

Both transitions return whether anything changed, and `set_managed_window_minimized` only retiles
on a real change. The taskbar restore path is the same reconciliation, driven from the ordinary
show/focus/uncloak/manage events when Win32 reports the window is no longer minimized, so a
duplicated or out-of-order event converges instead of retiling repeatedly. Because the window
never left its container, the existing `contains_window` guard in the show path already stops it
from being re-managed as a new window.

`HIDDEN_HWNDS` still distinguishes a programmatic hide from a user minimize, and the model's own
visibility - not the Win32 state - decides what `Container::restore` brings back, so a workspace
hidden with `HidingBehaviour::Minimize` still restores correctly.

Not in this phase: the socket message and `komorebic` verb for restoring the last minimized
window. The core operation exists and is tested; the command surface is finalized in Phase 12 as
planned.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 226 passed/1
ignored (up from 213), layouts 128, bar 3. `cargo fmt --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing upstream warning; `cargo check -p komorebi
--features schemars` passed. Commit: `e051a934`.

### Phase 5D - Presentation replaces maximized and monocle ownership

Split into 5D-1, 5D-2 and 5D-3 on 2026-08-30, before coding. A call-site census counted 209
references to `maximized_window` and `monocle_container` across twelve files, which is several
times the per-phase review limit. The two alternate paths are almost disjoint per file, so each
can be removed on its own while the other keeps compiling unchanged.

#### Phase 5D-1 - Maximize is a window presentation

- [x] Remove `Workspace::maximized_window` and `maximized_window_restore_idx` ownership; derive the
  maximized window from the containers instead.
- [x] Route maximize and unmaximize through `ManagedWindow::set_maximized`/`set_normal` so a
  maximized window keeps its container, its stack position and both histories.
- [x] Make container show/hide and the render loop presentation-aware so a maximized window is not
  silently restored into its slot.
- [x] Narrow the "cannot move a native maximized window" refusals to the container actually being
  moved instead of the whole workspace.
- [x] Stop reporting the maximized window as alternate ownership in the validator.
- [x] Add ownership-preserving maximize, idempotency, restore-rectangle and stack tests.
- [x] Commit as `feat: make maximize a window presentation`.

Expected handwritten change: 350-500 lines. Likely files: `workspace.rs`, `container.rs`,
`managed_window.rs`, `window_manager.rs`, `monitor.rs`, `process_event.rs`, `process_command.rs`,
`monitor_reconciliator/mod.rs`, `invariants.rs`, `state.rs`.

Actual files: `workspace.rs`, `container.rs`, `managed_window.rs`, `window_manager.rs`,
`monitor.rs`, `process_event.rs`, `process_command.rs`, `monitor_reconciliator/mod.rs`,
`invariants.rs`, and `state.rs`. Actual Rust change: 465 added, 348 removed, roughly half of it
tests. Every predicted file was touched and no other file needed to change; `komorebi-bar` and the
client were unaffected because neither reads the maximized window.

`Workspace::maximized_managed_window` is a listing derived from container ownership, in container
order and then stack order, exactly like `floating_windows()` from Phase 5B. `maximize_window` and
`unmaximize_window` are the whole mutation surface, and both are idempotent: maximizing a window
which is already maximized reapplies the Win32 state without overwriting the restore rectangle,
and unmaximizing without a maximized window is refused before anything changes.

Three upstream behaviours changed as a direct consequence, and all three are what the model
requires. Maximizing no longer removes the window from its container, so it no longer empties and
destroys a container and no longer rebuilds a different container with a new ID on the way back;
`test_maximize_and_unmaximize_window` asserts the new contract, which is why its second half now
maximizes the second window of the same stack instead of a window of a container that the old
split had created. A maximized window now blocks a container move only when it is in the container
being moved, rather than anywhere in the workspace. And the separate maximized branch in
`move_window_to_monitor` became unreachable once the window lives in a container, so it was
removed rather than left as dead code that reads like a live path.

`ManagedWindow::show` is what keeps Win32 and the model in agreement: a plain restore is
`SW_RESTORE`, which also unmaximizes, so `Container::restore` and `Container::load_focused_window`
would have silently dropped a presentation the model owns. The render loop makes the same
distinction in the other direction: a maximized window keeps its container's slot, because its
container is still Active, but it is drawn maximized instead of into that slot, and a window Win32
still reports as maximized after the model returned it to Normal is restored there.

Old runtime-state JSON which still carries `maximized_window` and `maximized_window_restore_idx`
deserializes, and those fields are ignored rather than migrated, matching the Phase 5B precedent
for `floating_windows`. `StaticConfig` is unaffected. `WorkspaceWindowLocation::Maximized` was
removed because a maximized window is now found by the container arm before it could ever be
reached.

Not in this phase: the monocle container, which still holds windows outside the ring and is still
reported as the one remaining alternate ownership path by the validator. That is Phase 5D-2.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 236 passed/1
ignored (up from 226), layouts 128, bar 3, all other targets and doc-tests passed. `cargo fmt
--check` clean; `cargo clippy --workspace --all-targets` reported only the pre-existing upstream
warning; `cargo check --workspace --all-targets` and `cargo check -p komorebi --features schemars`
passed.

#### Phase 5D-2 - Monocle is a workspace reference, not workspace storage

- [x] Replace `monocle_container`/`monocle_container_restore_idx` with a `ContainerId` reference to
  a container which stays in the workspace ring.
- [x] Make monocle a slot-authority decision rather than a removal, preserving container ID, stack
  order and both histories.
- [x] Remove the transitional debt clause from the validator.
- [x] Add monocle ownership, cycle, reintegration and idempotency tests.
- [x] Commit as `feat: make monocle a container reference`.

Expected handwritten change: 350-500 lines. Likely files: `workspace.rs`, `window_manager.rs`,
`process_event.rs`, `process_command.rs`, `monitor.rs`, `monitor_reconciliator/mod.rs`,
`border_manager/mod.rs`, `transparency_manager.rs`, `stackbar_manager/mod.rs`, `state.rs`,
`komorebi-bar/src/widgets/komorebi.rs`.

Actual files: every predicted file except `invariants.rs`, which was not predicted and had to
change because this is the phase that removes the transitional debt clause. Actual Rust change:
446 added, 372 removed, roughly half of it tests.

`monocle_container_id: Option<ContainerId>` is the whole storage change. `monocle_container()`,
`monocle_container_mut()`, `monocle_container_idx()` and `is_monocle()` resolve it against the
ring, so a reference which no longer resolves degrades to "not in monocle mode" instead of
producing a container the workspace does not own. `new_monocle_container` and
`reintegrate_monocle_container` now only set and clear that reference and reload the container's
focused window; neither moves a container in or out of the ring, so a monocle toggle can no longer
change a container ID, lose a stack, drop a resize adjustment or renumber locked containers.

Because the monocle container is an ordinary ring container, five places which had to handle it
separately were handling it twice: the `exe()` sweep and the `known_hwnds` scan in
`window_manager.rs`, the two accent sweeps, the three monitor-reconciliation sweeps, the lower
pass in the tiling layer, and the bar's container listing. All of them were removed in favour of
the container loop that already covers it. The monocle branch in `move_window_to_monitor` became
unreachable for the same reason and was removed with the maximized branch Phase 5D-1 had already
left dead; `transfer_container` moves the monocle container through the ordinary path, and
`forget_container` takes the reference with it.

Two behaviours are deliberately narrowed rather than preserved. `Workspace::visible_windows` and
`visible_window_details` now return the monocle container alone while monocle is on, because the
other containers are hidden; previously the monocle window was an extra entry in front of an
arrangement that was not on screen. And `hide_containers_around_monocle` replaces "hide every
container", which would now hide the container monocle is meant to be showing.

`prune_monocle_reference` closes the one path where containers leave the ring in bulk rather than
through `remove_container_by_idx`: the empty-container retain at the top of `Workspace::update`,
and `prune_histories` for imported state.

The validator no longer tolerates any alternate ownership. The clause is replaced by the rule
that a monocle reference must resolve to a container the workspace owns, which is the only way
the reference can now be wrong.

Old runtime-state JSON carrying `monocle_container` and `monocle_container_restore_idx`
deserializes with those fields ignored rather than migrated, matching the Phase 5B and 5D-1
precedents. `StaticConfig` is unaffected.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 245 passed/1
ignored (up from 236), layouts 128, bar 3, all other targets and doc-tests passed. `cargo fmt
--check` clean; `cargo clippy --workspace --all-targets` reported only the pre-existing upstream
warning; `cargo check --workspace --all-targets` and `cargo check -p komorebi --features schemars`
passed.

#### Phase 5D-3 - Fullscreen distinct from maximize

- [x] Add fullscreen enter/exit as its own presentation transition with its own restore rectangle.
- [x] Keep fullscreen, maximize and minimize independent and idempotent in both directions.
- [x] Add fullscreen/maximize independence and restore-rectangle tests.
- [x] Commit as `feat: separate fullscreen from maximized presentation`.

Expected handwritten change: 200-350 lines. Likely files: `managed_window.rs`, `workspace.rs`,
`window_manager.rs`, `process_command.rs`, `core/mod.rs`.

Actual files: every predicted file plus `container.rs`, `monitor.rs`, `process_event.rs`,
`komorebic/src/main.rs`, `komorebic.lib.ahk` and `docs/cli/`. Actual Rust change: 549 added, 68
removed, roughly half of it tests. The four unpredicted source files are all consequences of the
same thing: the model gained a second presentation, so every place which asked "is a window
covering the arrangement" had to stop asking "is a window maximized".

`Presentation::Fullscreen` already existed as observed state from Phase 2A; this phase is what
gives it transitions, a Win32 application, a slot-authority decision and a command. The two
presentations are never applied through the same Win32 call: maximizing is `ShowWindow`, and
fullscreen is `SetWindowPos` onto the monitor bounds, which is exactly the rectangle
`WindowsApi::is_fullscreen` recognises when an application enters borderless fullscreen without
being asked. `WorkspaceGlobals::monitor_size` carries those bounds from `Monitor::size`, so the
fullscreen rectangle needs no Win32 query and is therefore testable; it is the monitor bounds and
not the work area because a fullscreen window covers the taskbar.

`presented_window()` is the new name for "a window drawn over the arrangement instead of in its
slot", at container and workspace level. Ten call sites which meant that but said `maximized_window`
now say so: workspace entry Z order, `is_focused_window_monocle_or_maximized`, both directional
`new_idx_for_direction` guards, both cross-monitor focus paths, the follow-focus and stack-focus
paths in `update_focused_workspace`, and the two `process_event` foreground branches. The sites
which really do mean maximize -- `toggle_maximize`, `unmaximize_window` -- keep asking for it.
`focused_container_has_maximized_window` was renamed to `focused_container_has_presented_window`
for the same reason: a fullscreen window is no more movable to another monitor than a maximized one.

`enter_presentation` and `leave_presentation` are the whole mutation surface, and both presentations
share them, so neither can acquire a transition rule the other lacks. Entering is idempotent because
the Win32 state is reapplied whether or not the model changed. Switching between the two keeps the
restore rectangle captured when the window left Normal, rather than recording the monitor-sized
rectangle the previous presentation had given it. Leaving is presentation-aware in one place only:
a maximized window has Win32 window state to drop, and a fullscreen window has none and must be
repositioned instead, because nothing else would move it off the monitor bounds.

`focus_container_in_cycle_direction` carried a maximized presentation to the container focus moved
to. It now carries whichever presentation was there, because reapplying the wrong one would silently
convert a fullscreen window into a maximized one.

`komorebic toggle-fullscreen` and `SocketMessage::ToggleFullscreen` are the command surface, with a
matching `komorebic.lib.ahk` wrapper. Existing commands are untouched. Runtime state and static
configuration are unaffected: `Presentation` already serialized all three variants, and
`monitor_size` is `#[serde(default)]` so old runtime state still deserializes.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 257 passed/1 ignored
(up from 245), layouts 128, bar 3, all other targets and doc-tests passed. `cargo fmt --all --check`
clean; `cargo clippy --workspace --all-targets` reported only the pre-existing upstream
`items after a test module` warning in `window.rs`; `cargo check --workspace --all-targets` and
`cargo check -p komorebi --features schemars` passed.

### Phase 6 - Hidden slot absorption and restoration

Split into 6A and 6B on 2026-08-30, before coding. The phase has a pure-geometry half and a
workspace-state half, and the geometry half is what the state half has to be written against, so
doing them together would mean reviewing an unproven algorithm and its wiring at once. 6A owns the
edge algebra and both directions of the plan; 6B owns the records, the transition detection and the
invalidation rules.

This is also the phase where the slot map stops being a pure function of the layout. Until now
`record_logical_slots` recomputed every slot from `layout.calculate()` on every update, so hiding a
container meant a full relayout of the workspace. From 6B the map can carry local edits, and the
layout recalculation becomes the fallback rather than the only path.

#### Phase 6A - Complete-edge groups and the absorption algebra

- [x] Implement complete-edge neighbour group selection in left/right/up/down order.
- [x] Implement the absorption plan and its exact reverse as validated, not-yet-applied values.
- [x] Add single/multiple neighbour, direction priority, partial-cover refusal, odd-pixel,
  round-trip and min-size tests.
- [x] Commit as `feat: add complete edge slot absorption`. Commit: `273052ca`.

Expected handwritten change: 300-400 lines. Likely files: `geometry.rs`.

Actual files: `geometry.rs` only, as predicted. Actual Rust change: 479 added, 0 removed, roughly
half of it tests.

`SlotShift` describes both directions with one value, because an absorption and its release are the
same operation with the movers' before and after swapped. Nothing is written until a plan is
applied, which is what lets a caller refuse a whole operation without having half-changed the
geometry -- the atomicity requirement, expressed as a type rather than as a rollback path.

`complete_edge_group` is one sweep: the candidates on an edge are walked in their deterministic
order and each must begin exactly where the previous one ended, the first at the edge's start and
the last at its end. Overlaps, gaps and partial cover are all rejected by that one condition, so
there is no separate check which could disagree with another.

`plan_release`'s whole validity test is that every recorded absorber still holds exactly the
rectangle the absorption gave it. That equality is stronger than a generation comparison and than a
minimum-size check: if it holds for all of them, moving each edge back releases precisely the
recorded slot and nothing else. The minimum-size check is kept anyway because the recorded
rectangles arrive from stored state and are therefore checked rather than trusted.

`SlotShift` deliberately does not derive `PartialEq`: `OperationDirection` is an upstream
`komorebi-layouts` type which does not implement it, and deriving it there for a test convenience
would widen the upstream conflict surface for no model benefit.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 268 passed/1 ignored
(up from 257), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing upstream warning.

#### Phase 6B - Hidden slot records, transitions and invalidation

- [x] Add `HiddenSlotRestore` snapshots keyed by container, carrying the geometry generation.
- [x] Drive absorption from Active -> Hidden and exact release from Hidden -> Active.
- [x] Invalidate restores on all named topology/geometry operations and fall back to full relayout.
- [x] Add only-active, consecutive hide/restore, exact/fallback and invalidation tests.
- [x] Commit as `feat: restore hidden container slots safely`.

Expected handwritten change: 300-450 lines. Likely files: `workspace.rs`, `window_manager.rs`,
`state.rs`.

Actual files: `workspace.rs`, `state.rs` and `komorebi-layouts/src/operation_direction.rs`.
`window_manager.rs` did not need to change, which is the point of the fingerprint decision below.

This is the phase where the slot map stopped being a cache of the layout. `record_logical_slots`
now reconciles rather than recalculates: a container which just became hidden gives its slot to a
complete edge group, one which just became active takes its slot back from exactly the containers
which absorbed it, and anything else falls back to `recalculate_logical_slots`, which is the old
behaviour under its own name.

The hardest decision was how to know that a recalculation is needed. The alternative to a
fingerprint was a dirty flag set by each of the twenty-odd places which assign a layout, a flip, a
resize adjustment or a container order, spread across `workspace.rs`, `window_manager.rs` and
`process_command.rs`. One forgotten call there leaves the arrangement stale with no test able to
notice, so `SlotInputs` compares the arrangement inputs themselves instead. The focused container is
deliberately not one of them: only `DefaultLayout::Scrolling` arranges around it, and including it
would let an ordinary focus change discard a local absorption, so that one layout invalidates the
geometry explicitly in `focus_container`.

Both absorption and release plan every step against a running copy before writing any of it, so a
group which becomes incomplete because of an earlier absorption in the same update is caught before
the arrangement is half-collapsed. A container which departed because it was removed rather than
hidden gets no restore record, because there is nothing left to restore it to.

Container deletion still takes the recalculation path: `remove_container_by_idx` drops the slot
itself, and the container list is part of the fingerprint. Deletion expansion is Phase 8, and it
will reuse the same `plan_absorption`.

`HiddenSlotRestore::geometry_generation` is recorded and serialized but is diagnostic rather than
decisive. `plan_release` compares the absorbers' current rectangles against what the absorption gave
them, which is strictly stronger than a generation comparison: a generation can be advanced by a
change which does not affect these containers, and it cannot detect a change which happens to leave
the counter where a stale record expects it.

`OperationDirection` gained `PartialEq`/`Eq` in `komorebi-layouts`. `Workspace` derives `PartialEq`,
so a restore record which names a direction cannot compile without it, and almost every other type
in that crate already derives it. This is the first change this task has made outside `komorebi`
itself and is recorded as an upstream conflict point.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 278 passed/1 ignored
(up from 268), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing upstream warning; `cargo check -p komorebi --features
schemars` passed.

Not in this phase: `ContainerState` and the restore records are not yet in the `state.rs` query
output beyond what serializing `Workspace` already gives. The full state-output list is Phase 14.

### Phase 7 - New-window threshold placement and manual split

Split into 7A, 7B and 7C on 2026-08-30, before coding, for the same review-size reason as Phases 2,
3, 5 and 6. 7A owns the two pure-geometry primitives the other two are written against; 7B owns the
automatic allocation rule the window manager reaches on every new window; 7C owns the operator-driven
split, which shares 7A's primitive but takes its donor and its window from the focus histories rather
than from the arrangement.

This is also the phase where a container arrives with a slot of its own. Until now every arrival went
through `recalculate_logical_slots`, because the only local edits were a hide and its exact reverse.
From 7B a split is a third local edit, and it is applied at insertion time so that the arrangement
the caller asked for is already in place by the time `record_logical_slots` reconciles.

#### Phase 7A - Split and neighbour selection primitives

- [x] Add `SlotSplit` as a validated, not-yet-applied 50:50 division of one slot.
- [x] Add automatic long-edge and forced-axis splitting on the slot map.
- [x] Add deterministic neighbour selection in left/right/up/down order with per-direction ordering.
- [x] Add axis, odd-pixel, refusal, generation, neighbour-priority and neighbour-order tests.
- [x] Commit as `feat: add slot splitting and neighbour selection`.

Expected handwritten change: 150-300 lines. Likely files: `geometry.rs`.

Actual files: `geometry.rs` only, as predicted. Actual Rust change: 307 added, 15 removed, about two
thirds of it tests.

`SlotSplit` is the same shape of value as `SlotShift`: validated, inert until applied, and refusable
without having half-changed the geometry. It reuses `SlotMove` for the donor, because a donor in a
split changes exactly one edge, which is what that type already means.

The one real decision here was that neighbour selection is *not* the absorption group. Absorption
needs a complete edge because area changes hands and a partial group would leave a hole; choosing a
container to receive a window moves no area at all, so a neighbour which covers part of an edge is a
perfectly good recipient. They share `neighbours_on_edge` and the `ABSORPTION_DIRECTIONS` priority,
so the two answers can never disagree about who is adjacent or in what order, but the completeness
test belongs only to the one that needs it. `complete_edge_group` was rewritten onto that shared
helper so there is a single definition of adjacency ordering.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 290 passed/1 ignored
(up from 278), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings (`window.rs` items-after-test-module and
the upstream `net2` note).

#### Phase 7B - Active-count allocation for new windows

- [x] Implement the N=0, N<=2 and N>2 rules against the active container count.
- [x] Apply the split as a local slot edit at insertion so the arrangement is not recalculated.
- [x] Use the geometry-focused container when the focused container is hidden.
- [x] Add the no-neighbour diagnostic fallback to the focused active container.
- [x] Add N=0/1/2/3, split-position, odd-pixel, neighbour-order and hidden-focus tests.
- [x] Commit as `feat: add threshold based container allocation`.

Expected handwritten change: 250-400 lines. Likely files: `workspace.rs`, `window_manager.rs`,
`process_event.rs`.

Actual files: `workspace.rs` and `process_event.rs`. `window_manager.rs` did not need to change:
its one `new_container_for_window` call is a cross-workspace send, which is Phase 10/11's rule and
not this one. Actual Rust change: 436 added, 2 removed, a little over half of it tests.

The threshold is expressed where the existing `WindowContainerBehaviour::Create` behaviour was,
rather than as a new behaviour beside it. Creating a container for every new window and stopping at
two are not two policies a user would want to choose between per workspace; they are one rule with a
threshold in it, and `Append` still means what it always meant. This does change what `Create` does
for anyone with three or more containers, and it is recorded as an intentional incompatibility.

The decision worth recording is that the split is applied to the slots at insertion rather than left
to the layout. The slot map is the geometry authority, but the arrangement fingerprint from 6B
contains the container list, so an insertion would otherwise be seen as an input the arrangement has
never been calculated for, and the next reconciliation would recalculate the whole workspace and
discard the halves the rule had just produced. `adopt_slot_geometry` is the counterpart to
`invalidate_slot_geometry`: this edit was deliberate, so the arrangement is what the slots now say
it is. A split which cannot be planned - a slot too small to halve, no donor at all - falls back to
an ordinary creation and therefore to a recalculation, so a refusal can never leave a container
without a slot.

`DefaultLayout::Scrolling` is excluded from the local split for the same reason it invalidates
geometry on focus change: it defines its arrangement from the focused container, so an edit to its
slots would not survive its next recalculation. It gets the fallback creation instead.

Joining does not require a complete edge group, only adjacency, which is what 7A separated. The
no-neighbour arm is unreachable while three or more slots tile the work area - it means the slots
and the containers disagree - so it warns and falls back to the focused container rather than
failing the window's placement.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 300 passed/1 ignored
(up from 290), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings; `cargo check -p komorebi --features
schemars` passed.

Not in this phase: the manual split command and its donor selection, which is 7C, and the CLI
surface for either, which is Phase 12.

#### Phase 7C - Manual container creation from an eligible donor

- [x] Select the donor by container MRU among active containers holding at least two windows.
- [x] Select the window by the donor's window MRU, falling back to the top of the stack.
- [x] Preserve placement, visibility, presentation and floating rectangle across the move.
- [x] Recompute donor and new container state and route both through the Hidden transition engine.
- [x] Refuse atomically when no eligible donor exists, changing nothing.
- [x] Add auto/horizontal/vertical, MRU-selection, state-preservation, hidden-outcome and
  atomic-failure tests.
- [x] Commit as `feat: split a container from an eligible donor`.

Expected handwritten change: 250-400 lines. Likely files: `workspace.rs`, `window_manager.rs`.

Actual files: `workspace.rs` and `container.rs`. `window_manager.rs` did not need to change; the
command surface is Phase 12. Actual Rust change: 373 added, 0 removed, about two thirds of it tests.

Donor window selection is `Container::donor_window_idx`, deliberately beside
`first_focusable_window` rather than reusing it. They answer different questions: focus selection
must skip a minimized window because focusing one would be wrong, while a manual split may take a
floating or a minimized window, because a container is perfectly entitled to start out hidden. Two
functions which differ by one condition are clearer than one with a flag, and the difference is
exactly the one this phase had to get right.

Atomicity is by construction rather than by rollback. Everything which can refuse - no eligible
donor, no window to take, a slot which cannot be divided as asked - is decided while nothing has
been written, and the first mutation is the window's removal, after which no step can fail. This is
the same discipline as `plan_absorption` and `plan_split`, applied to an operation which touches
containers as well as geometry.

Neither hidden outcome is a special case in the code. A new container which received a floating or
minimized window has no visible stored window, so it is hidden by the ordinary derivation and hands
its half straight back through the ordinary absorption on the next reconciliation, with a restore
record which will give it back if the window is ever unfloated or restored. A donor left without a
visible stored window becomes hidden the same way. What makes this work is that only one of the two
can happen per split, so the reconciliation never sees an arrival and a departure at once: the
created container's slot is written here, not discovered there.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 312 passed/1 ignored
(up from 300), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings; `cargo check -p komorebi --features
schemars` passed.

Phase 7 is complete: a new window is placed by the active-count threshold, and an operator can split
a container off a donor along either axis or the longer edge. Next phase: 8, container deletion,
window distribution and multi-neighbour resize, which reuses `plan_absorption` for the deletion
expansion the same way this phase reused `plan_split`.

### Phase 8 - Container deletion, distribution, and multi-neighbor resize

Split into 8A, 8B and 8C on 2026-08-30, before coding, for the same review-size reason as Phases 2,
3, 5, 6 and 7. The three concerns are genuinely separable: 8A is what happens to the *area* a
departing container leaves behind, 8B is what happens to the *windows* it was holding, and 8C is a
boundary move which deletes nothing at all. Only 8A and 8B share a primitive, and 8B is written
against the recipient ordering 8A establishes.

Until this phase a deletion has always ended in a full recalculation: `forget_container` drops the
slot outright, and the container list inside `SlotInputs` then differs from the fingerprint, so the
next `record_logical_slots` recalculates the whole workspace. That is never *wrong* - it always
tiles - but it discards every manual boundary the user had, which is exactly what the task's
directional expansion rule exists to avoid.

#### Phase 8A - Active deletion expansion and post-deletion focus

- [x] Plan the departing container's absorption while its slot is still in place, and apply it as a
  local slot edit rather than falling through to a recalculation.
- [x] Fall back to invalidation, not to a hole, when no edge can absorb the slot.
- [x] Invalidate the hidden restore records whose absorbers the expansion moved.
- [x] Select post-deletion focus from the first expansion recipient in plan order.
- [x] Add expansion, multi-neighbour expansion, fallback, restore-invalidation and focus tests.
- [x] Commit as `feat: expand neighbours over a deleted container`.

Expected handwritten change: 250-400 lines. Likely files: `workspace.rs`.

Commit: `a3bf6443`. Actual files: `workspace.rs` only, as predicted. Actual Rust change: 418 added,
8 removed, about two thirds of it tests.

The whole phase turns on one ordering constraint. The absorption can only be planned while the
departing slot is still in the map, because that is the only moment the group which can take it is
knowable; it can only be *adopted* once the container has left the ring, because the arrangement
fingerprint from 6B contains the container list, and adopting a list the container is still in would
leave the next reconciliation certain to recalculate. So `remove_container_by_idx` plans first,
removes, then applies - and it is the single removal chokepoint, so every deletion path in the
workspace gets the expansion without having to opt in.

`SlotDeparture` exists because "no plan" was being asked to mean two different things. A hidden
container leaving changes no geometry at all and must cost the user nothing; a slot no edge can
absorb must cost a rearrangement. Collapsing them into `Option<SlotShift>` would have made every
hidden container's deletion throw away the workspace's manual boundaries.

Restore-record invalidation is now explicit rather than incidental. `plan_release` would already
have refused a record whose absorber had grown again, so the *outcome* was correct before this
phase; but the record kept claiming `exact_restore_valid`, which is a lie the state output would
have published. Marking it is the same answer arrived at honestly, and it keeps `old_rect` as the
anchor the fallback placement needs. `forget_container` also drops the departing container's own
record, which it had been leaving behind for the next recalculation to clear.

`slots_are_authoritative` was factored out of `try_local_slot_update` rather than written fresh, so
there is one definition of "the slots are the arrangement" and a local edit and a departure cannot
disagree about it.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 320 passed/1 ignored
(up from 312), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings; `cargo check -p komorebi --features
schemars` passed.

Not in this phase: destroying a container which still holds windows, which is 8B, and the command
surface for it, which is Phase 12.

#### Phase 8B - Explicit destruction and window distribution

- [x] Add explicit destruction of a container which still holds windows.
- [x] Order recipients: surviving absorbers, then active MRU, then hidden MRU, for a Hidden source;
  the expansion group for an Active one.
- [x] Distribute source windows top-to-bottom, round-robin, to recipient stack bottoms, preserving
  placement, visibility, presentation and floating rectangle.
- [x] Refuse atomically when a non-empty container has nowhere to send its windows.
- [x] Add distribution-order, state-preservation, hidden-source, refusal and focus tests.
- [x] Commit as `feat: distribute the windows of a destroyed container`.

Expected handwritten change: 250-400 lines. Likely files: `workspace.rs`, `container.rs`.

Actual files: `workspace.rs` and `container.rs`, as predicted. Actual Rust change: 383 added, 0
removed, about two thirds of it tests.

The recipient order is the phase's actual content, and it is one rule rather than two. An active
container hands its windows to the same group which takes its area, so the windows and the space
they occupied travel together; a hidden container has no area to give, so it falls back to the
containers which absorbed it when it was hidden, which is where its space went. Both answers are
"follow the area", and only the way of finding it differs. The workspace MRU, active before hidden,
is underneath both, and container order underneath that, so an empty history cannot refuse an
operation the workspace can obviously perform.

Dealing top-down into stack bottoms turned out to preserve the source's own stack order, which was
not something the rule was designed for but is what makes it feel right: the destroyed container's
windows arrive together, in the order they had, underneath whatever the recipient was already
showing. `receive_window_at_bottom` is where that lives, and it also has to move the recipient's
ring index up by one, because everything it was holding has shifted; without that the recipient
would silently start showing its neighbour.

Two test expectations were written wrong and the code was right both times. The first assumed the
deal reversed the source stack, and the second assumed every window of a hidden container lands on
an absorber, which only holds when there are no more windows than absorbers. Both were corrected to
assert the behaviour rather than the guess, and the second now pins the recipient *ordering*, which
is the property that actually matters.

The minimize history is snapshotted across the destruction and restored. `forget_container` drops
the entries of every window of a departing container, which is right when the container is leaving
the workspace and wrong when only the container is going and its windows are staying. Restoring the
snapshot and then pruning to owned handles is what keeps the restore order the user had.

Refusal is by construction, as in 7C: the only thing which can fail is having nowhere to send the
windows, and that is decided before anything has been written.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 329 passed/1 ignored
(up from 320), layouts 128, bar 3. Two of the new tests assert the workspace invariant validator
directly rather than only the geometry. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings; `cargo check -p komorebi --features
schemars` passed.

Not in this phase: the shared-edge resize, which is 8C, and the command surface, which is Phase 12.

#### Phase 8C - Multi-neighbour shared-edge resize

- [x] Add a validated, not-yet-applied shared-edge resize plan to the slot map.
- [x] Move one shared boundary at a time, changing only the axis the edge belongs to.
- [x] Move every active container on both sides of the boundary together.
- [x] Clamp the delta to the legal range and refuse rather than overlap or open a hole.
- [x] Invalidate the hidden restore records the moved boundary touches.
- [x] Add axis, multi-neighbour, clamp, minimum-size, refusal and hidden-target tests.
- [x] Commit as `feat: resize an active container along a shared edge`.

Expected handwritten change: 300-450 lines. Likely files: `geometry.rs`, `workspace.rs`.

Actual files: `geometry.rs` and `workspace.rs`, as predicted. Actual Rust change: 718 added, 0
removed. That is over the range this phase
estimated; the overrun is entirely test code, and the two implementation files add 218 and 90 lines
respectively, about two thirds of it tests.

The insight the phase turns on is that a resize moves a *boundary*, not a container's edge. The
task's wording - "synchronise the containers on the other side" - is only half of it: a container
on the *near* side which shares the same line has to move too, or the column tears. So the plan
starts from the target's edge interval and grows it to a fixpoint, taking in every slot which
touches what it has grown to so far, until both sides are described by one line. The loop
terminates because the interval only grows and the work area bounds it.

Both sides are then checked to tile that line exactly. In a valid tiling the sweep always closes,
so this is a safety net rather than a normal refusal path, and the test which covers it has to use a
slot map with a hole in it to reach the check at all - which is exactly the condition it defends
against.

A positive delta grows the target whichever side the boundary is on, so no caller has to reason
about which way the axis runs. Each mover's size along the moving axis is `size + coefficient *
shift`, with the coefficient +1 on the near side and -1 on the far side, so the legal range of the
shift is one bound per mover and the clamp is a single `clamp` rather than a case analysis. A delta
which is too large settles against `MIN_SLOT_EDGE` instead of refusing, because a held-down resize
key that stops working reads as a bug.

The workspace side refuses when the slots are not authoritative. Editing them would mean adopting an
arrangement the workspace is not in, and the pending recalculation would discard the edit anyway;
refusing says so instead of pretending to work. `resize_dimensions` is deliberately untouched: it is
part of the arrangement fingerprint, and a manual boundary now lives in the slots, which is what
makes a layout change discard it - the behaviour the task asks for and a test now pins.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 349 passed/1 ignored
(up from 329), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the two pre-existing warnings; `cargo check -p komorebi --features
schemars` passed.

Two of the tests were rewritten after first passing. One asserted conditionally, so it could not
fail; the other was named for an asymmetric clamp it did not actually construct. Both now pin the
property their names claim.

Phase 8 is complete: a container's area is expanded over rather than relaid out when it goes, its
windows are shared out when it is destroyed, and a boundary can be moved without tearing the tiling.
Next phase: 9, independent floating window movement and edge resizing.

### Phase 9 - Floating move and edge resize

Split into 9A, 9B and 9C on 2026-08-30, before coding, for the same review-size reason as Phases 2,
3, 5, 6, 7 and 8. The concerns separate cleanly: 9A is arithmetic on a rectangle which needs no
window manager at all, 9B is which window an operation is allowed to act on and what Win32 says
afterwards, and 9C is the command spelling. 9C is brought forward from Phase 12 for these two
commands only, because "independent of container movement" is a claim about the command surface and
cannot be demonstrated while the only way to reach the behaviour is the existing `MoveWindow`.

The existing `move_floating_window_in_direction` is upstream's implementation and is wrong for this
model in four separate ways: it shares `resize_delta` with container resizing, it finds its subject
by querying the foreground window rather than by asking the model, it never records the result in
`floating_rect`, and it validates nothing about visibility or presentation. It is replaced rather
than extended.

#### Phase 9A - Floating geometry primitives and configuration

- [x] Add pure move planning: only `left`/`top` change, size is preserved exactly.
- [x] Add pure edge-resize planning with the task's eight edge/sizing meanings, where the named
  edge moves and the opposite edge does not.
- [x] Clamp movement so a draggable strip stays inside the work area, and sizing to a minimum size.
- [x] Scale a configured delta by the monitor's DPI factor, in one place, as an explicit function.
- [x] Add defaulted `floating_move_delta` and `floating_resize_delta` configuration.
- [x] Add move, eight-edge resize, clamp, minimum-size and DPI tests.
- [x] Commit as `feat: add floating window geometry primitives`.

Commit: `9d588595`.

Expected handwritten change: 300-450 lines. Likely files: new `floating_geometry.rs`, `lib.rs`,
`static_config.rs`, `window_manager.rs`, `state.rs`.

Actual files: `floating_geometry.rs`, `lib.rs`, `static_config.rs`, `window_manager.rs`, `state.rs`,
as predicted. Actual Rust change: 595 added, 0 removed, which is over the estimate; 335 of those
lines are the module's tests and 219 are the module itself, so the reviewable weight is inside the
range and the excess is test coverage of the eight edge meanings and the clamp cases.

The module is separate from `geometry.rs` rather than added to it, because the two answer opposite
questions. A logical slot may not overlap its neighbours and is meaningless outside a tiling; a
floating rectangle may overlap anything and means nothing to the tiling at all. Sharing a module
would have put the one kind of rectangle which must never be written into a slot next to the
functions which write slots.

The clamp is deliberately asymmetric between the axes, and that is a behaviour decision rather than
an implementation detail: a window too wide for the work area may hang off either side, because a
grabbable strip remains either way, but no window may be pushed above the top of the work area,
because the title bar is the thing being clamped for and it is at the top. Containment wins whenever
the window fits, which is the ordinary case and the task's stated default.

An edge resize derives the moving edge from the fixed one instead of adjusting both by the delta.
Adjusting both means a clamped size moves the fixed edge, which is the one thing an edge resize
promises not to do; deriving it means an over-large delta comes to rest with the opposite edge still
where the user left it.

The delta is a logical quantity scaled per monitor, so one configured value covers a mixed-DPI
desktop. `scale_delta` refuses to return zero for a non-zero configured delta: rounding a small
delta down to nothing on a 50% scale would present as a dead key binding.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 361 passed/1 ignored
(up from 349), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing `items after a test module` warning; `cargo check -p
komorebi --features schemars` passed.

Not in this phase: which window an operation is allowed to act on, Win32 application and read-back,
which are 9B, and the command spelling, which is 9C.

#### Phase 9B - Validated floating operations and Win32 read-back

- [x] Resolve the subject from the model's focused window, never from a foreground query.
- [x] Reject non-floating, minimized and presented subjects with typed reasons and no state change.
- [x] Store the planned rectangle, apply it, then read the accepted rectangle back and store that.
- [x] Leave slots, container state, focus and every other window untouched.
- [x] Add rejection, isolation, hidden-container, clamp and read-back tests.
- [x] Commit as `feat: move and resize floating windows independently`.

Commit: `7e76c9c4`.

Expected handwritten change: 300-450 lines. Likely files: `workspace.rs`, `window_manager.rs`,
`windows_api.rs`, `managed_window.rs`.

Actual files: `managed_window.rs`, `workspace.rs`, `window_manager.rs` and `process_command.rs`.
`windows_api.rs` was not needed: `position_window` already compensates for the shadow frame and
`window_rect` already returns the DWM extended frame bounds, so applying a rectangle and reading
back what was accepted are both existing calls. Actual Rust change: 576 added, 66 removed, of which
about 300 are tests and 66 removed are upstream's `move_floating_window_in_direction`.

The subject is the focused window and nothing else. `focused_floating_window` was the obvious thing
to reuse and is the wrong function here: its fallback to the first floating window in cycle order
is what entering the floating layer needs, and would make a floating command act on a window the
user never selected. Upstream's foreground-window query is wrong for a second reason - it asks the
desktop who is focused instead of asking the model, so it disagrees with the model exactly when
they have drifted apart, which is the moment a command most needs to be predictable.

The starting rectangle comes from Win32 rather than from the record. A floating window can be
dragged with the mouse without any command passing through the model, so `floating_rect` is a
record of the last commanded geometry rather than a claim about where the window is now; planning
from the stale record would make the first keystroke after a drag jump the window back.

Refusals are values, not errors. `FloatingRejection` distinguishes "you asked to move a tiled
window" from "komorebi could not move it", which is what Phase 9C needs to return distinct exit
information, and it is what lets the floating layer's existing directional move report a wrong
subject without failing a command. The state is inspected before anything is written, so a refused
command has nothing to roll back.

The read-back is the phase's only real Win32 dependency and it exists because an application may
refuse the size it is given. Recording what it settled on rather than what was asked for is what
stops the next resize from starting off a rectangle the window never had.

Nothing in either command touches a slot, a container state, a stack, a focus history or another
window, and the isolation test pins that: the slot map and the container identities are compared
before and after, and a floating window inside a Hidden container moves without the container
becoming Active.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 372 passed/1 ignored
(up from 361), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing `items after a test module` warning.

Not in this phase: the command spelling, which is 9C.

#### Phase 9C - Distinct floating commands

- [x] Add socket messages for floating move and floating edge resize with an optional delta.
- [x] Add the `komorebic` subcommands and client bindings, distinct from the container commands.
- [x] Add hand-written CLI documentation pages in the checked-in format.
- [x] Add parsing/serialization tests.
- [x] Commit as `feat: add distinct floating window commands`.

Expected handwritten change: 150-300 lines. Likely files: `core/mod.rs`, `process_command.rs`,
`komorebic/src/main.rs`, `komorebi-client/src/lib.rs`, `docs/cli/`.

Actual files: `core/mod.rs`, `process_command.rs`, `komorebic/src/main.rs`, two new `docs/cli/`
pages and `mkdocs.yml`. `komorebi-client/src/lib.rs` needed no change, because it re-exports
`SocketMessage` rather than enumerating its variants, so a new variant reaches every client for
free. Actual change: 113 lines of Rust, 43 lines of new CLI documentation and 2 navigation
entries.

The delta is `Option<i32>` rather than a required argument, so `komorebic move-floating-window left`
uses the configured step and `komorebic move-floating-window left 200` overrides it for that press
only. That is what lets one AutoHotkey script bind both a normal and a coarse step without keeping
its own copy of the configuration.

`move-floating-window` and `resize-floating-window` are separate commands rather than modes of
`move` and `resize-edge`. The existing pair act on containers and boundaries, and their names
already mean that; overloading them would have made the meaning of a key depend on the placement of
whatever happened to be focused. The floating layer's existing directional move still routes to the
same core operation, so there is one implementation and two ways in.

The CLI pages were hand-written in the checked-in format for the reason recorded in Phase 7:
`komorebic docgen` disagrees with the 177 checked-in pages, so running it to add two pages would
have rewritten every one of them.

`schema.json` is deliberately not regenerated for the two new configuration fields. A regeneration
against the current toolchain also rewrites three unrelated descriptions and repairs mojibake em
dashes in the checked-in file, which is upstream drift rather than this task's change; Phase 14
owns reconciling the generated artifacts in one place. The fields have serde defaults, so a
configuration which omits them is unaffected either way.

Verification: `cargo test --workspace -- --test-threads=1` passed with komorebi 374 passed/1 ignored
(up from 372), layouts 128, bar 3. `cargo fmt --all --check` clean; `cargo clippy --workspace
--all-targets` reported only the pre-existing `items after a test module` warning. The two new
commands' generated help was read from a release `komorebic` build, because a debug build overflows
its stack building the clap command tree, as recorded in Phase 7.

### Phase 10 - Workspace ordering, deletion, merge, and minimized restore

Split into 10A, 10B and 10C on 2026-08-30, before coding, for the same review-size reason as the
earlier phases. The concerns separate cleanly: 10A moves a workspace within its monitor's list and
changes nothing inside any workspace, 10B is the destructive operation - one workspace's entire
contents entering another's model - and 10C is the window manager wiring which has to focus, load
and retile the survivor, plus the last-minimized restore path this phase owns.

Ordering is the phase's quiet hazard rather than its visible one. A workspace's identity is its
`WorkspaceId`, but three index-keyed side tables describe workspaces by position: the monitor's
configured `workspace_names`, the global `WORKSPACE_MATCHING_RULES` application routing, and the
monitor's `last_focused_workspace`. Reordering without moving those would silently rename
workspaces and re-route applications, which the task forbids, so a reorder returns the permutation
it performed and every index-keyed table is moved with it.

#### Phase 10A - Stable workspace identity and ordering

- [x] Add stable-ID workspace lookup on `Monitor` to match the container-ID lookup on `Workspace`.
- [x] Add reorder and swap which move the workspace and return the old-index-to-new-index
  permutation, keeping focus on the same workspace by identity.
- [x] Move the monitor's configured names and last-focused index with the permutation, and remap
  the global workspace application rules for that monitor in the window manager.
- [x] Add identity, name, rule, focus, no-op and out-of-range refusal tests.
- [x] Commit as `feat: reorder workspaces by stable identity`.

Actual files: `komorebi/src/monitor.rs`, `komorebi/src/window_manager.rs`.

`WorkspaceReorder` reports the permutation rather than applying it, because the tables which
describe a workspace by position do not all belong to the monitor: the configured names, the
focused index and the last-focused index do, while the application routing rules are a global keyed
by monitor index as well. A reorder is refused whole when either index is out of range, and
reordering onto the same index is an identity permutation which writes nothing.

Reordering deliberately invalidates no geometry. Workspaces are independent of each other and of
their position, so no container, window, slot, history, ID or name changes when the list is
rearranged, and there is nothing to retile.

Cycling wraps, which keeps the operation total: the first workspace can always be moved left, and
the last can always be moved right.

Verification: `komorebi` lib tests 374 -> 386 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --all-targets` are clean apart from the pre-existing
`items after a test module` warning. Toolchain note: the stable rustfmt and Clippy in this
environment now run, unlike the earlier phases which recorded rustfmt as unavailable; `cargo fmt`
warns that `imports_granularity` is nightly-only and skips only that option.

#### Phase 10B - Workspace deletion and merge

- [x] Add the merge: every container of the source workspace enters the target with its ID, stack,
  window states and window focus history unchanged.
- [x] Merge both histories with deduplication, source order first, and inherit the source's focused
  container and window when that window is still focusable.
- [x] Invalidate the target's exact hidden restores and manual resize dimensions, leaving Hidden
  containers hidden and letting the next update relayout only the Active ones.
- [x] Add the delete-direction rule on `Monitor`: refuse the only workspace, merge the first into
  its right neighbour and every other workspace into its left neighbour, atomically.
- [x] Add first/middle/last direction, only-workspace refusal, container/history/state preservation,
  hidden-container, focus inheritance and rollback tests.
- [x] Commit as `feat: merge deleted workspaces into a neighbour`.

Actual files: `komorebi/src/workspace.rs`, `komorebi/src/monitor.rs`.

`Workspace::merge_from` re-parents containers rather than rebuilding them, which is what makes the
preservation claims true by construction: a container arrives with its ID, stack, window states,
floating rectangles and its own window focus history because the container value itself is moved.
Hidden containers need no special handling for the same reason - the state is derived from the
windows, and the windows did not change.

Two things deliberately do not survive. The manual resize dimensions and every exact hidden restore
describe an arrangement which no longer exists, so they are discarded and the next update
recalculates the slots of the active containers alone. A preselect container is a transient
insertion marker indexing a ring which is about to change, so both sides drop theirs, and the
source's monocle reference is dropped because it claims a whole work area the target's containers
are about to share.

Deleting shifts the same index-keyed tables reordering does, but not identically: the name of a
deleted workspace is dropped, while its application routing rules follow its windows to the
workspace which absorbed them. `WorkspaceReorder` therefore answers two questions -
`new_idx` for what describes the workspace and `content_idx` for what describes its windows - and
the focused index follows the contents, so deleting the focused workspace lands on the survivor.

Verification: `komorebi` lib tests 386 -> 400 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 10C - Window manager wiring and minimized restore

- [x] Add the window manager operation: merge, focus the survivor, load it and retile it.
- [x] Route the existing workspace-closing command through the merge so a workspace is never
  removed by a path which could strand what it held.
- [x] Confirm the last-minimized restore path drives placement, presentation, both MRUs and the
  Hidden-to-Active transition, and add the tests this phase owes it.
- [x] Raise a restored window to the top of its container's stack, which the model requires and the
  restore path did not do.
- [x] Add end-to-end merge, focus-follow, and restore tests.
- [x] Commit as `feat: merge workspaces through the window manager`.

Actual files: `komorebi/src/window_manager.rs`, `komorebi/src/process_command.rs`,
`komorebi/src/container.rs`, `komorebi/src/workspace.rs`, `komorebi/src/monitor.rs`.

`Monitor::merge_workspace` returns the rearrangement rather than the target index, which makes it
the same shape as reorder and swap and lets one caller answer both questions the permutation
covers. The window manager remaps the routing rules immediately after the model changes and before
it shows or retiles anything: a test with unreal window handles caught the first ordering, where a
Win32 focus failure returned early and left the rules pointing at a workspace which no longer
existed. That is an atomicity rule, not a test artefact - the model's own tables have to settle
together, and only the desktop work may fail afterwards.

`CloseWorkspace` keeps its long-standing condition that the workspace is empty and unnamed, but it
now deletes through the merge, so it picks its neighbour by the model's direction rule instead of
decrementing the index. Its floating-window and monocle conditions were dropped as redundant rather
than relaxed: both are derived from container membership in this model, so an empty container ring
already implies them. The unguarded delete-and-merge command is Phase 12's, with the rest of the
command surface.

The restore path needed one behaviour it did not have: a restored window returns to the top of its
container's stack. `Container::raise_window` is a change of depth rather than of membership, and it
moves the ring focus with the windows it indexes so a container goes on showing what it was
showing.

Two tests were corrected after they failed. `a_restored_window_keeps_the_placement_it_was_minimized_with`
addressed its window by depth, which raising changed; it now addresses it by handle and asserts the
depth separately. The new restore test asserted that the windows a raise passed came out reversed,
which was simply wrong about correct behaviour: they keep their order.

Verification: `komorebi` lib tests 400 -> 406 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --all-targets` clean apart from the pre-existing
`items after a test module` warning.

### Phase 11 - Cross-monitor container/workspace migration

Split into 11A, 11B and 11C on 2026-08-30, before coding, for the same review-size reason as the
earlier phases. The three concerns are what happens to a rectangle which no slot describes, what
happens to a container which arrives from somewhere else, and the window manager wiring which has
to take one side apart and put the other together without a step in between that could fail.

The phase's hazard is that a monitor transfer crosses a coordinate system. Every stored rectangle
in the model is in physical pixels on one monitor's work area: a logical slot, a floating
rectangle, a manual resize dimension. A slot is recalculated on arrival and a manual resize is
discarded, so the only rectangle which has to be carried across is the floating one - and it is
the only rectangle the arrangement will never correct, because nothing else ever writes it.

#### Phase 11A - Floating rectangles across work areas

- [x] Add a pure work-area transfer to `floating_geometry`: position and size scale with the ratio
  between the areas, then the result is clamped into the target with the existing rule.
- [x] Apply it across a container and across a whole workspace, touching only stored floating
  rectangles and nothing else about a window.
- [x] Add identity, mixed-DPI, clamping, and untouched-stored-window tests.
- [x] Commit as `feat: carry floating rectangles between work areas`.

Actual files: `komorebi/src/floating_geometry.rs`, `komorebi/src/container.rs`,
`komorebi/src/workspace.rs`.

`transfer_between_areas` scales position and size by the ratio between the areas and then reuses
`clamp_position`, so the two rules a transfer needs stay one rule each: relative placement is the
scaling, reachability is the clamp. Scaling the size as well as the position is deliberate. A
window moved from a 1920-wide work area to a 2560-wide one would otherwise keep its pixel width and
occupy a visibly smaller share of the new display, which reads as the window having shrunk rather
than as the monitor having changed.

Two degenerate inputs are handled rather than refused, because both are states a monitor
reconfiguration briefly reports: a source area with no width or height has no ratio to scale by and
only clamps, and a scaled size is floored at one pixel so a window can never collapse to nothing.

Only stored floating rectangles are rewritten. A stored window's rectangle comes from a slot the
receiving workspace is about to recalculate in its own coordinates, and a window which has never
floated has no rectangle to carry: it will be given one by the work area it is in when it first
floats.

Verification: `komorebi` lib tests 406 -> 416 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 11B - Container adoption by a foreign workspace

- [x] Add adoption on `Workspace`: an arriving container fills an empty workspace, halves the
  geometry-focused active container's slot when the workspace is occupied, and takes no slot at all
  when it arrives Hidden.
- [x] Keep the container's ID, stack, window states and window focus history, and carry its
  floating rectangles into the target work area.
- [x] Add the `Monitor` transfer which removes with absorption on one side and adopts on the other,
  atomically.
- [x] Add empty-target, occupied-target 50:50, Hidden, preservation, absorption and refusal tests.
- [x] Commit as `feat: adopt containers into a foreign workspace`.

Actual files: `komorebi/src/workspace.rs`, `komorebi/src/monitor.rs`.

`Workspace::adopt_container` moves the container value, which is what makes its preservation claims
true by construction rather than field by field: the ID, the stack order, the window focus history
and every window's placement, visibility, presentation and floating rectangle are the container
itself. Its windows already name it as their container, so unlike `adopt_managed_window` there is
nothing to restamp.

Only placement is decided, and the rule follows the container's own state rather than where it came
from. This is why `ContainerArrival` is a separate type from `NewWindowPlacement` despite the two
sharing three of their four outcomes: adoption has an outcome placing a new window cannot have,
which is arriving with nothing for the arrangement to place. A hidden arrival takes no slot, and
focus does not follow it either - it has no focusable window to move to, and the container which
was being shown goes on being shown.

The split path reuses `LogicalSlots::plan_split` exactly as `split_for_new_window` does, so the
division a container gets on arrival and the division a new window's container gets are the same
code and cannot drift apart. Planning before inserting is what makes a slot which cannot be halved
cost nothing but the fallback.

Exact hidden restores are discarded on arrival, which the task's invalidation list requires for a
container moving between monitors. This is the one place adoption is deliberately heavier than
`split_for_new_window`, which leaves the records to be rejected by their own validity check: a
container arriving from another work area is a topology change those records describe wrongly
rather than merely staler.

`Monitor::release_focused_container` and `Monitor::adopt_container` are the two halves the window
manager needs. Release cancels any preselect first, because a preselect marker indexes a ring which
is about to change and is not a container of the model. Adopt validates the target workspace before
it touches the container, so a bad index refuses without stranding a container between two
workspaces.

Verification: `komorebi` lib tests 416 -> 428 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 11C - Workspace migration and window manager wiring

- [x] Refuse to move a monitor's last workspace, and move the index-keyed name/last-focused tables
  and the global application rules with the workspace, as the ordering phase does.
- [x] Rewire `move_container_to_monitor` onto the adoption path so a whole container moves rather
  than the foreground window the desktop happened to report.
- [x] Rewire `move_workspace_to_monitor` and the monitor workspace swap onto the transfer, keeping
  workspace IDs and names and translating floating rectangles into the target work area.
- [x] Add end-to-end transfer, last-workspace refusal, rule-remap, Hidden and focus-follow tests.
- [x] Commit as `feat: preserve the model across monitor migrations`.

Actual files: `komorebi/src/monitor.rs`, `komorebi/src/window_manager.rs`.

`move_container_to_monitor` was moving a window rather than a container. It asked Win32 which
window had the foreground and, when that window was floating, took only that window across - a
survival from the model in which a floating window belonged to a workspace's own list rather than
to a container. It now releases and adopts the whole container, so a floating window changes
monitor as part of the container which owns it, which is what the ownership invariant requires. A
move direction, which a drag across a monitor boundary supplies, is mapped to a forced split axis
rather than to an insertion index, because under adoption an arriving container gets a half of a
slot rather than a position in a ring.

`Monitor::release_workspace` refuses a monitor's only workspace, which is the phase's stated
invariant, and returns the rearrangement its removal caused so every index-keyed description of a
workspace can follow it. It differs from the merge's rearrangement in exactly one respect, and that
respect is the whole distinction: both `new_idx` and `content_idx` answer `None` for the workspace
which left, because nothing on this monitor inherited either the workspace or its windows. The
configured name travels with the workspace rather than staying behind to describe whichever
workspace slides into the index.

Ordering is load-bearing between the two routing-rule passes, and this is the phase's real hazard.
`readdress_workspace_rules` sends the departing workspace's rules to the monitor it went to, and it
must run first, while every index still describes the list the rules were written against.
Remapping first would slide a following workspace's rule down onto the vacated index, and the
readdressing pass would then send that rule to the wrong monitor. Both passes run before anything
talks to the desktop, for the atomicity reason Phase 10C established.

`WindowManager::remove_focused_workspace` was deleted rather than left unused. It removed a
workspace by index with no refusal at all, so it could leave a monitor with none - the exact state
this phase exists to make unreachable - and after the rewiring its only remaining caller was its
own test. That test is replaced by the refusal tests which pin the behaviour it violated. This is
an intended API removal relative to upstream komorebi.

The monitor workspace swap needed the same treatment as the move on both sides: each list arrives
in a work area it was not arranged for, so floating rectangles are carried across and the slots and
manual boundaries are discarded.

Verification: `komorebi` lib tests 428 -> 441 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning. Size note: 11C came to 615 added and 131 removed lines against
a 300-450 estimate. Roughly half is tests and the rewritten `move_container_to_monitor` is a
replacement rather than an edit, so the reviewable unit is smaller than the count suggests, but the
estimate was low and is recorded as such.

### Phase 12 - Socket protocol and komorebic CLI

Split into 12A outcomes, 12B window lifecycle, 12C container lifecycle, 12D workspace ordering and
12E stable-ID transfers on 2026-08-30, before coding. The split is drawn around what a command
*answers* rather than around what it touches: 12A is the answer itself, and each of the others is a
family of operations the model has owned since an earlier phase with no way for a caller to reach
it.

The audit which opened the phase found that the command surface had fallen a long way behind the
model. Everything Phases 5 to 11 built - suspension, minimize restore, manual splitting, container
destruction, workspace reordering and merging - existed with no socket message at all, and the only
new commands since Phase 1 were the two floating ones brought forward in 9C.

#### Phase 12A - Typed command outcomes and the reply channel

- [x] Add `CommandOutcome` and `CommandResponse` with the eight answers the task names, plus a
  distinct process exit code for each.
- [x] Convert `FloatingOutcome` and `FloatingRejection` into responses, and reply from the floating
  commands.
- [x] Add `komorebic send_command`, which reads the reply, exits with the outcome's code and prints
  its detail to stderr.
- [x] Add wire-form, exit-code and conversion tests.
- [x] Commit as `feat: report typed command outcomes to komorebic` (`be1a5791`).

Actual files: new `komorebi/src/command_outcome.rs`, `komorebi/src/lib.rs`,
`komorebi/src/process_command.rs`, `komorebi-client/src/lib.rs`, `komorebic/src/main.rs`.

The transport needed nothing new. `process_command` has always taken a `reply` writer for queries,
and `send_query` already writes, shuts down its write half and reads to EOF, so a mutating command
can answer over the same connection a query does. What makes this compatible in both directions is
that neither side insists: komorebi writes the reply best-effort, because a caller which used
`send_message` has a socket nobody is reading, and komorebic treats both an empty reply and no
reply within the read timeout as "this build does not report an outcome for this command", which is
every build older than this one.

A presented window is reported as `NotFloating` rather than as an outcome of its own. What the
caller needs to know is that the window is not currently positioned by a floating rectangle; which
presentation is responsible is detail, not a different answer.

Verification: `komorebi` lib tests 441 -> 450 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 12B - Window lifecycle commands

- [x] Add `Pause`, `Unpause`, `SuspendWindow(Option<isize>)`, `ResumeWindow(Option<isize>)` and
  `RestoreLastMinimizedWindow`, with `komorebic` subcommands for each.
- [x] Answer `Ignored` or `NoTarget` for a window komorebi does not own, and `Suspended` for one
  already suspended.
- [x] Reply `NoOp` for commands which arrive while paused instead of dropping them silently, and
  process the pause commands themselves while paused.
- [x] Add pause idempotence, suspension, resume-subject and refusal tests.
- [x] Commit as `feat: expose window lifecycle commands with outcomes` (`4dab5145`).

Actual files: `komorebi/src/core/mod.rs`, `komorebi/src/window_manager.rs`,
`komorebi/src/process_command.rs`, `komorebic/src/main.rs`, five new `docs/cli` pages, `mkdocs.yml`.

Suspension and resumption take deliberately different paths, and the asymmetry is the phase's one
real design decision. Suspension is applied directly, because the caller is asking about one window
and the answer is already known: whether it was managed at all, and whether detaching it left the
model consistent. Resumption goes back through the event pipeline, because a resumed window is
processed as a newly opened one - the current monitor, the current workspace, the new-window
threshold and the routing rules decide where it lands - and none of that is the command's decision
to make.

A resume with no handle takes the foreground window when that window is suspended, otherwise the
only suspended window there is, and refuses anything ambiguous rather than guessing. Suspending a
window komorebi does not own distinguishes `Ignored` from `NoTarget` by asking the same rule
evaluation the event pipeline asks, so the caller hears which of the two it is.

Pause and Unpause are idempotent so that a hotkey which names a state leaves komorebi in that
state, whichever was pressed last, and both are in the paused allow-list - without that, `unpause`
would be dropped by the very state it exists to leave.

Verification: `komorebi` lib tests 450 -> 457 passing, full workspace suite serial and green. Docs
pages were generated with `komorebic docgen` into a scratch directory and reconciled by hand to the
checked-in format, as Phase 9C established.

#### Phase 12C - Container lifecycle commands

- [x] Add `CreateContainer(Option<SplitAxis>)` and `DestroyContainer`, with `komorebic`
  subcommands, and give `SplitAxis` a clap `ValueEnum`.
- [x] Add `commit_workspace_change`: apply to a copy, validate, commit only if consistent.
- [x] Add split, refusal, distribution and empty-workspace tests.
- [x] Commit as `feat: expose container creation and destruction` (`d8c73a29`).

Actual files: `komorebi/src/core/mod.rs`, `komorebi/src/geometry.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/process_command.rs`, `komorebic/src/main.rs`,
`komorebi-client/src/lib.rs`, two new `docs/cli` pages, `mkdocs.yml`.

`commit_workspace_change` is the phase's substance rather than the two commands. It makes "validate
before committing" structural instead of remembered: the operation runs against a clone, the result
is validated, and a candidate which breaks an invariant is thrown away with the clone. That is also
what makes the `InvariantViolation` outcome reachable at all - without it the outcome would be a
variant nothing could ever produce.

The desktop work stays outside the validated commit and after it, which is the ordering Phase 10C
established: the model settles first, and only the part which talks to Win32 may fail afterwards.
The tests rely on exactly that - a focus call for an unreal handle does fail, and the model change
it follows is still there to assert on.

Verification: `komorebi` lib tests 457 -> 462 passing, full workspace suite serial and green.

#### Phase 12D - Workspace ordering and merge commands

- [x] Add `MoveWorkspaceToIndex`, `CycleMoveWorkspace`, `SwapWorkspaceWithIndex` and
  `MergeFocusedWorkspace`, with `komorebic` subcommands.
- [x] Answer refusals rather than erroring on them: a position which does not exist, a position
  already held, a swap with itself, a monitor's last workspace.
- [x] Add ordering, swap, no-op and refusal tests.
- [x] Commit as `feat: expose workspace ordering and merge commands` (`6e252521`).

Actual files: `komorebi/src/core/mod.rs`, `komorebi/src/window_manager.rs`,
`komorebi/src/process_command.rs`, `komorebic/src/main.rs`, four new `docs/cli` pages, `mkdocs.yml`.

A position which does not exist is `NoTarget` rather than a clamp. A hotkey which meant workspace 5
on a monitor with three of them has asked for nothing, not for the last one, and silently moving a
workspace somewhere the user did not name is worse than doing nothing. The merge command is
deliberately not the older `CloseWorkspace`: that one still requires an empty, unnamed workspace,
while this one merges whatever the workspace owns into the neighbour the model's direction rule
chooses, and refuses only a monitor's last workspace.

Verification: `komorebi` lib tests 462 -> 470 passing, full workspace suite serial and green.

#### Phase 12E - Stable-ID window transfers

- [x] Add `MoveWindowToWorkspaceId(String, bool)` and `MoveWindowToContainerId(String, bool)` with
  `--follow` variants, and ID lookups on `WindowManager`.
- [x] Plan the transfer on copies of both workspaces, validate, then commit and touch the desktop.
- [x] Place into the target's MRU active container, or into a container of its own when there is
  none; a named container receives at the top of its stack.
- [x] Add refusal, no-op, empty-target, stack-position, focus-follow and emptied-source tests.
- [x] Commit as `feat: address window transfers by stable id` (`ac9a157d`).

Actual files: `komorebi/src/core/mod.rs`, `komorebi/src/window_manager.rs`,
`komorebi/src/process_command.rs`, `komorebic/src/main.rs`, two new `docs/cli` pages, `mkdocs.yml`.

The transfer spans two workspaces, so `commit_workspace_change` does not fit it; the same
discipline is applied to a pair instead, with the same-workspace case handled by cloning once. The
`--follow` flag is what separates an operator's move from a rule's move, which the task requires:
a background move must not take focus away from what the user is doing.

Two tests were corrected after they failed. Both assumed the window at the top of a test container
was the focused one; a container built by pushing windows focuses the first it was given, so the
window that travels is the first, and the tests now say so.

Verification: `komorebi` lib tests 470 -> 477 passing; the socket wire-form test added afterwards
brought this to 478. Full workspace suite serial and green. `cargo fmt --check` and
`cargo clippy --workspace --all-targets` clean apart from the pre-existing warning.

Not in this phase: no static configuration field changed, so `schema.json` and `schema.asc.json`
need no regeneration. Reconciling `komorebic docgen` with the checked-in `docs/cli` format remains
deferred to Phase 14.

#### Phase 12F - Stack depth and post-close focus

Added on 2026-08-30, when the Phase 13 shortcut audit found that the task's "raise the next window
in the stack" command (task section 7) had no socket message and no CLI subcommand. Binding a
hotkey to a command which does not exist is not an option, so the command is built before the
shortcuts which call it.

- [x] Add `Container::next_stack_window` and a workspace operation which raises it, focuses it and
  updates both MRUs.
- [x] Choose the window a removal leaves focused by validity rather than by index arithmetic: keep
  the focused window when something else was removed, otherwise take the next visible window down
  the stack and then up.
- [x] Add `SocketMessage::RaiseNextStackWindow` and the `komorebic raise-next-stack-window`
  subcommand, answering `NoOp` when the stack has nothing else to raise.
- [x] Add selection, raise, focus-history and post-close focus tests.
- [x] Commit as `feat: raise the next window in a stack`.

Actual files: `komorebi/src/container.rs`, `komorebi/src/workspace.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/core/mod.rs`, `komorebi/src/process_command.rs`,
`komorebic/src/main.rs`, new `docs/cli/raise-next-stack-window.md`, `mkdocs.yml`.

The command is deliberately not routed through `commit_workspace_change`. Raising changes depth and
focus and nothing else - no window joins or leaves a container - so there is no compound state for a
candidate to protect, and the validated-clone path exists for operations which move ownership.

`remove_window_by_idx` was the phase's real defect rather than the missing command. It focused
`idx - 1` whatever was there, which both moved focus off a window the caller had not touched and
could land on a minimized window nothing had asked to restore. The replacement answers two
different questions: a removal which did not take the shown window keeps showing the same window at
its new index, and a removal which did takes the next window which can accept focus, down the stack
first and then up. Every existing removal test passed unchanged.

Verification: `komorebi` lib tests 478 -> 487 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 12G - Window-level monitor transfer

Added on 2026-08-30 by the same Phase 13 audit. Task section 12 requires crossing a monitor
boundary to be an explicit "send this window to that monitor" command rather than something a
repeated move falls into, and section 20 asks for a shortcut which does it, but every existing
monitor command - `move-to-monitor`, `send-to-monitor` and their cycling forms - moves a whole
container. A window sharing a stack had no way to travel alone.

- [x] Add `WindowManager::send_focused_window_to_monitor`, reusing the validated two-workspace
  transfer Phase 12E built.
- [x] Rewrite a travelling floating rectangle into the receiving work area when the transfer
  crosses a monitor boundary.
- [x] Add `SocketMessage::MoveWindowToMonitorNumber(usize, bool)` and the
  `komorebic move-window-to-monitor <TARGET> [--follow]` subcommand.
- [x] Add refusal, no-op, arrival, focus-follow and floating-rectangle tests.
- [x] Commit as `feat: send a single window to another monitor`.

Actual files: `komorebi/src/window_manager.rs`, `komorebi/src/core/mod.rs`,
`komorebi/src/process_command.rs`, `komorebic/src/main.rs`, new
`docs/cli/move-window-to-monitor.md`, `mkdocs.yml`.

The floating rectangle fix is the phase's substance rather than the command. `transfer_focused_window`
has been able to cross monitors since Phase 12E, because a workspace ID names a workspace on any
monitor, but it carried the floating rectangle across unchanged - the one rectangle in the model
which no arrangement on the receiving side will ever correct. Phase 11A settled what to do about
that for containers and workspaces; this applies the same rule to a single window, so the
window-level command and the older ID-addressed one now agree.

The target is the destination monitor's *focused* workspace, which is what a shortcut means by
"the other monitor", and `--follow` separates an operator's move from a rule's move as it does
everywhere else.

Verification: `komorebi` lib tests 487 -> 491 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

### Phase 13 - AutoHotkey v2 workflow

- [x] Add a directly runnable AHK v2 example using top-level executable/config/delta variables and
  helper functions around `Run`/`RunWait`.
- [x] Cover every shortcut group in the task with Chinese comments; never emit whkd config.
- [x] Use safe stop/start restart and the version-correct static configuration replacement command.
- [x] Prefer an existing shortcut panel command; otherwise add a small AHK v2 shortcut panel.
- [x] Validate generated command lines against `komorebic --help`.
- [x] Commit as `docs: add complete AutoHotkey v2 workflow`.

Actual files: new `docs/common-workflows/komorebi-model.ahk`, new
`docs/common-workflows/autohotkey-window-model.md`, `mkdocs.yml`.

The upstream `docs/common-workflows/autohotkey.md` and `docs/komorebi.ahk.txt` were left untouched.
They document the whkd-equivalent sample this project has always shipped, and rewriting them would
have replaced an unrelated example rather than adding the model's own workflow.

`komorebic toggle-shortcuts` was rejected as the panel command after reading what it opens:
`komorebi-shortcuts` parses a `whkdrc`, so with no whkd configuration - which this task forbids -
the panel would come up empty. The task's fallback applies, and the script carries a small
AutoHotkey v2 ListView panel built from a table of its own bindings.

Command lines were validated rather than assumed. Every subcommand the script names was checked to
exist with `komorebic <name> --help` from a release build, and the enum arguments - `increase` /
`decrease`, `previous` / `next`, `left-right` / `top-bottom`, the four directions - were read out
of the same help output. The script itself was parsed by the installed AutoHotkey v2 with
`AutoHotkey64.exe /validate`, which exits 0 for this file and 2 for a deliberately broken one.

Two commands the shortcut list needs did not exist when the phase started, which is why Phases 12F
and 12G were added ahead of it: raising the next window in a stack, and sending a single window to
another monitor.

Verification: AHK v2 syntax validation clean; all 34 subcommands used by the script exist in the
release `komorebic`; no Rust source changed, so the test suite is unchanged from Phase 12G at 491
passing.

### Phase 14 - Event reconciliation, serialization, documentation, and final verification

The closing phase covers task sections 21, 22, 23 and 26. It was split into five sub-phases on
2026-08-30, after an audit of the event pipeline, the state output and the test inventory, because
the four bullets it started as are four unrelated pieces of work and only one of them is an event
question.

- [x] 14A: reclaim a suspended handle Windows has reused, and clear every per-window runtime table
  when a window is reaped rather than destroyed.
- [x] 14B: converge the model's presentation with a maximize or restore the user performed outside
  komorebi, idempotently and without changing placement or ownership.
- [x] 14C: complete the state output with the derived fields a consumer cannot compute, and version
  it so an older state document is recognised rather than silently misread.
- [x] 14D: add a deterministic seeded operation harness which drives long random operation
  sequences against the invariants.
- [x] 14E: map all 16 invariants to their implementation and tests, regenerate the schemas, run
  every available check and write the final delivery summary.

#### Phase 14A - Suspended handle identity and orphan cleanup

The audit which opened Phase 14 found two defects in what happens to a window komorebi stops
tracking without seeing it destroyed.

A suspended window is removed from `known_hwnds` and from the reaper's cache, which is correct -
komorebi no longer manages it - but it means nothing will ever notice that the window has gone. Its
handle stays in `temporarily_unmanaged_hwnds` for the lifetime of the process, and Windows reuses
window handles freely: the next window to be given that numeric handle is silently suppressed from
management forever, with no command able to fix it because the resume path refuses a window it does
not own.

The reaper is the second. It removes a vanished window from its workspace and from `known_hwnds`,
but not from `HIDDEN_HWNDS`, `already_moved_window_handles` or a pending move operation, all of
which the destroy path clears. A crashed application therefore leaves entries behind which a reused
handle will read as its own.

- [x] Give the suspension set an identity per handle - the owning process - and validate it on
  every consultation, so a handle which now names a different window is reclaimed instead of
  suppressed.
- [x] Keep the identity check off the hot path: an event for a handle the set does not hold makes
  no Win32 call at all.
- [x] Prune the whole set where a stale entry would change an answer: the resume subject heuristic
  and the suspend and resume commands.
- [x] Clear the per-window runtime tables when the reaper removes an orphan, exactly as the destroy
  path does.
- [x] Add reuse, death, pruning and orphan-cleanup tests against an injected identity source.
- [x] Commit as `feat: reclaim reused and orphaned window handles`.

Actual files: new `komorebi/src/suspension.rs`, `komorebi/src/lib.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/process_event.rs`, `komorebi/src/reaper.rs`,
`komorebi/src/invariants.rs`, `komorebi/src/static_config.rs`.

`HashSet<isize>` became `SuspensionSet`, which holds a `SuspendedWindow` per handle rather than a
bare number, and the difference between its two lookups is the phase. `contains` is the plain
membership question and asks Win32 nothing, which is what invariant validation needs; `claims` is
the question the event path asks, and it gives the entry up when the handle no longer names the
window which was suspended with it.

Staleness is anchored on what was recorded, not on liveness alone: an entry keeps the owning
process it was suspended with, and it is stale exactly when the handle does not currently name a
window of that process. That covers both the dead handle and the reused handle with one comparison.
An entry recorded without an identity - which in a running window manager means Win32 refused to
answer about a window it was managing a moment earlier - has no anchor to contradict, so it is only
ever given up explicitly. That is deliberately the old behaviour, and it is what keeps the
suspension semantics of the existing tests, whose handles name no real window, unchanged.

The identity source is injected through a `WindowIdentity` trait so the reuse and death cases are
testable without a desktop session. `Win32Identity` is the only production implementation and the
default for every method which does not name one.

The reaper fix is the smaller half but the same defect. It removed an orphan from its workspace and
from `known_hwnds` while leaving `HIDDEN_HWNDS`, `already_moved_window_handles` and a pending move
operation pointing at a handle whose window no longer exists - all of which the destroy path clears.
`WindowManager::forget_window` is now the single answer to "this window is gone", and the reaper
calls it.

Verification: `komorebi` lib tests 491 -> 503 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 14B - External presentation convergence

Since Phase 5D the model has owned a window's presentation, and the retile reapplies it. Nothing
reads it back. A user who restores a maximized window the way every Windows user does - the title
bar, the system menu, the application's own shortcut - was therefore fighting komorebi, which
maximized the window again at the next retile and reported it as maximized until a command said
otherwise.

- [x] Add `Presentation::observed` and `Presentation::reconcile`: a pure rule from a Win32
  observation and a record to the presentation the record should become.
- [x] Add `ManagedWindow::adopt_presentation`, which applies that rule to the model alone and
  touches nothing else about the window.
- [x] Reconcile from the event path, next to the existing restore-from-minimize reconciliation, for
  the events which mean the window is in a settled visible state.
- [x] Leave a window alone while komorebi is animating it.
- [x] Add rule-table, idempotence, ownership-preservation and unowned-window tests.
- [x] Commit as `feat: follow a window out of a presentation komorebi recorded`.

Actual files: `komorebi/src/managed_window.rs`, `komorebi/src/workspace.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/process_event.rs`.

Which observation to believe is the whole phase. Only the maximized state bit is ever acted on, and
only in the direction of leaving what komorebi recorded.

`is_zoomed` is a window state which Windows sets synchronously, so "komorebi recorded maximized and
the window is not maximized" is a fact about the user rather than a race with komorebi's own call.
Fullscreen is a *rectangle*, and it is the rectangle komorebi itself writes: an observation which
disagrees with a fullscreen record cannot tell "the application left fullscreen" apart from "the
rectangle has not landed yet", and one which agrees with no record cannot tell an application's own
fullscreen apart from a window which simply fills its monitor. Acting on either would make
komorebi's own fullscreen command flap, so neither is acted on. Entering maximized from normal is
not believed either: the retile already restores a tiled window which was maximized by hand, and
believing it here would let an application which is slow to drop its maximized state undo an
unmaximize command.

A window komorebi is animating is skipped, because its rectangle is somewhere between where it was
and where komorebi is putting it and no observation taken there means anything.

The minimize event was left trusting its own report rather than confirming it against `IsIconic`.
The confirmation is not reliably true yet at `EVENT_SYSTEM_MINIMIZESTART`, and the case it would
have caught already converges without it: restoring a window produces a show or focus event, and
the reconciliation which runs there marks the window visible again.

Verification: `komorebi` lib tests 503 -> 517 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 14C - State output completeness and versioning

An audit of the state output against task section 22 found it complete but for one field, and
missing any way to tell which model wrote it.

- [x] Serialize a container's derived `ContainerState`, without storing it on the container.
- [x] Keep the schema honest by describing the serialized shape rather than the stored one.
- [x] Add a state document version, defaulting to the unversioned document every older komorebi
  wrote.
- [x] Refuse to apply a document from another version instead of letting serde fill in defaults.
- [x] Add version, derived-state and output-completeness tests.
- [x] Commit as `feat: version and complete the state document`.

Actual files: `komorebi/src/container.rs`, `komorebi/src/state.rs`,
`komorebi/src/window_manager.rs`.

Everything else section 22 asks for was already there: stable workspace and container IDs, the
active logical rectangles with their geometry generation, the hidden restore records with their
`old_rect` and `exact_restore_valid`, per-window placement, visibility, presentation and floating
rectangle, both focus histories, the minimize history and the layout. A single test now asserts
that list against a serialized `State`, so a field cannot quietly stop being published.

`ContainerState` was the exception, and it is exactly the field a consumer cannot compute: deciding
whether a container is Active means knowing that a window is both visible and stored, which is the
model's rule rather than a fact in the document. It is derived on every read and never stored, so
publishing it needed a serialized shape which differs from the stored one. `ContainerRepr` already
existed for reading containers back; it now also describes the schema, a borrowed `ContainerView`
writes them, and `Container` gets a hand-written `Serialize` and `JsonSchema` which delegate to
those. Reading a document which carries the field ignores it, because the windows decide it again.

The version exists because a state document is a model, not a configuration. Serde will happily
default an absent container identity, absent logical slots or an absent placement, and the result
is a model which never held rather than an older one. `apply_state` now refuses any document which
does not carry the current version, and `StateVersion::MANAGED_WINDOW_MODEL` is the first value:
every document written before this work is `UNVERSIONED` and is not applied. Configuration
compatibility is unaffected - that is `StaticConfig`, where every added field has a serde default.

Verification: `komorebi` lib tests 517 -> 522 passing, full workspace suite serial and green.
`cargo fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing
`items after a test module` warning.

#### Phase 14D - Seeded operation harness

- [x] Add a deterministic seeded harness which drives random workspace operations and checks the
  invariants after every one.
- [x] Check the rules which only hold for a recorded arrangement against a settled copy, so both
  "in flight" and "at rest" are covered.
- [x] Assert that a refusal changes nothing, for the operations whose refusals are the model's own.
- [x] Assert that the harness reaches hidden containers, stacks, floating and minimized windows and
  an empty workspace, so a green run means something.
- [x] Fix what it found and add a named regression test for it.
- [x] Commit as `feat: drive the model with seeded random operations`.

Actual files: new `komorebi/src/model_harness.rs`, `komorebi/src/lib.rs`,
`komorebi/src/workspace.rs`.

No property-testing dependency was added. `proptest` is not in the tree, the plan allows a
deterministic harness instead, and a plain xorshift generator gives the one thing shrinking would
have given here: a failure names a seed and a step which reproduce it exactly.

The harness found a real defect on its second run, which is the entire justification for writing
it. `split_for_new_window`, `create_container_from_donor` and `adopt_container` planned a split
against the current slots and then called `adopt_slot_geometry` unconditionally. When the workspace
already owed a full recalculation - after a layout change, a merge, a monitor move - the slots it
halved were the *old* arrangement, and adopting them cleared the pending recalculation. The
sequence the harness found was: change the layout, destroy a container while the recalculation is
outstanding so the survivor keeps its old half, then open a window. From that point the workspace
tiled half of its work area and nothing ever put it back. `plan_authoritative_split` now refuses to
plan against slots which are not the arrangement, so those callers take their existing fallback and
the recalculation survives.

Two harness assumptions were wrong rather than the model, and both are recorded in the harness
itself. A container becomes Hidden the moment its last visible stored window does but gives its
slot up when the arrangement is next recorded, so "a hidden container holds no slot" is a rule
about a recorded arrangement. And the window lifecycle operations end by asking Windows to focus or
position a window and report *that* failure after the model has committed, so in a harness whose
handles name no real window their errors say nothing about whether the model changed; the
no-change-on-refusal property is asserted for the operations which decide entirely within the
model.

Verification: `komorebi` lib tests 522 -> 527 passing, full workspace suite serial and green. The
regression test was confirmed to fail against the unfixed code before being kept. `cargo fmt
--check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing `items
after a test module` warning.

#### Phase 14E - Invariant map, schemas, documentation and final verification

- [x] Write the model reference: the three window classes, the window state dimensions, Active and
  Hidden containers, the hide-and-restore algorithm, the three rectangles, the configuration
  fields, the state document and the differences from upstream.
- [x] Map all 16 invariants to the code which implements them and the tests which check them, with
  every citation verified to exist.
- [x] Regenerate the schemas.
- [x] Run every available check and record what was not run and why.
- [x] Commit as `docs: map the model invariants and regenerate the schema`.

Actual files: new `docs/window-model.md`, `mkdocs.yml`, `schema.json`.

`schema.json` had been stale since Phase 9A: `floating_move_delta` and `floating_resize_delta` were
added to the static configuration then and the schema was never regenerated. Regenerating also
picked up three upstream docstring changes which predate this work, and replaced the mojibake em
dashes the previously committed file carried - it had been generated through a console which was
not UTF-8. `schema.asc.json` regenerates identically and was left alone.

The generated notification schema was checked as well, because Phase 14C hand-wrote `Container`'s
`Serialize` and `JsonSchema`: it reports `state` among the container's properties, `version` among
the state's, and `StateVersion` as a definition, so the published schema and the published document
agree.

One environment note which is not a defect: `cargo run --package komorebic -- static-config-schema`
overflows the stack in a *debug* build. The release binary generates every schema without
complaint, and a release binary built before this phase behaves the same way, so this is a debug
stack-size limitation in schema generation rather than anything this work introduced.

Verification: `komorebi` lib tests 527 passing, full workspace suite serial and green. `cargo
fmt --check` and `cargo clippy --workspace --all-targets` clean apart from the pre-existing `items
after a test module` warning.

### Phase 15 - Runtime integration, deployment, and live verification

Added on 2026-08-30 after the completed model was installed and the user's existing automation was
found to target a different, zone-enabled branch. The installed `komorebi.exe` and
`komorebic.exe` already report commit `57f1ee0d`, but the active configuration still selects
`Append`, gives its workspaces no layout, and carries zone fields this repository has never read.
The AutoHotkey layer sends those unavailable zone messages through a PowerShell named-pipe daemon,
and the remaining PowerShell state readers still expect a container to serialize bare `Window`
objects instead of `ManagedWindow`.

This phase changes deployment files in `%USERPROFILE%` and `%ProgramFiles%` as well as recording the
work here. Every overwritten user file and installed binary is copied to a timestamped backup before
replacement.

- [ ] 15A: migrate `komorebi.json` from the unavailable zone model to `Create` plus explicit BSP
  layouts, retain all application/display/bar rules, and add the two floating delta fields.
- [ ] 15A: give the retained PowerShell readers one compatibility accessor for both the new
  `ManagedWindow.window` state shape and the former bare-window shape.
- [ ] 15B: install the validated AutoHotkey v2 workflow which calls `komorebic` directly and covers
  the full model command surface; remove the daemon and zone operations from the active lifecycle.
- [ ] 15B: update start, stop, watchdog, reload, reset, and legacy zone wrapper behaviour so none can
  emit an unavailable socket message or silently undo the new-window allocation policy.
- [ ] 15C: run static configuration checking, PowerShell parsing, AutoHotkey v2 validation, format,
  workspace check, Clippy, and the serial workspace suite.
- [ ] 15C: build release binaries, verify their embedded commit/version, back up the installed
  binaries, replace them, restart the live environment, and verify managed ownership, active slots,
  socket replies, hotkey process health, and logs.
- [ ] Commit the phase as `fix: deploy the managed window workflow`.

Planned repository file: this document. Planned deployment files:
`%USERPROFILE%\komorebi.json`, `komorebi-hotkeys.ahk`, `komorebi-common.ps1`,
`komorebi-ops.ps1`, `komorebi-mru.ps1`, `komorebi-start.ps1`, `komorebi-stop.ps1`,
`komorebi-watchdog.ps1`, `komorebi-reload.ps1`, `komorebi-reset-workspaces.ps1`,
`komorebi-zones.ps1`, and `komorebi-zone-resize.ps1`.

### Phase 15 - User environment integration

Added after the fourteen implementation phases were complete, because the model shipped correctly
but the machine running it did not pick it up. This phase changes no Rust: it is the migration of
the user's own configuration and script layer from the pre-existing zone fork onto this model.

The reported symptom was "the new komorebi does not manage windows and most shortcuts do nothing".
komorebi itself was healthy throughout - the process ran, Win32 events were processed, windows were
in containers and the socket answered queries. Four separate environment defects produced the
symptom:

1. `komorebi.json` declared `zone_states`, `zone_state_cycle` and a per-workspace `zone_state`.
   None of those keys exist in this repository and serde ignored them silently, so no workspace
   declared `layout`, `custom_layout` or `tile`; `Workspace::tile` therefore defaulted to `false`
   and nothing ever placed a window. The zone fork tiled from its own layer and did not need
   komorebi's tiling; this model does.
2. `window_container_behaviour` was `Append`, which routes every new window into the focused
   container and bypasses `place_new_window` entirely - that is, it disables the Phase 7 threshold
   placement which is the core of the new-window model.
3. The resident PowerShell layer read `container.windows.elements[].hwnd`. Since Phase 2B those
   elements are `ManagedWindow` values whose Win32 window is nested under `.window`, so every read
   produced `$null` and every operation computed an empty target and "succeeded" without acting -
   which is why no error was ever logged.
4. The named-pipe daemon sent `ZoneStateCycle`, `ZoneState`, `ZoneStateDistribute`, `ZoneResize`,
   `FocusZone` and `MoveToZone`, and the CLI wrappers called six matching subcommands. None have
   ever existed in this repository or upstream; `git log -S` finds no trace of them. komorebi
   rejected each one with `unknown variant`.

Resolution, in dependency order:

- `komorebi.json`: `window_container_behaviour` set to `Create`; every workspace given
  `"layout": "BSP"`; the three zone keys removed; `floating_move_delta` and `floating_resize_delta`
  added, matching the Phase 9 configuration fields.
- Hotkeys: `komorebi-hotkeys.ahk` reduced to a thin wrapper which `#Include`s this repository's
  `docs/common-workflows/komorebi-model.ahk` and calls `komorebic.exe` directly, replacing the
  pipe-and-daemon transport. It re-points only `!^s`, `!^q` and `!+r` at the existing PowerShell
  lifecycle scripts, which also start the bar, the watchdog and the elevation task. Both scripts
  pass `AutoHotkey64.exe /validate`.
- `komorebi-start.ps1`: no longer runs `stack-all` on cold start - collapsing every workspace into
  one container is precisely what the threshold model must not do - and no longer starts the MRU
  recorder or the daemon, whose two jobs are now owned by `Workspace` and `Container`.
- `komorebi-common.ps1`: one compatibility boundary, `Get-StateWindow`, reads either document
  shape. `Get-Zone` now takes its geometry from `logical_slots`, keyed by `ContainerId`, instead of
  indexing `latest_layout` by container position: `latest_layout` holds only active containers, so
  a single hidden container shifted every later index and handed each container a neighbour's
  rectangle. Slotless containers are carried with `HasSlot = $false` so `Set-FocusedZone` can still
  find a hidden container by ID while `Get-DirectionalNeighbour` skips it, matching the rule that
  directional operations need an active slot. `Get-FocusedWindowId` looks the focused container up
  by `Index` rather than treating the filtered list as index-aligned. `Get-LayoutSignature` folds in
  `logical_slots.generation`, which changes on hide, restore and absorption even when the rendered
  rectangle count does not.
- The zone entry scripts were reduced to compatibility shims: cycling maps to `cycle-layout`, edge
  dragging to `resize-edge`, and the named-state and numeric-zone entry points now fail with a
  message naming the commands which replace them, rather than emitting a message komorebi will
  reject.

Verification: all 67 socket message names the PowerShell layer can emit were checked against
`komorebic socket-schema`, and every `komorebic` subcommand the script layer invokes against
`komorebic --help`; both sets are now fully covered. `Get-Zone` was exercised against live state and
returns six gap-free, non-overlapping slots exactly covering the work area. Since the restart which
picked up the new configuration the runtime log contains no `unknown variant` and no
`no container or monitor in this direction`; the thirty prior occurrences all predate it.

One incident, the same class as the Phase 13 one and for the same reason. `/validate` was passed to
AutoHotkey through the Bash tool, which rewrote it into a path, so AutoHotkey was handed a
nonexistent script and left a process holding an error dialog on the user's desktop. It was
identified from its command line, killed, and the check re-run through PowerShell where the switch
survives intact. The Phase 13 rule - ask a command what it would do rather than running it - holds,
but it is not sufficient on its own: on this machine a command aimed at the live desktop must also
be checked for shell rewriting before it is sent.

### Phase 16 - Startup adoption: four workspaces and one container

Added on 2026-08-31 after Phase 15 put the model into service. The request is about the state
komorebi is in the moment it opens, which is a part of the model no earlier phase specified: every
monitor is to come up with four workspaces, every window already on the desktop is to be adopted
into the first workspace's first container, and the remaining workspaces are to hold nothing.

What already holds: `WindowsApi::load_workspace_information` enumerates the desktop into the
monitor's *first* workspace and prunes the windows which belong to another monitor, so initial
adoption already targets workspace one and only workspace one. The cold-start script passes
`--clean-state`, so `apply_state` does not run and this is genuinely the state komorebi opens in;
the watchdog's recovery path deliberately does not pass it, and restoring a crashed session's
arrangement must go on overriding this.

What does not hold: the enumeration gives every window a container of its own
(`windows_callbacks::enum_window` pushes one `Container` per window), and the workspace count comes
only from `komorebi.json`, so a monitor the configuration does not name is born with one workspace.

- [x] 16A: `Workspace::consolidate_containers`, folding every container into the first one with
  each window's placement, visibility, presentation and floating rectangle intact, the stack order
  of the ring preserved top to bottom, both histories left describing only what still exists, and
  the arrangement invalidated. Unit tests in `workspace.rs`.
- [x] 16B: apply it to initial adoption, behind a `window_adoption_behaviour` configuration field
  with a serde default, so the upstream one-container-per-window adoption stays reachable.
- [x] 16C: `default_workspace_count`, a configuration field with a serde default of four, applied
  to every monitor as it is loaded and as it is connected, so the count does not depend on a
  monitor being named in the configuration. Configuration which asks for more still gets more.
- [x] 16D: regenerate `schema.json`, document both fields, update this plan, rebuild, install and
  verify against the live desktop.

Planned files: `komorebi/src/workspace.rs`, `komorebi/src/windows_api.rs`,
`komorebi/src/static_config.rs`, `komorebi/src/lib.rs`, `komorebi/src/monitor_reconciliator/mod.rs`,
`komorebi/src/core/mod.rs`, `schema.json`, `docs/`, this plan.

16A actual files: `komorebi/src/workspace.rs`.

The fold is built out of `remove_container_by_idx` and `receive_window_at_bottom` rather than out
of a new path, so it inherits what those already guarantee: the departing container leaves the slot
map, the container focus history and the monocle reference behind it, and each arriving window is
restamped, hidden if it was a visible stored window, and given the oldest place in its new
container's focus history. Two things had to be added around them. The minimize history is
snapshotted and put back, because dropping a container's minimize entries is right when its windows
leave the workspace and wrong when only the container does. And the target is re-resolved by ID
after every removal, because the ring re-anchors locked containers when one is taken out, so a
position is not a stable reference across a removal - which is also why a locked container is
folded around rather than into.

Verification: komorebi 527 -> 534 passing, full serial suite green; `cargo fmt --check` clean;
`cargo clippy --workspace --all-targets` clean apart from the pre-existing `items after a test
module` warning.

16B actual files: `komorebi/src/core/mod.rs`, `komorebi/src/lib.rs`,
`komorebi/src/static_config.rs`, `komorebi/src/windows_api.rs`.

The fold is applied at the end of `load_workspace_information`, after the windows belonging to
other monitors have been given back, so nothing is folded in and then taken out again. The
behaviour travels as a global rather than through the call, because the enumeration runs from
`WindowManager::init` with no configuration in reach; `apply_globals` sets it during preload, which
happens first. A reload stores it for the next start rather than for this one, and that is what the
comment on it says, because the desktop has already been adopted by then.

`window_adoption_behaviour` is absent-means-unsaid like every other field here: a configuration
which does not mention it leaves the global alone, so a reload cannot silently reset it, and an
existing `komorebi.json` deserializes unchanged.

Verification: komorebi 534 -> 537 passing, full serial suite green; `cargo fmt --check` clean;
Clippy clean apart from the pre-existing warning.

16C actual files: `komorebi/src/lib.rs`, `komorebi/src/static_config.rs`,
`komorebi/src/windows_api.rs`, `komorebi/src/monitor.rs`.

`monitor::new` was left building a monitor with one workspace and the count applied in
`WindowsApi::load_monitor_information` instead, because that is the one place a display becomes a
monitor komorebi holds - it is called both from `WindowManager::init` and from the reconciliator
when a monitor is plugged in - while `monitor::new` is also the constructor a dozen tests use to
build a monitor value. `ensure_workspace_count` only grows a list, so configuration naming more
workspaces still gets them and a monitor restored from the cache keeps what it had. A configured
count of zero is stored as one: a monitor with no workspace has nowhere to put a window.

Verification: komorebi 537 -> 539 passing, full serial suite green; `cargo fmt --check` clean;
Clippy clean apart from the pre-existing warning.

16D actual files: `schema.json`, `docs/window-model.md`, this plan. Deployment files:
`%USERPROFILE%\komorebi.json` (backed up as `komorebi.json.bak-20260831-072003`), and the four
installed binaries.

The schema was regenerated from the release `komorebic`, as in Phase 14E: both new fields carry
their descriptions and defaults, and `check_schema_docs.py` reports neither of them among its 644
pre-existing missing docstrings. It has to be run with `PYTHONUTF8=1` on this machine, where
Python's default encoding is GBK and the schema is UTF-8.

The live verification is the phase's real result. Cold start with `--clean-state` on commit
`2d49746d`: one monitor, four workspaces, workspace one holding a single active container with all
five windows in it, the foreground window on top of its stack and focused, one logical slot exactly
covering the work area, three workspaces with no containers at all, and no ERROR or WARN in the log
since the process booted.

A bounded command smoke test followed, chosen so that it returns the desktop to the state it
started in. `create-container` split the work area 50:50 with the new container on the left and the
donor's most recently focused window in it; `focus right` and `focus left` moved between them;
`raise-next-stack-window` answered exit 10 on a single-window container and reordered the
four-window stack without changing its membership; `move-floating-window` answered exit 11 on a
stored window; `toggle-float` left the window in the container it was already in; a 120-unit move
and a 100-unit right-edge increase moved 150 and 125 physical pixels on this 125% display, changed
only the floating rectangle's x and width respectively, and left the slot generation untouched;
`destroy-container` gave the lone window back at the bottom of the surviving stack, expanded that
container over the whole work area, and focused its own top window rather than following the window
it received. Both AutoHotkey scripts pass `/validate` and every `komorebic` subcommand they name
exists in the new binary.

Verification: komorebi 539 passing; `cargo fmt --check` clean; Clippy clean apart from the
pre-existing warning; schema regenerated; live desktop verified as above.

### Phase 17 - The shortcut panel key

Added on 2026-08-31. The report was that `Alt+I` does not open the shortcut panel, and that the
panel is wanted as the surface for testing the rest of the model by hand.

The cause is a key that moved. The user's pre-Phase-15 script bound the Chinese cheat sheet to
`Alt+I`; the script this repository ships bound it to `Alt+/`, and Phase 15 replaced the former
with the latter. Nothing at all is bound to `Alt+I`, so the key does nothing - there is no failure
in komorebi, in the socket, or in the hotkey layer. Everything else was healthy while this was
diagnosed: komorebi answered `state`, was not paused, and held the desktop's five windows.

- [x] 17A: bind `Alt+I` to `ToggleShortcutPanel`, keeping `Alt+/` as a second entry, and add a tray
  menu entry so the panel is reachable when a hotkey is swallowed by an elevated foreground window.
- [x] 17B: the panel's own `ignore_rules` entry. The panel is a captioned window with
  `WS_EX_WINDOWEDGE`, which is exactly what `window_is_eligible` looks for, so komorebi would give
  it a slot and shift the tiling every time it is opened. The user's configuration already ignored
  the *old* panel by its exact title, `komorebi 快捷键速查`, which the new panel's title does not
  match; the rule is now `StartsWith` on `komorebi 快捷键`, which covers both.

Two things were tried and rejected on evidence rather than taste. `+ToolWindow` alone does not take
the panel out of `window_is_eligible` - a tool window still carries `WS_EX_WINDOWEDGE` - and
stripping that bit between `Gui()` and `Show()` does not survive `Show()`, which puts it back. The
option is kept for the taskbar-button behaviour only, and the script now says so rather than
claiming a protection it does not provide. An `ignore_rules` entry is the mechanism komorebi
actually offers here, and it is what komorebi's own `komorebi-shortcuts.exe` relies on.

Verification was end to end, not by inspection. The hotkey layer runs elevated - deliberately, for
UIPI - so it cannot be replaced from an unelevated shell; deleting `hotkeys.pid` makes the
watchdog relaunch it with the same token, and `#SingleInstance Force` retires the old instance.
A probe script sending at `SendLevel 1` (AHK hotkeys ignore level-0 input from other AHK scripts)
pressed `Alt+I` from its own caption-less window: from a clean start the panel opened, a second
press closed it, and `Alt+/` opened and closed it again. With the panel open, `komorebic state`
listed the same five managed windows and not the panel, so the ignore rule holds. The panel was
closed again afterwards.

One thing recorded because it nearly produced a wrong answer: the first Win32 enumerator written to
cross-check the AHK probes declared `GetWindowTextW` without `CharSet.Unicode`, so every title came
back truncated to one character and the panel looked absent when it was on screen. The AHK probes
were right and the cross-check was wrong.

### Phase 18 - The window a manual split leaves behind

Added on 2026-08-31. The report was that creating a container appears to work - the arrangement
divides - but the created half draws nothing; pressing the key again fills the half which was empty
and leaves the newest one empty, and so on down the chain.

The cause is one the model never had to face until a container could give a window away. A stack
shows the window on top of it and hides every window underneath, so the moment
`create_container_from_donor` takes the donor's most recent window out, the window left in charge
of the donor is one komorebi hid when it was pushed down the stack, and nothing told it that it is
now the window being shown. The donor keeps half of the work area and draws an empty rectangle in
it. Only the *moved* window came out looking right, and by accident: the created container takes
focus, and `raise_and_focus_window` passes `SWP_SHOWWINDOW`, so the focus call revealed it. That
accident is also the whole of the chain the user saw - each press revealed the window the previous
press had left hidden.

- [x] 18A: `create_container_from_donor` loads the donor's focused window after the removal, the
  way `take_window` and `move_window_to_container` already do on the same removal, and loads the
  created container's focused window after the arrival, so that what a container shows is decided
  by the container and not by whichever Win32 call happens to follow.

Both calls are `Container::load_focused_window`, which shows the focused stored window, hides the
rest, leaves floating windows alone and never restores a minimized one - so a split which creates a
hidden container, or which leaves a hidden donor behind, still shows nothing, which is correct.

Verified by a regression test rather than by inspection.
`a_manual_split_shows_the_window_each_container_is_left_showing` asserts against `HIDDEN_HWNDS`,
the set komorebi hides windows into, so it observes the reveal itself rather than a model field. It
fails on the code as it was - the window left behind stays in the hidden set - and passes with the
load in place. The serial komorebi suite passes at 540, fmt is clean, and Clippy reports only the
pre-existing `items_after_test_module` warning.

### Phase 19 - Getting the Phase 18 fix onto the desktop

Added on 2026-08-31. The report was that creating a container still leaves the created half empty,
described exactly as in Phase 18. No Rust changed: Phase 18's fix was already in the tree and
already tested, but it had never been built into the binaries the machine runs.

The evidence is the version stamp. The repository was clean on `eebcab88`, which is Phase 18, while
the installed `komorebic.exe` reported `commit_hash:2d49746d` built at `2026-08-31 07:18:58` - the
Phase 16 build, two commits behind, made before the fix existed. The running `komorebi.exe`
therefore did not contain the two `load_focused_window` calls, so the symptom was the unfixed
behaviour, not a second defect. This is the failure mode Phase 15 already recorded once from the
other direction: a correct model does nothing for a desktop which is running a different binary.

- [x] 19A: rebuild the six release binaries, confirm the embedded commit is `eebcab88`, install
  through the existing request-file path in `komorebi-start.ps1`, cold start, and verify the fix
  against the live desktop rather than against the tests which already covered it.

No repository file changed but this plan.

Verification of the deployment itself: `cargo fmt --check` clean; the serial komorebi suite 540
passing, 0 failed; Clippy reporting only the pre-existing `items after a test module` warning; the
release `komorebic --version` reporting `eebcab88`; and the SHA-256 of the installed
`komorebi.exe` equal to the built one after the swap, which is what says the running process is the
fixed one rather than a rollback.

Verification of the fix on the live desktop, chosen so it returns the desktop to the state it
started in. The probe reads `DWMWA_CLOAKED` per window, because this configuration hides with
`Cloak`, so it observes what Windows is drawing rather than what the model believes. From one
container holding three windows with the top one on screen and two cloaked: the first
`create-container` left the created half showing its window *and* the donor half showing the window
it was left with - which is the case that was broken, since the donor's next window had been cloaked
underneath the one taken away. The second `create-container` split the donor again and all three
containers drew their window, with three gap-free non-overlapping slots exactly covering the work
area. Two `destroy-container` calls returned the desktop to a single container filling the work
area with the same three windows; the stack order came back as the round-robin distribution
produces it rather than in the original order, which is the documented behaviour and not a defect.
The runtime log holds one ERROR for the whole day, ten seconds before the new process started, so
it belongs to the binary being replaced.

### Phase 20 - Container count as an operation of its own

Added on 2026-08-31 after the model went into service. The request separates *how many containers
a workspace has* from *where windows go*, and makes both halves of the count - adding and removing -
explicit operations with their own selection rules. Four points were undecided in the request and
were settled by the user before any code was written:

- The automatic threshold stays, but becomes configurable. `N <= 2` splitting remains the default
  because it is the behaviour in service; a configured `0` gives the "never split automatically"
  model, where the count is decided by hand alone.
- Destroying a container which holds the focused window keeps that window focused: it is raised to
  the top of whichever recipient it is dealt to, while every other window arrives at the bottom as
  before. Focus is a window, not a position.
- The window a manual split takes is the first window in the workspace's window focus history which
  is not the top of its own container, starting from the second most recent. When the history does
  not cover the workspace - straight after a restart, or after an import - the stack order is the
  fallback: the first window below the top of the container holding the most windows.
- `create-container` and `destroy-container` keep their names and change meaning, both gaining an
  optional count. Destroying by age and destroying by focus are two separate keys, so the old
  "destroy the focused container" stays reachable as `destroy-focused-container`.

Corrected on 2026-08-31, mid-phase, before the destroy path was written: `destroy-container`
destroys the *newest* container, not the oldest. Adding and removing containers by hand are then
each other's inverse - a create followed by a destroy leaves the workspace where it started - which
is what the pair of keys is for.

The rules this phase implements, over and above what Phases 7 and 8 established:

- A workspace may never hold more containers than windows, and may never hold zero containers while
  it holds a window.
- A manual split divides the *largest* active logical slot, the most recently created container
  winning a tie - not the container the window came from, which may be a different one.
- A manual destroy destroys the *newest* container, by creation order rather than by focus. The
  focused container is destroyed by its own separate command.
- Neither operation changes which window is focused.

- [x] 20A: the two model facts these rules need and the workspace does not yet record - a
  workspace-level window focus history, and a creation order for containers. Both serialized, both
  defaulted, both maintained by every path which already maintains the histories beside them.
- [x] 20B: `create_container_from_donor` rewritten around the new donor and window selection, with
  the geometry donor and the window donor allowed to be different containers, and focus untouched.
- [x] 20C: destroy the newest container, keep destroying the focused one as a command of its own,
  raise the focused window in whichever recipient it is dealt to, and add the container-count
  invariants to `validate_invariants`.
- [x] 20D: socket and `komorebic` wiring: `--count` on both commands, `destroy-focused-container`,
  and outcomes which distinguish "no window can be moved" from "nothing to destroy".
- [x] 20E: `auto_split_threshold` configuration field with a serde default of 2, applied to the
  new-window placement path, plus the regenerated schema.
- [x] 20F: AutoHotkey bindings, `docs/window-model.md`, this plan, and live verification on the
  desktop, including that startup adoption still opens with one container in workspace one.
- [x] 20G: write the workspace window history from the focus event which actually happens. Found
  by the live verification, not by the tests.

Planned files: `komorebi/src/container.rs`, `komorebi/src/workspace.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/process_command.rs`, `komorebi/src/core/mod.rs`,
`komorebi/src/static_config.rs`, `komorebi/src/lib.rs`, `komorebic/src/main.rs`, `schema.json`,
`komorebi.ahk`, `docs/window-model.md`, this plan.

20A actual files: `komorebi/src/container.rs`, `komorebi/src/workspace.rs`, `komorebi/src/state.rs`.
Commit `c5ad24ba`.

The container stamp is a process-wide atomic rather than a workspace-level list, because a container
keeps its identity across workspaces and monitors and its age has to travel with it. It is
serialized, and a document which carries one lifts the counter above it, so a restart cannot hand a
new container an age older than everything already on the desktop; a document written before the
stamp existed carries zero, which is never issued, and is stamped afresh on the way in. The window
history is recorded by `record_focused_window`, the one call which already records the other two, and
is preserved explicitly wherever a container leaves a workspace its windows are staying in -
`destroy_container` and `consolidate_containers` - because `forget_container` drops the entries of
windows which leave with it.

Verification: komorebi 540 -> 549 passing, full serial suite green; `cargo fmt --check` clean;
Clippy clean apart from the pre-existing `items after a test module` warning.

20B actual files: `komorebi/src/workspace.rs`, `komorebi/src/container.rs`,
`komorebi/src/window_manager.rs`. Commit `4f247ad0`.

`donor_container_idx` became two selectors which answer two different questions, and
`Container::donor_window_idx` was deleted with the rule it existed for. The tie-break is
`max_by_key` on `(area, sequence)`, which is the newest of the largest, and the fallback when no
slot has been recorded yet is the geometry-start container, because a workspace which has not been
arranged is about to be rearranged and the choice only decides an insertion position.

Six tests changed rather than being deleted: they were the old rule written down. One consequence
worth recording is that the container which gives a window up can no longer be hidden by the split,
because the window it keeps showing is by definition a visible stored window - the test which
asserted the opposite now asserts that.

Verification: komorebi 549 -> 548 passing (nine added, ten of the old rule's removed or folded
in), full serial suite green; fmt clean; Clippy unchanged.

20C actual files: `komorebi/src/workspace.rs`, `komorebi/src/invariants.rs`. Commit `e7f785b4`.

`destroy_container` captures the focused container and window before it writes anything and puts
the focus back afterwards, by identity rather than by index. The travelling case is the only one
which moves anything: `raise_window` + `focus_window_by_hwnd` + `load_focused_window` on the
recipient, which is the one place a recipient's shown window is allowed to change. Natural
destruction - the last window of a container closing - still goes through `remove_window` and keeps
the Phase 8 post-removal focus, because that is a window disappearing rather than a count being
changed.

The count invariant is implied by "no empty container" plus "one owner per window" and is checked
directly anyway; it turned two existing invariant tests into two-violation tests, which is correct
and is recorded in them.

Verification: komorebi 548 -> 553 passing, full serial suite green; fmt clean; Clippy unchanged.

20D actual files: `komorebi/src/core/mod.rs`, `komorebi/src/process_command.rs`,
`komorebi/src/window_manager.rs`, `komorebic/src/main.rs`. Commit `356208db`.

`DestroyContainer` keeps its wire form and changes meaning; `DestroyFocusedContainer` is new and is
the old meaning. `--count` is sent as N messages rather than as a payload: each is validated and
committed on its own, `send_command` already exits on the first refusal with that outcome's code,
and no existing message shape changes.

Verification: komorebi 553 -> 555 passing, full serial suite green; fmt clean; Clippy unchanged.

20E actual files: `komorebi/src/lib.rs`, `komorebi/src/static_config.rs`,
`komorebi/src/workspace.rs`. Commit `1b4b1998`.

`AUTO_SPLIT_THRESHOLD` is a global read on every new window, so a reload changes where the next one
goes - unlike `window_adoption_behaviour`, which is read once. An empty workspace is still a special
case above the threshold: there is nothing to join.

Verification: komorebi 555 -> 558 passing, full serial suite green; fmt clean; Clippy unchanged.

20F actual files: `docs/common-workflows/komorebi-model.ahk`,
`docs/common-workflows/autohotkey-window-model.md`, `docs/window-model.md`, `docs/cli/`
(`create-container`, `destroy-container`, and the new `destroy-focused-container`), `mkdocs.yml`,
`schema.json`, this plan. Commit `242ba9a5`. Deployment: the four installed binaries.

`komorebic docgen` was not committed as it ran: run from a build directory it writes `Usage: <name>`
rather than `Usage: komorebic.exe <name>` and drops the page title, which would have rewritten all
199 pages to say nothing. The three pages this phase changes were regenerated from the release
binary's own `--help` and trimmed to the committed style, and the three pages docgen wanted to add
for older commands were dropped as belonging to no phase.

20G actual files: `komorebi/src/workspace.rs`. Commit `adffe0ac`.

Found by the live verification and by nothing else. `record_focused_window` is called from exactly
two places in the running program - merging a workspace and folding an adoption - so on a live
desktop the new workspace window history held one entry and never grew, and every manual split fell
through to its stack fallback. The tests could not see it: they call `record_focused_window`
directly, which is precisely the path the desktop does not use. `focus_container_by_window` is where
a focus change on the desktop reaches the model, and every focus komorebi performs itself comes back
through the same event, so one record there covers a click, the directional focus commands and the
stack raise alike.

Live verification, on commit `adffe0ac`, chosen so the desktop ends where it began. Cold start with
`--clean-state`: one monitor, four workspaces, workspace one holding one active container with all
four desktop windows, the foreground window on top and shown, one slot covering the work area,
three workspaces with no containers.

Two `raise-next-stack-window` calls grew the workspace window history from one entry to two and
reordered it, which is what says the history is now written by the event which happens.
`create-container` then moved the *second* entry - not the second window of the stack, which is the
fallback's answer and a different window - into a new container on the left, both halves 1908 wide
and touching at 1920, with the focus and both shown windows unchanged. A `DWMWA_CLOAKED` probe
confirmed both halves draw a window and the two stack members underneath stay cloaked.

Earlier on `242ba9a5`, before the history fix: a second `create-container` divided the newer of two
equally sized slots top to bottom, old above and new below, taking its window from the *other*
container's stack - the two roles falling to two containers, which is the rule. Four windows in four
containers tiled the work area 2x2 gap-free, and the fifth `create-container` answered exit 13 with
"every window in this workspace is the one its container is showing". `destroy-container --count 2`
and `--count 3` unwound the arrangement to one container each time.

`destroy-focused-container` was verified twice, once from the CLI and once through `Alt+Shift+D`:
the focused container went, the survivor took the whole work area, and the focused window was raised
to the top of the stack it landed in and is what that container shows. `Alt+C` and `Alt+D` were
verified through the hotkey layer the same way, with a `SendLevel 1` probe, and left the workspace
where it started.

The runtime log holds no ERROR or WARN from either new process. The two entries around each
deployment fall seconds *before* the new process starts, so they belong to the binary being
replaced.

Verification: komorebi 558 -> 559 passing, full serial suite green; `cargo fmt --check` clean;
Clippy clean apart from the pre-existing `items after a test module` warning; the installed
`komorebic --version` reporting `adffe0ac`; live desktop as above.

### Phase 21 - Three reports from the desktop

Added on 2026-08-31 from live use. Three independent reports, one per sub-phase; none of them share
a cause.

- The absorption priority order. Deleting the bottom right of four containers handed its area to the
  container beside it, because [`ABSORPTION_DIRECTIONS`] is left, right, up, down - the order in
  section 14 of the task. The user's expectation is vertical first, and confirmed it before any code
  was written: the order becomes up, down, left, right. The order a *new window* uses to pick a
  neighbour to join is section 9's separate rule and stays left, right, up, down, so the one
  constant which served both becomes two.
- The bar's layout icon is a fixed glyph per layout kind, so it says the same thing whether the
  workspace holds one container or six. It should draw the arrangement the workspace actually holds.
- `Alt+Shift+M` restored the container's slot but left the window minimized on the taskbar.

- [x] 21A: `ABSORPTION_DIRECTIONS` becomes up, down, left, right; `NEIGHBOUR_DIRECTIONS` keeps the
  old order for new-window joining; tests and `docs/window-model.md` follow.
- [x] 21B: restoring a minimized window takes it out of the minimized state Win32 holds it in.
- [x] 21C: the bar's layout widget draws the focused workspace's active logical slots.
- [x] 21D: build, deploy and verify all three on the live desktop.

Planned files: `komorebi/src/geometry.rs`, `komorebi/src/window.rs`,
`komorebi/src/window_manager.rs`, `komorebi-bar/src/widgets/komorebi_layout.rs`,
`komorebi-bar/src/widgets/komorebi.rs`, `docs/window-model.md`, this plan.

21D actual files: this plan. Deployment: the four installed binaries.

Built, installed through the request-file path in `komorebi-start.ps1` and cold started, with the
installed `komorebic --version` reporting `fbc05192`. Phase 19's lesson is the reason this is a
sub-phase of its own rather than a line at the end of the others.

The desktop opened as it always does - one monitor, four workspaces, workspace one holding one
active container with all four windows and one slot covering the work area - and every check below
was chosen so it ends there too, which it did.

21A on the desktop: three `create-container` calls quartered the work area into four gap-free slots,
1908 wide and 1018 high each. `destroy-container` then destroyed the newest, the bottom right, and
the *top right* container grew down from 1018 to 2036 while the bottom left was untouched. That is
the reported case, answered the way the user asked for. `destroy-container --count 2` unwound it.

21B on the desktop, reproducing the report exactly: two containers, focus left, `minimize`. The
window went genuinely iconic, its container went Hidden - two containers, one slot - and the survivor
took the whole work area. `restore-last-minimized-window` then brought both halves back to the same
rectangles they held before, *and* `IsIconic` came back false with the window in the foreground and
drawn at 29,69-1903,2071, inside its slot with the gaps applied. Before this phase the slots came
back and that last part did not.

21C on the desktop, by screenshot rather than by state, since the point is what is drawn: the icon
showed one filled cell with one container, two halves with the left one filled with two, and a 2x2
grid with four - the focused container's cell filled in each. The hotkey layer was not involved in
any of this and did not need to be: `Alt+Shift+M` has been bound to
`restore-last-minimized-window` since Phase 13, and the live script includes this repository's
`komorebi-model.ahk` directly.

The runtime log holds no ERROR or WARN from the new process. Today's twenty-one entries all fall at
or before the second the old process was stopped, so they belong to the binary being replaced.

21C actual files: `komorebi-client/src/lib.rs`, `komorebi-bar/src/widgets/komorebi_layout.rs`,
`komorebi-bar/src/widgets/komorebi.rs`.

The icon was a fixed glyph per layout kind, so it drew the same two cells whether the workspace held
one container or six, and nothing about it could change when a container was created or destroyed.
It now draws the workspace's own active logical slots: one cell per active container, in the
proportions the workspace actually holds, with the focused container's cell filled. Hidden containers
hold no slot and so are not drawn, which is the rule the tiling itself follows.

Only the interior edges are painted, because the icon's border is already the work area's outline: a
cell contributes its left edge unless that edge is the frame's own, and its top edge on the same
rule, so every dividing line is drawn exactly once whatever the arrangement - no per-layout drawing
code, and nothing to add when a new layout appears.

Monocle, the floating layer and a pause keep their glyphs. Those are modes rather than arrangements,
and a monocle in particular is showing one container over an arrangement which still exists
underneath it; drawing the slots there would say the workspace holds one container when it does not.

The slots reach the bar because they are already in the state document the bar subscribes to, keyed
by container ID. They are normalized against `logical_work_area` into the unit square on the way in,
with the slots' own bounding box standing in for a workspace which has not been arranged yet.
`komorebi-client` gained the four exports this needs - `LogicalRect`, `LogicalSlots`, `SlotOrder`,
`ContainerId` and `WorkspaceId` - which is the first time the geometry model has been part of the
client's surface.

Verification: four bar tests added, komorebi-bar 3 -> 7 passing; komorebi 561 passing, full serial
suite green; fmt clean; Clippy clean apart from the pre-existing warning. The parallel runner's
failures remain the pre-existing isolation defect recorded in Phase 18 and vary run to run - nine on
the unchanged tree, five with this change - which is what says they are scheduling rather than
regression.

21B actual files: `komorebi/src/window.rs`, `komorebi/src/managed_window.rs`,
`komorebi/src/window_manager.rs`.

`Window::restore` is not the opposite of a minimize and never has been: under the default `Cloak`
hiding behaviour it clears the cloak komorebi applied and nothing else, and cloaking and minimizing
are independent states. So `Alt+Shift+M` did every model thing correctly - the window became
`Visible`, its container became Active again and took its slot back - and then told Windows to
uncloak a window which was not cloaked. The window stayed on the taskbar. Only `Cloak` is affected;
the two other hiding behaviours restore the window as part of their own reveal.

`Window::unminimize` is the missing half, and it is deliberately a separate call rather than a change
to `restore`: un-minimizing a window the user minimized is the one thing komorebi must not do on its
own, so only a caller which knows the window is meant to be on screen may ask for it. Two callers
qualify. `restore_last_minimized_window` is the command itself, and it un-minimizes before the retile
because a window Windows is not drawing cannot be positioned. `ManagedWindow::show` is the general
reveal, guarded on the recorded visibility being `Visible` - a window the model believes minimized
never reaches that path at all - which covers a workspace switch or a reconciliation arriving at the
same disagreement. A maximized window needed neither, because `SW_MAXIMIZE` restores on its way.

No unit test: every step of this is a Win32 call on a real window handle, and the model half was
already correct and already covered. Verified on the desktop instead, recorded in 21D.

21A actual files: `komorebi/src/geometry.rs`, `docs/window-model.md`, this plan.

One existing test changed, which is the old order written down, and two were added: the fall-through
to a side edge when nothing is above or below, and the reported arrangement itself - four containers
quartering a landscape work area, deleting the bottom right, the top right growing down. The
new-window neighbour test gained a comment saying it is deliberately the other order, because the two
constants otherwise read as a copy of each other.

Verification: komorebi 559 -> 561 passing, full serial suite green; `cargo fmt --check` clean;
Clippy clean apart from the pre-existing `items after a test module` warning.

### Phase 22 - The window a container is left showing

Added on 2026-08-31 from live use. One report with two faces, and the two faces have two causes.

Minimizing the focused window of a container holding three windows either left the container drawing
nothing - the desktop showed through its slot - or left another window of the container on screen but
made `Alt+Shift+M` refuse to bring back the window which had just gone to the taskbar. Floating the
window a container was showing produced the first face as well. What the user asks for is one rule:
a container which stops showing a window shows the next window it owns, and only becomes hidden when
it has no window left to show.

- [x] 22A: the reveal is part of the transition. `Container::load_focused_window` draws the stored
  window the container *should* be showing rather than whichever window holds the ring focus;
  `Workspace::minimize_window`, `Workspace::float_window` and `Container::raise_next_stack_window`
  perform the reveal they each imply.
- [x] 22B: a minimize event is read as the user's unless komorebi is hiding by minimizing.
- [x] 22C: the window a container is left showing after a minimize is focused.
- [x] 22D: the window the user has just floated keeps the foreground, and unfloating shows the
  window it returns to the arrangement.
- [x] 22E: build, deploy and verify all of it on the live desktop.

Planned files: `komorebi/src/container.rs`, `komorebi/src/workspace.rs`,
`komorebi/src/process_event.rs`, `komorebi/src/window_manager.rs`, `docs/window-model.md`,
this plan.

The first cause is that komorebi only draws one stored window of a container at a time and hides the
rest, so the window a container is left showing has to be *shown* by whatever stopped the previous
one from being drawn. A retile does not do it: `Workspace::update` positions the windows a container
already has on screen and never reveals one. Three transitions ended without their reveal.
Minimizing moved the ring focus to the successor and left it cloaked. Floating left the ring focus on
the floating window, and `load_focused_window` showed the focused window and hid every other stored
window - so with the focus on a window it no longer draws, the container hid all of them. Raising the
window under the top of a stack reordered and focused it without showing it, which is the one whose
consequences reach furthest: the raised window stayed in `HIDDEN_HWNDS`.

So `load_focused_window` now asks which stored window the container should be drawing rather than
which window holds the ring focus. That question already had an answer -
`focused_visible_stored_managed_window`, which `Container::restore` and `Workspace::visible_windows`
use - and it is now the only answer: the ring focus while it can be shown, then the container's own
focus history, then the top of the stack, which is the order `first_focusable_window` selects with.
The three transitions call the reveal next to the state change, not in the retile, because it is a
property of the transition.

The second cause is the failing `Alt+Shift+M`, and it is one condition. The minimize event handler
asked whether the window was in `HIDDEN_HWNDS` and treated membership as proof komorebi had caused
the minimize. komorebi only minimizes a window itself under `HidingBehaviour::Minimize`; under the
default `Cloak` the windows in that set are cloaked, not iconic, so a real minimize of a window a
stack was covering was discarded. Nothing was recorded: the model went on believing the window was
visible, its container kept its slot, and `take_last_minimized_window` had nothing to return. The two
faces of the report are the same event seen through the two possible states of the hidden set, which
is why the report reads as intermittent. Membership is now consulted only under the behaviour which
can produce a self-inflicted minimize, in `minimize_event_is_the_users` with its own tests.

The runtime log is what separated the two causes. In the failing case the minimize event carried no
`focus_window` line, and the restore two seconds later answered `no-op: this workspace has no
minimized window left to restore`; in the working case the same key on the same window logged the
focus move and restored. That is the model not being told, rather than the model being wrong.

22A actual files: `komorebi/src/container.rs`, `komorebi/src/workspace.rs`.
22B actual files: `komorebi/src/process_event.rs`.

Five tests added for 22A and two for 22B. Three of the five were confirmed to fail on the unfixed
code before being kept - the minimize, float and raise reveals - and the other two cover the
derivation either side of them: minimizing or floating the last window a container can show leaves
the container hidden and holding its window rather than destroyed. The reveal tests assert on
`HIDDEN_HWNDS`, which is the set that says whether a window is actually being drawn, so they observe
the reveal itself rather than the model's opinion of it.

22C and 22D are what the first deployment found, and both are consequences of the reveal rather
than defects it uncovered. Windows hands the foreground to whatever is next in its own z-order when
a window is minimized, and on this desktop that was the bar: the container drew the right window and
no keystroke reached it. So a minimize of the window the workspace was working in focuses what its
container is left showing, asked before the transition because minimizing moves the container's
focus off the window, and skipped entirely for a background window, a window on another monitor, or
one whose container has nothing left to show.

22D is the same thing seen from the other side. Uncloaking a window makes the shell activate it -
the runtime log shows the `ObjectUncloaked` for the revealed window followed immediately by a
`SystemForeground` for it - and that arrives after the float command has returned, so the window the
user had just floated lost the foreground to the window which appeared behind it. The fix is
ordering rather than timing: a `Raise` event queued on the event channel is processed after the
events already in flight, and komorebi already uses that channel for exactly this. Unfloating gained
the reveal it implies at the same time, which is the same rule read backwards.

22C and 22D actual files: `komorebi/src/window_manager.rs`, `komorebi/src/workspace.rs`. Two more
tests: the workspace-level minimize through the window manager, and the unfloat reveal.

Verification: komorebi 561 -> 570 passing, full serial suite green; `cargo fmt --check` clean; Clippy
clean apart from the pre-existing `items after a test module` warning; installed `komorebic
--version` reporting `d0610e60`.

Live verification, on a workspace holding one container with four windows - the reported shape - and
chosen so the desktop ends where it began, which it did:

- Before: three windows cloaked, one drawn and focused, which is what a stack looks like.
- `minimize` on the focused window: the window is genuinely iconic, the model records it `Minimized`
  with the minimize history holding it, the container keeps its slot, and the next window is
  uncloaked *and* focused. Before this phase the window was drawn but the bar held the foreground,
  and before 22B nothing was recorded at all.
- `restore-last-minimized-window`: the same window comes back on top of its stack, not iconic, in the
  foreground, and the history is empty. This is the `Alt+Shift+M` which was answering `no-op`.
- `toggle-float`: the floated window keeps the foreground with its own centred rectangle, and the
  next stored window is uncloaked in the container's slot instead of the desktop showing through.
  `toggle-float` again returns the desktop to exactly the arrangement it started from.
- With two containers: minimizing the only window of one leaves two containers and one slot, and the
  survivor takes the whole 3816-wide work area; restoring brings both 1908-wide halves back exactly.

The one ERROR in the runtime log across the whole session belongs to a PowerShell console window of
the deployment shell arriving during the previous instance's shutdown, and is not from this work.

### Phase 23 - The floating window the mouse cannot move, and the keys the user asked for

Added on 2026-09-01 from live use. Four reports, and the audit which opened the turn found that the
first three share one cause and the fourth is a keymap request:

1. A floated window (`Alt+T`) cannot be moved with the mouse. Releasing the drag makes it jump
   somewhere else, and after a few drags it is gone from the screen entirely and `Alt+Tab` will not
   bring it back.
2. The floating move and floating resize keys do nothing.
3. `Win+方向键` collides with the Windows snap shortcuts.
4. The keymap: `Alt+方向键` should move the focused window into the container in that direction,
   `Alt+Shift+方向键` should merge the focused container with the container in that direction,
   `Alt+X` / `Alt+Z` add and remove a container, `Alt+[` / `Alt+]` walk the focused container's
   window focus history, `Alt+Shift+[` / `Alt+Shift+]` walk the workspace's container focus history,
   `Alt+W` closes, `Alt+F` toggles floating, `Alt+I` opens the panel and `Alt+/` goes away.

The evidence for the first three came from the runtime rather than from the source. The log has
`resizing with mouse` for every single drag of a floating window, and a live `GetWindowRect` probe
found the two floating windows of the focused workspace at `y = 2271` and `y = 2187` on a 2160-tall
screen - entirely below it - while the model still recorded the centred rectangle it gave them when
they were floated. That is one chain:

- `process_event` handles `MoveResizeEnd` for any managed window with the tiling path. It measures
  the drag against `latest_layout[focused_container_idx]`, which for a floating window is another
  container's rectangle or nothing at all, so the delta is meaningless and `is_move` is false.
- The meaningless delta becomes `resize_window` calls, and `toggle_float` has set
  `workspace.layer = Floating`, so `resize_window` takes its floating arm - which positions the
  focused floating window directly by that delta. `(Up, Decrease)` is `rect.top += delta`, which is
  how a window walks off the bottom of the screen a drag at a time.
- Nothing on that path ever writes `floating_rect`, so the model's record and the window disagree
  from the first drag onwards.

The second report is the same window's second symptom rather than a separate defect: a floating
window which has been blown up to the size of the work area by that arm is pinned by
`clamp_position` in every direction, and `move-floating-window` correctly answers `no-op`. The
`Win+方向键` bindings themselves reach komorebi; the log shows the commands arriving.

- [x] 23A: a drag of a floating window is a change to that window's own rectangle. `MoveResizeEnd`
  records the observed rectangle, pulled back into the work area only when the drag left the window
  out of reach, and never enters the container move/swap/resize path; `resize_window` loses its
  floating arm, because a container resize and a floating resize are two commands in this model and
  only one of them may move a floating window.
- [x] 23B: `toggle-float` leaves a window with a rectangle it can be moved from - a window Win32
  still has maximized is restored before it is centred - and a container resize refuses a hidden
  container, which owns no slot and so shares no boundary.
- [x] 23C: `merge-container <direction>`: the windows of the neighbouring container move into the
  focused container, the neighbour is destroyed and its slot is absorbed by the ordinary deletion
  path.
- [x] 23D: history cycling which does not rewrite the history it walks:
  `cycle-window-history previous|next` over the focused container's window MRU, raising the
  selected window to the top of the stack, and `cycle-container-history previous|next` over the
  workspace's container MRU. A cursor walks the history; any focus which does not come from these
  commands resets it.
- [x] 23E: the AutoHotkey keymap, the panel and the documentation.
- [x] 23F: `toggle-float` acts on one window, decided once. Found on the live desktop after 23A-23E
  were deployed: the model's focused window and the desktop's foreground window disagree after a
  float, so every floating command answered `NotFloating` and a second `toggle-float` floated
  instead of unfloating.
- [x] 23G: build, deploy and verify on the live desktop.

Planned files: `komorebi/src/process_event.rs`, `komorebi/src/window_manager.rs`,
`komorebi/src/workspace.rs`, `komorebi/src/container.rs`, `komorebi/src/core/mod.rs`,
`komorebi/src/process_command.rs`, `komorebic/src/main.rs`,
`docs/common-workflows/komorebi-model.ahk`, `docs/common-workflows/autohotkey-window-model.md`,
`docs/window-model.md`, this plan.

On `Alt+[` / `Alt+]` not changing the history they walk: they must not, and 23D implements that.
A history whose head is rewritten on every step can only ever reach the two most recent windows,
which is the same reason Windows' own `Alt+Tab` holds its order until the key is released. The stack
order does change - the selected window is raised to the top of its container, which is what the
request asks for - but the MRU is left alone until a focus arrives from somewhere else.

The keymap this phase settles, with one rule behind it: `Alt` is the model, `Shift` reverses or
strengthens, `Ctrl` means the floating window, and the arrow keys are never used with `Win`.

| Key | Action |
| --- | --- |
| `Alt+H J K L` | focus the container to the left / below / above / right |
| `Alt+Shift+H J K L` | swap the focused container with that neighbour |
| `Alt+←↓↑→` | move the focused window into the container in that direction |
| `Alt+Shift+←↓↑→` | merge the focused container with the container in that direction |
| `Alt+Ctrl+H J K L` | move the floating window |
| `Alt+Ctrl+Shift+H J K L` | grow the floating window's left / bottom / top / right edge |
| `Alt+Ctrl+Shift+Y U I O` | shrink the same four edges (the row above `H J K L`) |
| `Alt+Ctrl+←↓↑→` | push the focused container's boundary outwards |
| `Alt+Ctrl+Shift+←↓↑→` | pull the same boundary inwards |
| `Alt+X` / `Alt+Shift+X` / `Alt+Ctrl+X` | create a container: automatic / left-right / top-bottom |
| `Alt+Z` / `Alt+Shift+Z` | destroy the newest / the focused container |
| `Alt+[` `Alt+]` | walk the focused container's window history |
| `Alt+Shift+[` `Alt+Shift+]` | walk the workspace's container history |
| `Alt+Ctrl+[` `Alt+Ctrl+]` | move the workspace one position left / right |
| `Alt+W` | close the window |
| `Alt+F` / `Alt+Shift+F` / `Alt+Ctrl+F` | floating / maximized / fullscreen |
| `Alt+M` / `Alt+Shift+M` | minimize / restore the last minimized window |
| `Alt+A` / `Alt+Shift+A` | new workspace / delete and merge this workspace |
| `Alt+I` | the shortcut panel; `Alt+/` is removed |

### Phase 24 - The focus a floating window does not get, the border it leaves behind, and the second monitor

Added on 2026-09-01 from live use and from a review the user asked for before taking this build to
a two-monitor desk. Three reports; the audit which opened the turn found four causes, and all four
are the same kind of defect: upstream code which reads a floating window the way upstream stored
one, in a fork where a floating window is an ordinary member of a container.

1. The floating move and resize keys answer `NotFloating` while the user is looking at the floating
   window they just clicked. `process_event`'s `FocusChange` handler has two arms, and the floating
   arm sets `workspace.layer = Floating` and nothing else - it never focuses the container which
   owns the window. Upstream had nothing to focus, because a floating window was in a
   workspace-level list. Here the model's focused container stays on whatever tiled container was
   focused before, and `floating_geometry_subject` - which is deliberately the focused window and
   only the focused window - correctly reports that this window is not floating. It is the wrong
   window.
2. A frame is left on the desktop after a floating window is dragged. `border_manager` gives each
   container a border tracking `Container::focused_window`, which in this fork can be the
   container's floating window; `handle_floating_borders` then creates a second border for that
   same window and overwrites its entry in `WINDOWS_BORDERS`. Only the entry in that map receives
   `EVENT_OBJECT_LOCATIONCHANGE`, so the floating border follows the drag and the container border
   is stranded at the rectangle the window was dragged away from, with nothing inside it.
3. Two monitors, first defect: `transfer_window` decides between "a container window" and "a
   floating window" by asking whether the window has a container - and in this fork every managed
   window has one, so the floating branch is unreachable and dragging a floating window to the
   other monitor moves its whole container, tiled windows and all.
4. Two monitors, second defect: arriving on a monitor by crossing its boundary focuses the
   container at `cross_boundary_edge_index`, an index derived from the layout kind and the
   container count. This fork's geometry authority is the ID-keyed logical slot map, which after a
   split, an absorption or a manual resize does not agree with a fresh `layout.calculate`, and the
   count passed in includes hidden containers while the arrangement was computed for the active
   subset only. The index therefore names an arbitrary container. It never ran on the user's single
   monitor and runs on every crossing on two.

- [ ] 24A: a focus change onto a floating window is a focus change. The floating arm of
  `FocusChange` focuses the container which owns the window, so both MRU histories record it and
  the floating commands act on the window the user is looking at.
- [ ] 24B: a container's border tracks the window the container draws in its slot
  (`focused_visible_stored_window`), and a hidden container - which owns no slot - has no border at
  all. A floating window keeps exactly one border, its own, which follows it.
- [ ] 24C: a dragged floating window travels alone across a monitor boundary, and the rectangle it
  was dropped at is recorded on the workspace which owns it rather than on whichever workspace
  happens to be focused.
- [ ] 24D: crossing a monitor boundary lands on the container whose slot is flush with the edge
  being entered, chosen from the logical slots.
- [ ] 24E: build, deploy and verify on the live desktop; documentation and this plan.

Planned files: `komorebi/src/process_event.rs`, `komorebi/src/border_manager/mod.rs`,
`komorebi/src/window_manager.rs`, `komorebi/src/workspace.rs`, `docs/window-model.md`, this plan.

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
- 2026-08-30: The same debug-build stack overflow affects every `komorebic` invocation, `--help`
  included, because clap builds the whole command tree first. A release build runs fine.
- 2026-08-30: `komorebic docgen` must not be run to add a page. Its current output disagrees with
  the 177 checked-in `docs/cli/` pages in three ways (no `#` heading, `Usage: <cmd>` instead of
  `Usage: komorebic.exe <cmd>`, no trailing newline), so regenerating rewrites every page. The new
  page was written by hand in the checked-in format instead, and reconciling the generator with the
  checked-in docs is deferred to Phase 14.

- 2026-08-30: The logical slots are the geometry authority rather than a cache of the layout, so
  they are reconciled and only recalculated when they cannot be edited into agreement. This is what
  makes a local absorption survive the next update at all.
- 2026-08-30: Whether a recalculation is needed is decided by comparing the arrangement inputs
  (`SlotInputs`), not by a dirty flag set at each mutation site. A flag can be forgotten at one of
  twenty-odd sites and leave the arrangement stale; a fingerprint cannot. The focused container is
  excluded because only the scrolling layout uses it, and that layout invalidates explicitly.
- 2026-08-30: `OperationDirection` gained `PartialEq`/`Eq` in `komorebi-layouts`, the first change
  this task has made outside the `komorebi` crate. `Workspace` derives `PartialEq` and a hidden
  restore record names a direction, so this is required rather than convenient.
- 2026-08-30: Fullscreen is applied as the monitor rectangle through `SetWindowPos`, and maximize
  stays `ShowWindow`. Neither presentation is ever applied through the other's call, which is what
  makes the model's distinction survive a round trip through Win32 observation.
- 2026-08-30: `WorkspaceGlobals::monitor_size` carries `Monitor::size` rather than having the
  workspace query Win32 for the fullscreen rectangle. The fullscreen target is therefore available
  in tests, and it is the monitor bounds rather than the work area because a fullscreen window
  covers the taskbar.
- 2026-08-30: `Container::state()` is derived on every read instead of being stored. A stored
  flag would need updating at every place a window's placement or visibility changes, and one
  missed site would leave a container claiming a slot it must not have.
- 2026-08-30: Only stale-safe slot rules are model invariants in Phase 5A. `assert_invariants`
  runs wherever a command or event leaves the model at rest, and a background workspace is not
  retiled at that point, so coverage would report violations which are staleness rather than
  defects.
- 2026-08-30: `Workspace::floating_windows()` returns an owned listing derived from containers
  rather than a reference into storage. That is what makes two sources of truth impossible; the
  cost is an allocation on a path which is not hot.
- 2026-08-30: Floating a window no longer removes it from its container, so it no longer
  destroys an emptied container or shifts locked container indices, and unfloating no longer
  creates a container. This is an intended behaviour change from upstream komorebi.
- 2026-08-30: Old runtime state carrying a workspace-level `floating_windows` array is accepted
  and ignored rather than migrated. Static configuration does not contain the field, so
  configuration compatibility is untouched.

- 2026-08-30: Minimize keeps container ownership, so restoring a window from the taskbar is a
  reconciliation rather than a fresh `manage`. This is an intended behaviour change from upstream
  komorebi and is what makes the minimize history able to name windows the workspace still owns.
- 2026-08-30: A floating rectangle is the only rectangle in the model which no arrangement will
  ever correct, so it is the only one a monitor transfer has to rewrite. Slots are recalculated for
  the receiving work area and manual boundaries are discarded, and both of those are cheaper and
  safer than trying to carry them across a change of area and DPI.
- 2026-08-30: A transfer scales a floating window's size as well as its position. Keeping the pixel
  size would make a window occupy a visibly smaller share of a denser display, which reads as the
  window having shrunk rather than as the monitor having changed.
- 2026-08-30: `ContainerArrival` is a separate type from `NewWindowPlacement` rather than a reuse of
  it. They share three outcomes, but adoption has one they cannot share: a container can arrive with
  nothing for the arrangement to place, and then it takes no slot at all.
- 2026-08-30: Moving a container to another monitor moves the container, not the foreground window.
  Upstream took a single floating window across when the desktop reported one as focused; a floating
  window belongs to a container in this model, so it travels with it. This is an intended behaviour
  change from upstream komorebi.
- 2026-08-30: `WindowManager::remove_focused_workspace` was removed rather than guarded. It could
  leave a monitor with no workspaces, and every caller now goes through `release_workspace`, which
  refuses that. This is an intended API removal relative to upstream komorebi.
- 2026-08-30: A mutating command answers over the same connection a query does, and both sides
  treat a missing answer as "this build does not report one". That is what let outcomes be added
  without breaking a single existing caller, all of which write and never read.
- 2026-08-30: A presented window is reported as `NotFloating` rather than as its own outcome. The
  caller needs to know the window is not positioned by a floating rectangle; which presentation is
  responsible is detail.
- 2026-08-30: Suspension is applied directly by its command while resumption is dispatched as an
  event. A resumed window is processed as a newly opened one, and where it lands is decided by the
  new-window path rather than by the command.
- 2026-08-30: Refusals are outcomes, not errors. A position which does not exist, a workspace
  already where it was asked to go, a swap with itself and a monitor's last workspace all answer
  rather than fail, so a script can tell them apart from komorebi being unreachable.
- 2026-08-30: `commit_workspace_change` applies a compound operation to a clone, validates it and
  commits only if consistent. This is what makes the `InvariantViolation` outcome producible, and
  it is the structural form of the task's "validate before committing" rule.
- 2026-08-30: `Pause`/`Unpause` are in the paused allow-list. A resume command dropped by the state
  it exists to leave would be unreachable.
- 2026-08-30: The two workspace routing-rule passes are ordered readdress-then-remap. The departing
  workspace's rules must be sent on while the indices still describe the list they were written
  against; the reverse order slides a following workspace's rule onto the vacated index and then
  sends that one to the wrong monitor.

- 2026-08-30: `komorebic toggle-shortcuts` is not the shortcut panel for this workflow. It launches
  `komorebi-shortcuts`, which parses a `whkdrc`; with no whkd configuration the panel is empty, so
  the AutoHotkey script carries its own.
- 2026-08-30: Environment hazard, recorded so it is not repeated. A komorebi built from an earlier
  phase of this work is running on this machine, so *any* `komorebic` invocation from this
  repository is a live command to the user's desktop, not a dry run. Command-line validation must
  use `komorebic <name> --help` only, which parses and exits without sending anything.

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
- 2026-08-30: Phase 5 turn. The plan was re-read and the worktree confirmed clean at `12c58fa9`
  before editing, and `cargo check --workspace` was re-run as the turn's baseline. A census of the
  three alternate ownership paths returned 309 references, so Phase 5 was split into 5A derived
  state, 5B floating ownership, 5C minimize ownership and 5D presentation before coding. 5A made
  container state derived and made the arrangement cover only active containers. A pre-existing
  test race that 5A's timing exposed was fixed first, in its own commit. 5B removed the
  workspace-level floating window list, which is the single largest ownership change in the task:
  a floating window is now an ordinary member of a container which the container does not
  position. 5C made minimizing a visibility change rather than a removal, which is what makes a
  container able to become Hidden through the minimize path and what stops a restored window
  from being re-managed as a new window. Full serial workspace suite passed at every step
  (komorebi 190 -> 204 -> 213 -> 226 passing); fmt and Clippy clean apart from the pre-existing
  upstream warning. Next phase: 5D, replacing maximized and monocle ownership with window
  presentation, which is the last alternate ownership path in the model.
- 2026-08-30: Phase 5D-3 turn. The plan was re-read and the worktree confirmed clean at `b5a5b6ef`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  5D-3 gave the already-observed `Presentation::Fullscreen` its transitions, its Win32 application,
  its slot-authority decision and its command, which closes the presentation model: maximize,
  fullscreen, minimize and floating are now four independent dimensions with no shared Win32 call
  between any two of them. The phase's real weight was not the fullscreen path but the ten call
  sites which had been asking `maximized_window` when they meant "a window is covering the
  arrangement"; those now ask `presented_window`, and the sites which really mean maximize were left
  alone. Full serial workspace suite passed (komorebi 245 -> 257 passing); fmt and Clippy clean apart
  from the pre-existing upstream warning. Phase 5 is now complete: there is no alternate ownership
  path left in the model. Next phase: 6, hidden slot absorption and restoration.
- 2026-08-30: Phase 6 turn. Split into 6A geometry and 6B wiring before coding. 6A added the
  complete-edge group sweep and the absorption plan with its exact reverse, as values validated
  before anything is written. 6B made those the workspace's slot behaviour: `record_logical_slots`
  reconciles, `recalculate_logical_slots` is the fallback, and `SlotInputs` decides between them.
  The phase's real risk was not the algebra but knowing when the layout has to be consulted again;
  the fingerprint is what removes the possibility of a missed invalidation site. Full serial
  workspace suite passed at every step (komorebi 257 -> 268 -> 278 passing); fmt and Clippy clean
  apart from the pre-existing upstream warning. Next phase: 7, new-window threshold placement and
  manual container splitting.
- 2026-08-30: Phase 7 turn. The plan was re-read and the worktree confirmed clean at `67b8b1dd`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 7 was split into 7A primitives, 7B automatic allocation and 7C the operator-driven split
  before coding. 7A added the split plan and neighbour selection, and separated adjacency from the
  complete-edge requirement: absorption needs a complete edge because area changes hands, while
  choosing a container to receive a window does not. 7B made the active container count the
  allocation rule and, more importantly, made a split a third kind of local slot edit - applied at
  insertion and adopted, so the arrangement fingerprint does not read a new container as a reason to
  recalculate and discard it. 7C added the manual split, whose whole difficulty is atomicity and the
  two hidden outcomes, both of which fall out of the existing derivation rather than being handled
  separately. Full serial workspace suite passed at every step (komorebi 278 -> 290 -> 300 -> 312
  passing); fmt and Clippy clean apart from the two pre-existing warnings. One environment note:
  commits are signed through 1Password, which cannot be reached from the sandboxed shell, so commits
  are made from the unsandboxed one and may need an approval prompt answered. Next phase: 8,
  container deletion, distribution and multi-neighbour resize.
- 2026-08-30: Phase 8 turn. The plan was re-read and the worktree confirmed clean at `8b389c28`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 8 was split into 8A expansion, 8B distribution and 8C resize before coding: the three
  concerns are what happens to the *area* a departing container leaves, what happens to the
  *windows* it held, and a boundary move which deletes nothing at all. 8A found that every deletion
  had been ending in a full recalculation, discarding the user's boundaries, and made the departing
  container's absorption a planned local edit instead - the ordering constraint being that it can
  only be planned while the slot is still in the map and only adopted once the container has left
  the ring. 8B added destruction with distribution, whose recipient order turned out to be one rule
  rather than two: follow the area. 8C established that a resize moves a boundary rather than an
  edge, which means near-side containers move too. Full serial workspace suite passed at every step
  (komorebi 312 -> 320 -> 329 -> 349 passing); fmt and Clippy clean apart from the two pre-existing
  warnings. Three tests were rewritten after passing, twice because the test was wrong about correct
  behaviour and once because it could not fail; each is recorded in its phase. The 1Password signing
  agent is unreachable from the sandboxed shell and intermittently needs a second attempt from the
  unsandboxed one. Next phase: 9, floating window move and edge resize.
- 2026-08-29: Phase 3 was split into 3A identity and 3B histories/invariants after call-site review
  showed that doing both together would exceed the phase review-size limit. Phase 3A added
  transparent typed workspace/container IDs, migrated managed ownership and UI integration
  boundaries, preserved workspace IDs in state snapshots, and maintained legacy JSON compatibility.
  Compile, schema, focused serial tests, and the full serial workspace suite passed. Next phase:
  explicit focus/minimize histories and ownership/history invariant validation.
- 2026-08-30: Phase 9 turn. The plan was re-read and the worktree confirmed clean at `77f90f57`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 9 was split into 9A primitives, 9B validated operations and 9C commands before coding, and
  9C was brought forward from Phase 12 for these two commands only, because "independent of
  container movement" is a claim about the command surface which cannot be shown while the only way
  to reach the behaviour is the existing `MoveWindow`. Upstream's `move_floating_window_in_direction`
  was replaced rather than extended: it shared the container resize delta, asked the desktop who was
  focused instead of asking the model, recorded nothing in `floating_rect` and validated nothing.
  The phase's real content is in what a floating command is allowed to touch - one window's
  rectangle and nothing else - and the isolation tests pin that by comparing the slot map and the
  container identities across each command, including for a floating window inside a Hidden
  container, which moves without its container reclaiming a slot. Full serial workspace suite passed
  at every step (komorebi 349 -> 361 -> 372 -> 374 passing); fmt and Clippy clean apart from the
  pre-existing warning. One environment note: the 1Password signing agent was unreachable for
  several minutes mid-turn and refused three commit attempts before recovering; staging the
  finished phase in the index kept the two phases separable while it was down. Next phase: 10,
  workspace ordering, deletion, merge and minimized restore.
- 2026-08-30: Phase 10 turn. The plan was re-read and the worktree confirmed clean at `e1d47e78`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 10 was split into 10A ordering, 10B merge and 10C wiring before coding. The phase's quiet
  hazard was not the merge but the index-keyed tables: a workspace's identity is its `WorkspaceId`,
  yet the configured names, the focused and last-focused indices and the global application routing
  rules all describe workspaces by position, so both reordering and deleting return a permutation
  and every such table is moved with it. Deleting needs two answers rather than one, which is why
  `WorkspaceReorder` distinguishes what follows the workspace - its name, the fact that it was
  focused - from what follows its windows: a merged workspace's routing rules point at the
  workspace which absorbed them. 10C found a real atomicity defect through a test with unreal
  window handles: the rules were being remapped after the retile, so a Win32 focus failure returned
  early and left them pointing at a workspace which no longer existed; the model's own tables now
  settle together, and only the desktop work may fail afterwards. 10C also closed the one gap left
  in the minimized-restore path, which had never raised a restored window to the top of its stack.
  Full serial workspace suite passed at every step (komorebi 386 -> 400 -> 406 passing).
  Environment change worth recording: unlike every earlier phase, `cargo fmt` and `cargo clippy`
  both run in this environment now; fmt warns that `imports_granularity` is nightly-only and skips
  that option alone, and Clippy is clean apart from the pre-existing `items after a test module`
  warning. Three tests were corrected after failing, each recorded in its phase. Next phase: 11,
  cross-monitor container and workspace migration.
- 2026-08-30: Phase 11 turn. The plan was re-read and the worktree confirmed clean at `6cd89fd6`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 11 was split into 11A floating rectangles, 11B container adoption and 11C workspace
  migration and wiring before coding. The phase's hazard turned out to be exactly what the split
  was drawn around: a monitor transfer crosses a coordinate system, and of every rectangle the
  model stores only the floating one has to survive it, because it is the only one nothing else
  will correct. 11C found two defects rather than one. `move_container_to_monitor` was moving a
  single foreground window instead of a container whenever the desktop reported a floating window
  as focused, which the ownership invariant forbids, and the routing-rule passes had an ordering
  constraint which is invisible until a workspace between two ruled ones moves away: readdressing
  must precede remapping or a following workspace's rule is sent to the wrong monitor.
  `remove_focused_workspace` was deleted rather than guarded, since it could leave a monitor with
  no workspaces and its only remaining caller was its own test. Full serial workspace suite passed
  at every step (komorebi 406 -> 416 -> 428 -> 441 passing); fmt and Clippy clean apart from the
  pre-existing warning. The 1Password signing agent was unreachable for several minutes again and
  refused four commit attempts; staging the finished 11B in the index kept the two phases separable
  while it was down, as in Phase 9. Next phase: 12, socket protocol and komorebic CLI.
- 2026-08-30: Phase 12 turn. The plan was re-read and the worktree confirmed clean at `40dc3f9a`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  Phase 12 was split into 12A outcomes, 12B window lifecycle, 12C container lifecycle, 12D
  workspace ordering and 12E stable-ID transfers before coding. The audit which opened the turn is
  the finding worth recording: the model had been ahead of the command surface since Phase 5, and
  suspension, minimize restore, manual splitting, container destruction, workspace reordering and
  merging all existed with no socket message at all. The transport needed nothing new - the reply
  writer for queries was already there - and compatibility came from neither side insisting: a
  missing answer means "this build does not report one". 12C produced the turn's most reusable
  piece in `commit_workspace_change`, which is also what made `InvariantViolation` a producible
  outcome rather than a variant nothing could reach. Two tests were corrected after failing, both
  wrong about which window a test container focuses; each is recorded in its phase. Full serial
  workspace suite passed at every step (komorebi 441 -> 450 -> 457 -> 462 -> 470 -> 477 -> 478
  passing); fmt and Clippy clean apart from the pre-existing warning. The 1Password signing agent
  was unreachable for several minutes again and refused three attempts at the 12A commit; staging
  the finished phase in the index kept the phases separable while it was down, as in Phases 9 and
  11. Next phase: 13, the AutoHotkey v2 workflow.
- 2026-08-30: Phase 13 turn. The plan was re-read and the worktree confirmed clean at `b7db727b`
  before editing, and `cargo check --workspace --all-targets` was re-run as the turn's baseline.
  The turn opened with a shortcut audit rather than with the script, and it found two commands the
  task's shortcut list needs which no socket message could reach: raising the next window in a
  stack (task section 7) and sending a single window to another monitor (task section 12). Both
  were built first, as Phases 12F and 12G, because binding a hotkey to a command which does not
  exist is not an option. 12F also fixed the removal-focus defect underneath the first of them:
  `remove_window_by_idx` focused `idx - 1` whatever was there, which could both move focus off an
  untouched window and land on a minimized one. 12G found that the ID-addressed transfer had been
  able to cross monitors since 12E while carrying its floating rectangle across unchanged, and
  applied Phase 11A's rule to it. Phase 13 itself is documentation: an AutoHotkey v2 script whose
  every command line was checked against `komorebic --help` and whose syntax was validated by the
  installed AutoHotkey v2, plus a page explaining the outcome exit codes and the three window
  classes as a user meets them. Full serial workspace suite passed at every step (komorebi 478 ->
  487 -> 491 passing); fmt and Clippy clean apart from the pre-existing warning. One incident worth
  recording: an early attempt to validate command lines ran them instead of asking for their help
  text, and a komorebi from an earlier phase of this work is running on this machine, so a handful
  of real commands reached the user's desktop. The rule this produced is in the decision log. Next
  phase: 14, event reconciliation, serialization, documentation and final verification.
- 2026-08-30: Phase 14 turn, the last one. The plan was re-read and the worktree confirmed clean at
  `2e30dfb4` before editing, and the full serial workspace suite was re-run as the turn's baseline
  at komorebi 491 passing. The turn opened with an audit of the event pipeline, the state output
  and the test inventory rather than with code, and it split Phase 14 into five sub-phases along
  what that audit found. 14A: a suspended window is removed from the reaper's cache, so nothing
  ever noticed when one went away, and its handle stayed in the suspension set for the lifetime of
  the process - Windows reuses handles, and the next window given that number was suppressed
  forever with no command able to recover it; entries now carry the process which owned the window
  and are given up when the handle stops naming it, and the reaper forgets everything the destroy
  path clears. 14B: the model has owned a window's presentation since Phase 5D and the retile
  reapplies it, but nothing read it back, so a user who restored a maximized window by hand was
  fighting komorebi; only the maximized state bit is believed, and only in the direction of leaving
  a recorded presentation, because a fullscreen rectangle is one komorebi writes itself and an
  observation which disagrees with it cannot be told from one which has not landed yet. 14C: the
  state output was complete but for the one field a consumer cannot compute - a container's derived
  Active or Hidden state - and carried no way to tell which model wrote it; the derived state is
  now published without being stored, and a document from another version is refused rather than
  read with serde's defaults filling in a model which never held. 14D is the phase which paid for
  itself: a seeded random-operation harness found, on its second run, that a split adopted the
  current slots as the arrangement even when the workspace already owed a recalculation, so a
  layout change followed by a container deletion and a new window could leave a workspace tiling
  half its work area permanently. 14E is the model reference, the invariant map with every citation
  verified to exist, and the schema, which had been stale since Phase 9A. Two harness assumptions
  turned out to be wrong rather than the model, and both are recorded in the harness itself. Full
  serial workspace suite passed at every step (komorebi 491 -> 503 -> 517 -> 522 -> 527 passing);
  fmt and Clippy clean apart from the pre-existing warning. The one check which does not run in
  this environment is debug-build schema generation, which overflows its stack; the release binary
  generates every schema, including the notification schema which exercises the hand-written
  container serialization. The plan is complete.
- 2026-08-31: Phase 15 turn, added after the plan was complete. No Rust changed. The reported
  symptom was that the newly built komorebi managed no windows and most shortcuts did nothing; the
  turn opened by reading the runtime rather than the source, and komorebi turned out to be healthy
  in every respect - the failure was entirely in the user's configuration and script layer, which
  still belonged to a zone fork this repository has never contained. Four defects, each sufficient
  on its own: workspaces declaring only `zone_state` left `tile` false so nothing placed a window;
  `Append` bypassed threshold placement; the PowerShell layer read `.hwnd` off what are now
  `ManagedWindow` values and silently computed empty targets, which is why nothing was ever logged;
  and six zone socket messages and subcommands were being sent which have never existed. The most
  interesting fix was not one of those four but the one underneath them: `Get-Zone` indexed
  `latest_layout` by container position, and `latest_layout` now holds active containers only, so a
  single hidden container would have handed every later container a neighbour's rectangle - a
  defect which would have survived all four visible fixes and shown up later as direction commands
  moving the wrong window. It now reads the ID-keyed `logical_slots`. Verified by checking all 67
  emittable socket message names against `socket-schema` and every invoked subcommand against
  `--help` (full coverage, no misses), by exercising `Get-Zone` against live state (six gap-free
  non-overlapping slots exactly covering the work area), and by confirming the runtime log has been
  free of `unknown variant` and direction errors since the restart. Both AutoHotkey scripts pass
  `/validate`. One incident is recorded in the phase: a `/validate` sent through the Bash tool was
  rewritten into a path and left a stray AutoHotkey process holding a dialog on the user's desktop;
  it was identified from its command line and killed, and the check re-run through PowerShell.
- 2026-08-31: Phase 16 turn. The plan was re-read and the worktree inspected before editing, and
  the turn opened by reading the runtime rather than the source: komorebi was healthy on commit
  `57f1ee0d`, the log had carried no `unknown variant` and no direction error since the Phase 15
  restart, and the five windows on the desktop were in containers and tiled. What Phase 15 fixed
  has held. The Phase 15 plan entry itself turned out to be sitting in the index uncommitted, and
  was committed first so the two turns stayed separable. The request was about startup, and the
  audit which opened the phase found half of it already true: `load_workspace_information`
  enumerates into the monitor's *first* workspace and prunes what belongs elsewhere, so adoption
  has always reached workspace one and nothing else, and the cold-start script passes
  `--clean-state` so no state document overrides it. Two things were not true - the enumeration
  gave every window its own container, and the workspace count came only from `komorebi.json`, so a
  monitor the configuration does not name arrived with one. 16A built the fold out of the removal
  and receiving paths which already exist, which is what makes it keep placement, visibility,
  presentation, floating rectangles and both histories; the two things it had to add are recorded
  in the phase, and the more interesting of them is that a container position is not a stable
  reference across a removal, because the ring re-anchors locked containers. 16C put the workspace
  count in `load_monitor_information` rather than in `monitor::new`, because that is the one place
  a display becomes a monitor komorebi holds - startup and hot-plug both - while `monitor::new` is
  the constructor a dozen tests build values with. Full serial suite passed at every step (komorebi
  527 -> 534 -> 537 -> 539 passing); fmt and Clippy clean apart from the pre-existing warning. The
  binaries were built, installed through the existing deployment request, and cold started, and the
  live desktop came up in exactly the requested shape; a bounded, self-restoring command smoke test
  is recorded in 16D. One thing found but not changed: a `SetCloak` panic in `com/mod.rs`, upstream
  code whose `CoCreateInstance(CLSID_ImmersiveShell).unwrap()` aborted the process once on
  2026-08-30 with `0x80040154`. It is not a regression from this work and hardening it would edit
  an upstream file for a once-in-five-days event, so it is reported rather than fixed.
- 2026-08-31: Phase 17 turn. No Rust changed. `Alt+I` opened no panel because nothing is bound to
  it: the panel moved to `Alt+/` when Phase 15 replaced the user's script with this repository's,
  and the user's key is the one their previous script used. komorebi itself was healthy throughout
  - answering `state`, unpaused, holding all five desktop windows - so the fix is entirely in the
  script and the configuration. `Alt+I` is now bound alongside `Alt+/`, the tray icon carries the
  same entry for when an elevated foreground window swallows a hotkey, and the panel's
  `ignore_rules` entry was widened from an `Equals` on the old panel's title to a `StartsWith` on
  `komorebi 快捷键`, which covers both panels - without it komorebi gives the panel a slot and
  shifts the tiling every time it opens. Two self-contained alternatives were tried and dropped on
  evidence: `+ToolWindow` does not clear `WS_EX_WINDOWEDGE`, and stripping that bit before `Show()`
  does not survive it. Verified end to end rather than by reading: the elevated hotkey layer was
  reloaded through the watchdog by removing its pid file, and a `SendLevel 1` probe opened and
  closed the panel with both keys from a clean start while `komorebic state` showed it unmanaged.
- 2026-08-31: Phase 18 turn. The plan was re-read and the worktree inspected first; it was clean on
  `d41a29ff`. The report was a manual split whose created half draws nothing, and the diagnosis
  came from the two screenshots rather than from a log: the empty half moves along by one every
  press, which is the signature of a reveal happening one operation late. `create_container_from_donor`
  was the only removal path in the workspace which did not load the donor's focused window
  afterwards, and the container which was drawing nothing was the donor, not the new container -
  the new one was being shown by its focus call, which is why it looked like the *new* container
  was empty. Fixed in the model, next to the removal, so the CLI, the socket and the harness all
  get it. The regression test asserts on `HIDDEN_HWNDS` so it observes the reveal itself; it was
  confirmed to fail on the unfixed code before being kept. Serial suite 540 passing, fmt clean,
  Clippy unchanged. Two window_manager tests fail under the default parallel runner both before and
  after this change - they assert on the same global hidden set that other tests mutate - which is
  a pre-existing test-isolation defect, not a regression, and is recorded here rather than fixed in
  a bug-fix turn.
- 2026-08-31: Phase 19 turn. No Rust changed. The reported symptom was Phase 18's symptom, and the
  cause was that Phase 18 had never been deployed: the tree was clean on `eebcab88` while the
  installed binaries reported `2d49746d`, so the running `komorebi.exe` predated the fix by two
  commits. The turn opened by comparing the embedded commit stamp with `git log` rather than by
  re-reading the split code, which is what made it a five-minute diagnosis instead of a second
  investigation of an already-correct model. Rebuilt, checked (fmt clean, serial suite 540 passing,
  Clippy unchanged), installed through the request-file path `komorebi-start.ps1` already owns -
  which keeps a rollback copy and elevates through the registered scheduled task rather than a UAC
  prompt - and cold started. The fix was then verified on the live desktop with a `DWMWA_CLOAKED`
  probe rather than from `komorebic state`, because the model was never the thing in doubt: what
  needed observing was whether Windows draws the window the donor is left showing. It does. The
  smoke test was bounded so the desktop ends where it began. The lesson worth keeping is the one
  Phase 15 taught in the configuration layer and this turn repeats in the binary layer: a fix which
  is committed and tested is not a fix the user has, and a bug report that matches the previous
  turn's report exactly should be checked against the deployed version stamp first.
- 2026-08-31: Phase 21 turn. Three reports from live use, and the audit which opened the turn found
  three unrelated causes, which is why they became three sub-phases rather than one. The absorption
  order was not a defect at all - `ABSORPTION_DIRECTIONS` was left, right, up, down, exactly as
  section 14 of the task specifies - so the question was put to the user before any code was
  written, and the order became up, down, left, right on their answer. The interesting part was
  what *not* to change with it: one constant was serving both the absorption order and section 9's
  separate rule for picking a neighbour to hand a new window to, and those are two questions with
  two separately specified answers, so it became two constants and only one of them moved. The
  minimize report is the turn's real bug and its cause is one line deep: `Window::restore` is not
  the opposite of a minimize - under the default `Cloak` hiding behaviour it clears the cloak
  komorebi applied and nothing else - so every model step of `Alt+Shift+M` was correct and the last
  Win32 call uncloaked a window which had never been cloaked. `unminimize` is deliberately separate
  from `restore` rather than folded into it, because un-minimizing a window the user minimized is
  the one thing komorebi must not do on its own; only the command itself and a reveal whose recorded
  visibility is already `Visible` may ask for it. The bar icon was never wrong, only static: it
  drew a glyph per layout kind, and this fork has had ID-keyed logical slots in its state document
  since Phase 4, so the arrangement was already there to read. It is drawn by painting interior
  edges only, which is one routine for every arrangement rather than one per layout. Serial suite
  komorebi 559 -> 561 passing, komorebi-bar 3 -> 7; fmt clean; Clippy clean apart from the
  pre-existing warning. The parallel runner's failures are the Phase 18 isolation defect and vary
  run to run - nine on the unchanged tree, five with this change - which is what says they are
  scheduling. All three were verified on the live desktop after deployment, on `fbc05192`, and the
  desktop ends where it began.
- 2026-09-01: Phase 23 turn. Four reports, and the audit which opened the turn read the runtime
  before it read the source: the log had `resizing with mouse` for every drag of a floating window,
  and a live `GetWindowRect` probe found two of the user's floating windows entirely below a
  2160-tall screen while the model still held the centred rectangle it had given them. That is one
  chain and it explains all three floating symptoms. `MoveResizeEnd` handled every managed window
  with the tiling path, measuring the drag against `latest_layout[focused_container_idx]` - which
  for a floating window is another container's rectangle or nothing - so `is_move` came out false
  and the meaningless delta became `resize_window` calls; `toggle_float` sets
  `workspace.layer = Floating`, and `resize_window`'s floating arm then applied that delta straight
  to the window, where `(Up, Decrease)` is `rect.top += delta`. That is how a window walks off the
  bottom of the screen a drag at a time. Both halves are gone: the drag records the rectangle it was
  dropped at, and a container resize no longer has any way to reach a floating window. The clamp for
  a dropped rectangle is deliberately the laxer of the two - a window dropped half off an edge is a
  placement, and moving it back would be the jump the fix is about - while the movement commands
  keep the containment rule the task specifies.

  The keymap is the fourth report and it settled a rule rather than a list: `Alt` is the model,
  `Shift` reverses or takes the whole container, `Ctrl` is the floating window, and the arrow keys
  never meet `Win`. Two commands had to exist for it. `merge-container <direction>` is built on the
  removal path which already exists - the only thing a merge changes is that one container is named
  as the recipient instead of the arrangement choosing several - so the stack order, both histories,
  the slot release and the focus are the operations already tested. `cycle-window-history` and
  `cycle-container-history` walk a most-recently-used list without rewriting it, which is the whole
  design: a walk which recorded each entry it visited would return to its starting point on the
  second step. Each level holds a cursor, the focus the walk asks for is the one focus that level
  does not record, and any other focus ends the walk.

  The turn's most interesting defect was found after 23A-23E were built, tested and deployed, by
  running the commands on the live desktop rather than by reading the code again: `toggle-float`
  decided its branch from `WindowsApi::foreground_window()` and then acted on the model's focused
  window. Those disagree exactly when komorebi reveals a window underneath the one being floated -
  the uncloak records the revealed window - so the floating commands' subject was the wrong window
  and answered `NotFloating`, and pressing `Alt+F` twice floated rather than unfloated. The command
  now resolves one window once. Its companion fix is in the `Raise` handler: Win32 reports a focus
  change only when the foreground window actually changes, so raising the window which already has
  it produces no event and leaves the model behind; the raise records the focus itself.

  komorebi 570 -> 588 passing on the serial runner, fmt clean, Clippy clean apart from the
  pre-existing `items after a test module`. Verified on the live desktop after deployment: the
  history walk stepped three times with the container history unchanged and the stack reordered
  each time; `create-container` then `merge-container left` left one container holding four windows
  across the whole 3816-wide work area; `toggle-float` gave a window 1400x1050 centred and
  `move-floating-window` / `resize-floating-window` moved and resized it by the DPI-scaled delta;
  and a synthesized title-bar drag of a floating window moved it by exactly the drag, updated
  `floating_rect` to match, left the tiled slot untouched and produced no `resizing with mouse`.
  The desktop ends where it began. One thing this turn could not do: the commits were blocked
  part-way by the 1Password SSH agent, whose vault locked - which is itself a symptom of the bug,
  since the 1Password window was one of the two this defect had pushed off the bottom of the
  screen. It was moved back on screen to unblock signing.
