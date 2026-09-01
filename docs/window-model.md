# The managed window model

This page is the reference for how this build of komorebi decides what a window is, which container
owns it, what area that container holds and what happens when any of those go away. The shortcut
half of the same model is in
[AutoHotkey for the window model](common-workflows/autohotkey-window-model.md).

## Three kinds of window, and only three

The difference between them is *where the window is*, not how it is drawn.

| Kind | In the model? | Owned by a container? | In the histories? | Positioned by komorebi? |
| --- | --- | --- | --- | --- |
| Ignored | Never | No | No | No |
| Temporarily unmanaged | Not while suspended | No | No | No |
| Managed, floating | Yes | Yes | Yes | No |
| Managed, stored | Yes | Yes | Yes | Yes |

**Ignored** windows are decided by configuration - `ignore_identifiers` and komorebi's own permanent
exclusions. They never enter the managed set, never join a container, never appear in a focus or
minimize history, and no ordinary command reaches them. `komorebic` answers a command aimed at one
with the `Ignored` outcome rather than pretending to act.

**Temporarily unmanaged** windows were managed and were explicitly suspended with
`komorebic suspend-window`. Suspending removes the window from its container, from the stack, from
both focus histories and from the minimize history, and destroys the container if it held nothing
else; the window is left exactly where Windows has it. Ordinary Win32 show, move and focus events
cannot take it back - that is the whole point - and `komorebic resume-window` hands it back as a
*new* window: the current monitor, the current workspace, the new-window threshold and the routing
rules decide where it goes, not where it used to be. The suspension set is runtime state and is not
persisted; a restarted komorebi manages every eligible window it finds.

A suspended handle is remembered with the process which owned the window. Windows reuses window
handles, and a handle which comes to name a different window is given up rather than suppressed, so
a reused handle cannot leave a new window permanently unmanageable.

**Floating** windows are fully managed. A floating window belongs to a container, keeps its place in
that container's stack, keeps both history entries, travels with the container across workspaces and
monitors, and takes part in distribution when a container is destroyed. The only thing that changes
is that komorebi stops positioning it: its rectangle is its own, stored as `floating_rect`, and
moved or resized by the floating commands alone.

## What a window's state is

Placement, visibility and presentation are independent, and no transition of one implies another.

- `placement`: `Stored` (positioned by its container) or `Floating` (positions itself).
- `visibility`: `Visible` or `Minimized`.
- `presentation`: `Normal`, `Maximized` or `Fullscreen`. Maximized is a Win32 window state;
  fullscreen is the monitor rectangle. They are applied by different calls and are never mixed.

Minimizing does not float a window. Maximizing does not float a window. Neither maximizing,
minimizing nor going fullscreen changes which container owns a window. Leaving a presentation puts a
stored window back in its container's slot and a floating window back on its own rectangle.

komorebi follows a window *out* of a presentation it recorded: if the user restores a maximized
window by hand, the record follows rather than re-maximizing it at the next retile. It does not
follow a window *into* one, because a tiled window is placed by its container.

## Active and Hidden containers

A container's state is derived from its windows on every read and is never stored:

- **Active**: at least one window is both `Visible` and `Stored`. The container occupies exactly one
  logical slot and takes part in the tiling, direction focus, swaps and resizing.
- **Hidden**: the container still owns windows, but none of them is both visible and stored - they
  are all floating, all minimized, or a mix. The container is *not* destroyed. It keeps its ID, its
  windows, its stack order, its window focus history and its place in the workspace's container
  history. It simply holds no slot, so the active containers cover the area it had.

### What a container shows

A container draws one of its stored windows at a time and hides the rest, and floating windows are
drawn beside that one from their own rectangles. The window it draws is the ring focus while that
window is visible and stored, then the most recently focused window which is, then the top of the
stack. The ring focus is allowed to sit on a floating or a minimized window - the user is working in
it - and the container goes on showing a stored window underneath.

Revealing is part of the transition which changed what the container shows, not part of the retile:
a retile positions windows which are already on screen. So minimizing the shown window, floating it,
and raising the window under the top of the stack each show what the container is left showing.
A container which is left with no window it can show is exactly a container which has become Hidden,
and it draws nothing because its slot is being given up.

### Giving a slot up and getting it back

When the last visible stored window of an Active container floats or is minimized:

1. The slot it held is recorded, along with the direction it was absorbed from, the containers which
   absorbed it and the rectangles they held before they did.
2. A *complete edge group* is chosen in up, down, left, right order: a set of active containers
   which touch the whole of the freed edge, whose contact intervals do not overlap and leave no gap.
   The container above a freed slot therefore grows down into it before the container beside it
   grows across. This is not the order a new window uses to pick a neighbour to join, which is left,
   right, up, down: one decides which container's area grows, the other which container a window
   joins.
3. Those containers grow over the freed slot, each along one axis only.
4. If no direction can absorb it, the whole workspace is relaid out from the layout instead and the
   record is marked as not exactly restorable.

When a window in a Hidden container becomes visible and stored again, the record is *consulted, not
replayed*: the release is planned against the slots as they are now, and it is only applied if the
absorbers still exist, are still active and still hold exactly what the absorption gave them. If
anything moved - a layout change, a manual resize, a swap, a merge, a monitor or work-area change -
the workspace is relaid out instead. Every geometry change advances a generation counter, and
records which describe an arrangement that no longer exists are dropped.

## Logical slots and rendered rectangles

Three rectangles, and only the first is geometry:

- `logical_rect`: the gap-free slot a container holds. Splitting, adjacency, absorption, resizing
  and coverage all work on these, keyed by container ID rather than by index.
- `render_rect`: the logical slot after workspace padding, container padding and border offsets.
  This is what a stored window is actually given.
- `floating_rect`: a floating window's own rectangle, which no arrangement writes.

Gaps therefore cannot change adjacency: two containers are neighbours because their logical slots
touch, whatever the gap between the windows drawn in them. A 50:50 split of an odd number of pixels
gives the extra pixel to the older container.

## The desktop komorebi opens on

Every monitor is given four workspaces as komorebi learns about it - at startup and again when a
monitor is plugged in - so the count does not depend on that monitor being named in
`komorebi.json`. Configuration which asks for more still gets more; nothing ever shrinks the list.

The windows already open when komorebi starts are enumerated into the *first* workspace of the
monitor they are on, and the windows belonging to another monitor are given back to it, so initial
adoption reaches workspace one and no other: the remaining workspaces open empty, with no
containers. Those windows are then folded into a single container, the first one, in the desktop's
own front-to-back order, so the window which was in the foreground is on top of the stack. Every
window keeps its placement, visibility and presentation through the fold; only which container owns
it is decided here.

This is what `window_adoption_behaviour` selects. `SeparateContainers` keeps upstream's adoption,
where each window found is given a container of its own.

A komorebi which is restarting from a dumped state document applies that state instead: the
arrangement a crash interrupted is restored rather than replaced by a fresh adoption. Passing
`--clean-state` to `komorebic start` is what makes a start a cold one.

## How many containers a workspace has

The container count is a thing in its own right, changed in four ways: two automatic and two by
hand.

**A new window adds one automatically** while the workspace holds `auto_split_threshold` active
containers or fewer - two by default, so the first three windows each get a container. Past that a
new window joins a neighbour instead. A threshold of `0` gives the other model whole: a workspace
which already has an active container never grows another one on its own.

**A container destroys itself automatically** when its last window is closed or suspended.

**`komorebic create-container` adds one by hand.** It divides the *largest* active logical slot, the
most recently created container winning a tie, so repeated splits walk across the workspace instead
of cutting the same half over and over. The window put into the created container is chosen
separately, and may come from a different container entirely: it is the second most recent window in
the workspace's window focus history which is not the window its own container is currently showing.
Skipping the shown windows is what keeps every container drawing what it was drawing, and it is also
why the operation refuses when there are already as many containers as there are windows - the only
window left to move would be one somebody is looking at. When the history says nothing, which is the
state a restart or an adoption leaves behind, the largest stack answers instead: the window directly
below the one it is showing. `--count` repeats the operation.

**`komorebic destroy-container` removes one by hand**: the container created most recently, which
makes it exactly the inverse of `create-container`. `komorebic destroy-focused-container` removes
the one holding the focus instead. Both deal the windows out to the containers which remain, at the
bottom of each recipient's stack.

Neither half of the count moves the focus. A created container is shown but not focused; a destroyed
container which was not holding the focus leaves it where it was. The single exception is the
focused window in a container being destroyed: it is dealt out like the rest, then raised to the top
of whichever container received it and focused there, so the window being worked on stays the window
being worked on.

**`komorebic merge-container <direction>` removes one by joining two.** Every window of the
container in that direction moves into the focused container, underneath what it is already
showing, and the neighbour is destroyed - so its slot is released through exactly the same
expansion the ordinary deletion path uses, and the focused container, which borders it, is usually
what grows over it. Both containers have to be active: a hidden container owns no slot, so there is
no direction in which it lies. This is deliberately not `komorebic stack <direction>`, which moves
one window into a neighbouring container and leaves both containers standing.

Two rules hold throughout, and are checked by `validate_invariants`: a workspace never holds more
containers than windows, and never holds no container at all while it holds a window.

## Walking a focus history

Every container keeps its windows in most-recently-used order and every workspace keeps its
containers the same way. Those two histories answer which window to focus when the current one goes
away, and `komorebic cycle-window-history` and `komorebic cycle-container-history` let the user walk
them directly - the first raising the window it lands on to the top of its container, the second
moving the focus to the container it lands on.

Walking a history does not reorder it. A walk which recorded every entry it visited would put that
entry at the front, so the next step would come straight back to where the walk started and the
third entry would be unreachable; this is the same reason `Alt+Tab` holds its order until the key is
released. Each walk holds a cursor into the history it is reading, the focus it asks for is the one
focus that history does not record, and any other focus ends the walk and is recorded normally.
The cursor is runtime state and never reaches a state document: it describes a key somebody is
still pressing rather than anything about the arrangement.

The stack *does* change under `cycle-window-history`, because the window walked to is raised to the
top of its container - that is what makes the container draw it.

## Configuration

| Field | Meaning | Default |
| --- | --- | --- |
| `default_workspace_count` | Workspaces every monitor is given, named in the configuration or not | `4` |
| `auto_split_threshold` | Active containers a workspace may hold before a new window joins one instead of adding one; `0` never adds | `2` |
| `window_adoption_behaviour` | What happens to the windows already open when komorebi starts | `SingleContainer` |
| `floating_move_delta` | Step used by `komorebic move-floating-window` when no delta is passed | `50` |
| `floating_resize_delta` | Step used by `komorebic resize-floating-window` when no delta is passed | `50` |

The two deltas are steps in logical units, scaled to the DPI of the monitor the window is on. All
five are ordinary `komorebi.json` fields with serde defaults, so a configuration written for an
older komorebi still loads, and a configuration which does not mention one leaves it at its
default. The deltas are deliberately separate from `resize_delta`, which is the step for resizing a
*container*.

`window_adoption_behaviour` is read once, by the desktop enumeration during startup. Changing it
and reloading stores it for the next start rather than for the running one, because the desktop has
already been adopted by then.

## State documents

`komorebic state` and the dumped `komorebi.state.json` now carry a `version`. A state document is a
model rather than a configuration: it names containers, slots, histories and per-window placement
which only mean something to the model that wrote them, and serde would happily default fields which
did not exist when it was written into a model that never held. komorebi therefore applies a dumped
state only when its version is the current one, and logs why when it does not. A document written
before this model existed reads back as version `0` and is not applied; komorebi manages the windows
it finds instead.

The state output publishes, for every workspace: its stable ID, its layout, its container focus
history, its window focus history, its minimize history, its logical slots with their geometry generation, the work area those
slots were calculated against, and the hidden restore records with their `old_rect` and
`exact_restore_valid`. For every container: its stable ID, its derived `state`, its stack and its
window focus history. For every window: its owning container ID, placement, visibility, presentation
and floating rectangle.

## The invariants

These are checked by `validate_invariants` in `komorebi/src/invariants.rs`, which runs after every
command and event in a debug build and logs rather than panicking in a release build. The seeded
operation harness in `komorebi/src/model_harness.rs` re-checks them after every operation of long
randomized sequences.

| # | Invariant | Implemented in | Tested by |
| --- | --- | --- | --- |
| 1 | Every managed window belongs to exactly one container | `container.rs` (`ManagedWindow::container_id`), `invariants.rs` (`WindowOwnership`) | `invariants.rs::a_window_in_two_containers_is_reported`, `model_harness.rs` |
| 2 | Ignored and temporarily unmanaged windows belong to no container | `window.rs::should_manage`, `suspension.rs`, `process_event.rs::should_suppress_temporarily_unmanaged_event` | `process_event.rs::temporarily_unmanaged_event_tests`, `window_manager.rs::suspend_removes_window_ownership_and_empty_container` |
| 3 | Every container owns at least one managed window | `workspace.rs::remove_window`, `invariants.rs` (`NonEmptyContainer`) | `invariants.rs::an_empty_container_is_reported`, `model_harness.rs` |
| 4 | A workspace may have no containers | `workspace.rs::calculate_logical_slots` | `workspace.rs::hiding_every_container_leaves_no_active_slot`, `model_harness.rs::the_harness_reaches_the_states_it_is_meant_to_exercise` |
| 5 | Floating and minimized windows still count as container windows | `container.rs::windows`, `container.rs::state` | `container.rs::a_container_whose_windows_all_float_is_hidden`, `container.rs::a_container_whose_windows_are_all_minimized_is_hidden` |
| 6 | Every active container holds one active logical slot | `workspace.rs::record_logical_slots` | `workspace.rs::logical_slots_tile_the_available_area_exactly`, `model_harness.rs` |
| 7 | A hidden container holds no active logical slot | `workspace.rs::active_container_ids`, `workspace.rs::absorb_departed_slots` | `workspace.rs::a_hidden_container_occupies_no_logical_slot`, `model_harness.rs` |
| 8 | Active logical slots do not overlap | `geometry.rs::LogicalSlots::validate_coverage`, `invariants.rs` (`SlotOwnership`) | `workspace.rs::logical_slots_tile_the_available_area_exactly`, `model_harness.rs` |
| 9 | Active logical slots cover the whole work area | `geometry.rs::LogicalSlots::validate_coverage` | `workspace.rs::the_active_containers_expand_over_a_hidden_container_area`, `model_harness.rs` |
| 10 | Gaps are applied when rendering, never to logical geometry | `geometry.rs::RenderInsets`, `workspace.rs::render_rect_at` | `workspace.rs::the_container_gap_does_not_change_the_logical_slots`, `geometry.rs::gaps_do_not_change_logical_adjacency` |
| 11 | A workspace focuses at most one container | `ring.rs`, `invariants.rs` (`FocusSelection`) | `invariants.rs::a_container_focused_out_of_range_is_reported` |
| 12 | A container focuses at most one window | `ring.rs`, `invariants.rs` (`FocusSelection`) | `container.rs::focus_selection_prefers_recency_and_skips_minimized_windows`, `invariants.rs::a_consistent_workspace_reports_nothing` |
| 13 | Minimize, maximize, fullscreen and float do not change ownership | `managed_window.rs`, `workspace.rs::enter_presentation` | `workspace.rs::maximizing_keeps_the_window_in_its_container`, `workspace.rs::a_floating_window_keeps_the_container_which_owns_it` |
| 14 | Deleting an object clears every history and index which named it | `workspace.rs::prune_histories`, `focus_history.rs::Mru::remove`, `window_manager.rs::forget_window` | `invariants.rs::history_entries_for_removed_objects_are_reported`, `invariants.rs::removal_paths_leave_a_workspace_consistent`, `window_manager.rs::forgetting_a_window_clears_every_runtime_table_the_destroy_path_clears`, `model_harness.rs` |
| 15 | A compound operation either succeeds completely or changes nothing | `window_manager.rs::commit_workspace_change`, `window_manager.rs::restore_workspace_snapshot` | `window_manager.rs::creating_a_container_without_an_eligible_donor_refuses`, `model_harness.rs` refusal property |
| 17 | A workspace never holds more containers than windows, nor none while it holds a window | `invariants.rs` (`ContainerCount`), `workspace.rs::split_window_hwnd` | `invariants.rs::more_containers_than_windows_is_reported`, `workspace.rs::a_manual_split_is_refused_when_every_window_is_the_one_its_container_shows` |
| 16 | Switching layout changes no container ID, ownership or stack order | `workspace.rs::recalculate_logical_slots` | `workspace.rs::a_layout_change_drops_the_restore_records_and_relays_out`, `workspace.rs::a_layout_change_discards_a_manual_resize` |

## Differences from upstream komorebi

- A workspace has no floating window list of its own. Floating is a property of a managed window,
  and the window stays in its container.
- Minimized windows stay in their containers instead of being removed from the workspace.
- Monocle is a reference to a container in the ring rather than a second place a container is
  stored, and maximize is a window presentation rather than a workspace-level window.
- Containers and workspaces have stable IDs, and the commands which address them by ID are additions
  rather than replacements: the index-based commands still work.
- The state document is versioned, and a document from an older komorebi is not applied.
- Every monitor opens with four workspaces rather than one, and the windows already open are
  adopted into a single container rather than one container each. Both are configurable, and
  `window_adoption_behaviour: "SeparateContainers"` restores upstream's adoption.
- `destroy-container` destroys the container created most recently rather than the focused one;
  upstream's behaviour is `destroy-focused-container`. Neither moves the focus, where upstream's
  moved it to the container which expanded.
- A manual split divides the largest slot and takes a window nobody is looking at, where upstream
  split the focused container and took the window it was showing.
- Dragging a floating window with the mouse records the rectangle it was dropped at. Upstream
  measured every drag of a managed window against the arrangement and turned it into a container
  swap or a container resize, which a floating window has no slot to be measured against.
- `resize-edge` only ever moves a boundary two active containers share. Upstream redirected it at
  the focused floating window whenever the workspace was on its floating layer; in this model a
  floating window is resized by `resize-floating-window` and by nothing else, and a hidden
  container - which owns no slot, so it shares no boundary - is refused rather than given a resize
  adjustment nothing would apply.
