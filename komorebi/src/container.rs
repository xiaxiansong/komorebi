use std::collections::VecDeque;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::Lockable;
use crate::core::Rect;
use crate::floating_geometry;
use crate::focus_history::Mru;
use crate::managed_window::ManagedPlacement;
use crate::managed_window::ManagedWindow;
use crate::managed_window::Presentation;
use crate::managed_window::Visibility;
use crate::model::ContainerId;
use crate::ring::Ring;
use crate::window::Window;

/// Whether a container currently takes part in the tiled arrangement.
///
/// This is never set by a user command and is never stored on the container. It is derived from
/// the container's own windows on every read, which is what makes it impossible for the recorded
/// state and the windows it describes to drift apart.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ContainerState {
    /// The container owns at least one visible, stored window, so it occupies one logical slot.
    #[default]
    Active,
    /// The container still owns windows, but none of them is both visible and stored, so it
    /// occupies no logical slot and other active containers may cover the area it had.
    Hidden,
}

impl Display for ContainerState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Hidden => write!(f, "Hidden"),
        }
    }
}

/// The source of container creation order.
///
/// A container's identity is a nanoid, which says nothing about when it was made, and its position
/// in the ring is an arrangement rather than a history. The operations which change a workspace's
/// container *count* are ordered by age - the oldest container is the one destroyed, the newest is
/// the one which wins a tie between equally large slots - so every container is stamped with a
/// number as it is made, and that stamp is the only thing which answers "which of these came
/// first".
static CONTAINER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Stamp the next container. Zero is never issued, so it can mean "unstamped" on the way in.
fn next_container_sequence() -> u64 {
    CONTAINER_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// Lift the stamp above a value which has just been read back from a state document.
///
/// A restored container keeps the stamp it was written with, so a restart cannot reorder a
/// workspace's containers; without this the counter would start again at one and hand a newly
/// created container an age older than everything already on the desktop.
fn observe_container_sequence(sequence: u64) {
    CONTAINER_SEQUENCE.fetch_max(sequence.saturating_add(1), Ordering::Relaxed);
}

/// Serialization and the schema go through [`ContainerRepr`] and [`ContainerView`] rather than a
/// derive, because the serialized shape carries one field this type deliberately does not store:
/// the container's derived [`ContainerState`].
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: ContainerId,
    /// When this container was created, relative to every other container in this process.
    ///
    /// Monotonic and never reused. It travels with the container across workspaces and monitors,
    /// because the container is the same container wherever it is shown.
    sequence: u64,
    pub locked: bool,
    windows: Ring<ManagedWindow>,
    /// Most-recently-used order of this container's window handles.
    ///
    /// The ring focus index is a position in the stack, so it cannot answer which window should
    /// be focused after the current one goes away. This history can, and it is the only source
    /// used for that decision.
    focus_history: Mru<isize>,
}

/// The serialized shape of a container.
///
/// This is what the schema describes and what a state document is read back through, and it holds
/// one field the container itself does not: its [`ContainerState`]. That state is derived from the
/// container's windows on every read, so it is written for consumers of the state output - a bar,
/// a script, `komorebic state` - and ignored on the way back in, where the windows decide it
/// again.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
struct ContainerRepr {
    id: ContainerId,
    /// Creation order. Absent or zero in a document written before this existed, which is stamped
    /// afresh on the way in rather than left to collide with every other unstamped container.
    #[serde(default)]
    sequence: u64,
    #[serde(default)]
    locked: bool,
    windows: Ring<ManagedWindow>,
    #[serde(default)]
    focus_history: Mru<isize>,
    /// Derived from the windows below it; never stored on the container.
    #[serde(default)]
    state: ContainerState,
}

/// The borrowed twin of [`ContainerRepr`], so serializing a container copies nothing.
#[derive(Serialize)]
struct ContainerView<'a> {
    id: &'a ContainerId,
    sequence: u64,
    locked: bool,
    windows: &'a Ring<ManagedWindow>,
    focus_history: &'a Mru<isize>,
    state: ContainerState,
}

impl Serialize for Container {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ContainerView {
            id: &self.id,
            sequence: self.sequence,
            locked: self.locked,
            windows: &self.windows,
            focus_history: &self.focus_history,
            state: self.state(),
        }
        .serialize(serializer)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Container {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Container".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        ContainerRepr::json_schema(generator)
    }
}

pub trait IntoManagedWindow {
    fn into_managed_window(self, container_id: &ContainerId) -> ManagedWindow;
}

impl IntoManagedWindow for Window {
    fn into_managed_window(self, container_id: &ContainerId) -> ManagedWindow {
        ManagedWindow::capture(self, container_id.clone())
    }
}

impl IntoManagedWindow for ManagedWindow {
    fn into_managed_window(mut self, container_id: &ContainerId) -> ManagedWindow {
        self.container_id.clone_from(container_id);
        self
    }
}

/// Mutable access to a container's window ring which keeps ownership correct on insertion.
pub struct ManagedWindowsMut<'a> {
    container_id: &'a ContainerId,
    windows: &'a mut VecDeque<ManagedWindow>,
}

impl ManagedWindowsMut<'_> {
    pub fn push_back(&mut self, window: impl IntoManagedWindow) {
        self.windows
            .push_back(window.into_managed_window(self.container_id));
    }
}

impl Deref for ManagedWindowsMut<'_> {
    type Target = VecDeque<ManagedWindow>;

    fn deref(&self) -> &Self::Target {
        self.windows
    }
}

impl DerefMut for ManagedWindowsMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.windows
    }
}

impl<'a> IntoIterator for ManagedWindowsMut<'a> {
    type Item = &'a mut ManagedWindow;
    type IntoIter = std::collections::vec_deque::IterMut<'a, ManagedWindow>;

    fn into_iter(self) -> Self::IntoIter {
        self.windows.iter_mut()
    }
}

impl<'de> Deserialize<'de> for Container {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let repr = ContainerRepr::deserialize(deserializer)?;

        let sequence = if repr.sequence == 0 {
            next_container_sequence()
        } else {
            observe_container_sequence(repr.sequence);
            repr.sequence
        };

        let mut container = Self {
            id: repr.id,
            sequence,
            locked: repr.locked,
            windows: repr.windows,
            focus_history: repr.focus_history,
        };

        // The container is the authority for ownership. This both migrates legacy serialized
        // windows (whose owner defaults to an empty ID) and repairs stale owner IDs.
        let container_id = container.id.clone();
        for window in container.windows_mut() {
            window.container_id.clone_from(&container_id);
        }

        container.repair_focus_history();

        Ok(container)
    }
}

impl Default for Container {
    fn default() -> Self {
        Self {
            id: ContainerId::new(),
            sequence: next_container_sequence(),
            locked: false,
            windows: Ring::default(),
            focus_history: Mru::default(),
        }
    }
}

impl Lockable for Container {
    fn locked(&self) -> bool {
        self.locked
    }

    fn set_locked(&mut self, locked: bool) -> &mut Self {
        self.locked = locked;
        self
    }
}

impl Container {
    /// When this container was created, relative to every other container in this process.
    ///
    /// Smaller is older. This is the ordering the count operations use: `destroy-container`
    /// destroys the smallest, and a tie between equally large slots is won by the largest.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn windows(&self) -> &VecDeque<ManagedWindow> {
        self.windows.elements()
    }

    pub fn windows_mut(&mut self) -> ManagedWindowsMut<'_> {
        ManagedWindowsMut {
            container_id: &self.id,
            windows: self.windows.elements_mut(),
        }
    }

    /// Compatibility accessor for callers that only need the Win32 handle wrapper.
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.focused().map(|window| &window.window)
    }

    pub fn focused_window_mut(&mut self) -> Option<&mut Window> {
        self.windows.focused_mut().map(|window| &mut window.window)
    }

    pub fn focused_managed_window(&self) -> Option<&ManagedWindow> {
        self.windows.focused()
    }

    pub fn focused_managed_window_mut(&mut self) -> Option<&mut ManagedWindow> {
        self.windows.focused_mut()
    }

    pub const fn focused_window_idx(&self) -> usize {
        self.windows.focused_idx()
    }

    pub fn preselect() -> Self {
        Self {
            id: ContainerId::from("PRESELECT"),
            sequence: next_container_sequence(),
            locked: false,
            windows: Default::default(),
            focus_history: Mru::default(),
        }
    }

    pub fn is_preselect(&self) -> bool {
        self.id.as_str() == "PRESELECT"
    }

    /// This container's derived [`ContainerState`].
    ///
    /// A preselect marker is reported as active because it is a reserved insertion slot in the
    /// arrangement rather than a container of the model; it holds no window by design and must
    /// not be treated as a container which lost its last visible stored window.
    #[must_use]
    pub fn state(&self) -> ContainerState {
        if self.is_preselect() || self.has_visible_stored_window() {
            ContainerState::Active
        } else {
            ContainerState::Hidden
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state() == ContainerState::Active
    }

    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.state() == ContainerState::Hidden
    }

    /// The windows which the owning container is allowed to position.
    ///
    /// A floating window keeps its container membership but is positioned from its own floating
    /// rectangle, and a minimized window is not positioned at all, so neither is included.
    pub fn visible_stored_windows(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.windows()
            .iter()
            .filter(|window| window.is_visible_stored())
    }

    #[must_use]
    pub fn has_visible_stored_window(&self) -> bool {
        self.visible_stored_windows().next().is_some()
    }

    /// The number of windows this container owns which are floating, minimized, or both.
    ///
    /// Floating and minimized windows still count towards the container's window total; this
    /// only reports how many of them the container does not position.
    #[must_use]
    pub fn unpositioned_window_count(&self) -> usize {
        self.windows()
            .iter()
            .filter(|window| {
                window.visibility == Visibility::Minimized
                    || window.placement == ManagedPlacement::Floating
            })
            .count()
    }

    pub fn hide(&self, omit: Option<isize>) {
        for window in self.windows().iter().rev() {
            let mut should_hide = omit.is_none();

            if !should_hide
                && let Some(omit) = omit
                && omit != window.hwnd
            {
                should_hide = true
            }

            if should_hide {
                window.hide();
            }
        }
    }

    /// Show what this container should be showing.
    ///
    /// Only one stored window of a stack is on screen at a time, but a floating window is not
    /// part of that stack for display purposes: it keeps its own rectangle and stays visible
    /// next to whichever stored window is on top. A minimized window is never restored here,
    /// because being minimized is a window state this container does not own. Every window is
    /// shown through its own presentation, so a maximized window comes back maximized.
    pub fn restore(&self) {
        if let Some(window) = self.focused_visible_stored_managed_window() {
            window.show(true);
        }

        for window in self.visible_floating_windows() {
            window.show(true);
        }
    }

    /// Hides the unfocused stored windows of the container and restores the focused one. This
    /// function is used to make sure we update the window that should be shown on a stack. If the
    /// container isn't a stack this function won't change anything.
    ///
    /// Floating windows are left alone unless they are minimized: their visibility does not
    /// depend on where they sit in the stack.
    pub fn load_focused_window(&mut self) {
        let focused_idx = self.focused_window_idx();

        for (i, window) in self.windows_mut().iter_mut().enumerate() {
            match (window.placement, window.visibility) {
                (_, Visibility::Minimized) => {}
                (ManagedPlacement::Floating, _) => window.show(false),
                (ManagedPlacement::Stored, _) => {
                    if i == focused_idx {
                        window.show(false);
                    } else {
                        window.hide_with_border(false);
                    }
                }
            }
        }
    }

    /// The stored window this container is currently showing, if it is showing one.
    ///
    /// The ring focus can be on a floating or a minimized window, which is exactly when this
    /// falls back to the first stored window the container could show instead.
    pub fn focused_visible_stored_window(&self) -> Option<&Window> {
        self.focused_visible_stored_managed_window()
            .map(|window| &window.window)
    }

    /// [`Self::focused_visible_stored_window`] with the managed state the caller needs to show it.
    pub fn focused_visible_stored_managed_window(&self) -> Option<&ManagedWindow> {
        self.focused_managed_window()
            .filter(|window| window.is_visible_stored())
            .or_else(|| self.visible_stored_windows().next())
    }

    /// The window this container currently presents maximized, if it has one.
    ///
    /// A maximized window is an ordinary member of its container: it keeps its stack position and
    /// its history entries, and only the presentation it is drawn with differs.
    pub fn maximized_managed_window(&self) -> Option<&ManagedWindow> {
        self.managed_window_presented_as(Presentation::Maximized)
    }

    /// The window this container currently presents fullscreen, if it has one.
    pub fn fullscreened_managed_window(&self) -> Option<&ManagedWindow> {
        self.managed_window_presented_as(Presentation::Fullscreen)
    }

    /// The window this container draws over the arrangement, in either presentation.
    pub fn presented_managed_window(&self) -> Option<&ManagedWindow> {
        self.windows()
            .iter()
            .find(|window| window.is_presented() && window.visibility == Visibility::Visible)
    }

    /// A minimized window is never a subject here: its presentation is remembered for the restore
    /// but nothing is drawing it, so it must not claim to be what the container is presenting.
    fn managed_window_presented_as(&self, presentation: Presentation) -> Option<&ManagedWindow> {
        self.windows().iter().find(|window| {
            window.presentation == presentation && window.visibility == Visibility::Visible
        })
    }

    pub fn visible_floating_windows(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.windows().iter().filter(|window| {
            window.placement == ManagedPlacement::Floating
                && window.visibility == Visibility::Visible
        })
    }

    pub fn floating_windows(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.windows()
            .iter()
            .filter(|window| window.placement == ManagedPlacement::Floating)
    }

    /// Mutable access to this container's floating windows.
    ///
    /// This goes past [`ManagedWindowsMut`] deliberately: that guard exists to stamp ownership on
    /// insertion, and iteration inserts nothing.
    pub fn floating_windows_mut(&mut self) -> impl Iterator<Item = &mut ManagedWindow> {
        self.windows
            .elements_mut()
            .iter_mut()
            .filter(|window| window.placement == ManagedPlacement::Floating)
    }

    /// Carry every floating rectangle this container holds from one work area to another.
    ///
    /// Only stored floating rectangles change. A window's placement, visibility, presentation and
    /// position in the stack are untouched, and a window which has never floated has no rectangle
    /// to carry: it will be given one by the work area it is in when it first floats.
    ///
    /// A stored window needs nothing done to it, because its rectangle comes from a slot which the
    /// receiving workspace is about to recalculate in its own coordinates.
    pub fn transfer_floating_rects(&mut self, from: Rect, to: Rect) {
        if from == to {
            return;
        }

        for window in self.floating_windows_mut() {
            if let Some(rect) = window.floating_rect {
                window.floating_rect =
                    Some(floating_geometry::transfer_between_areas(rect, from, to));
            }
        }
    }

    pub fn hwnd_from_exe(&self, exe: &str) -> Option<isize> {
        for window in self.windows() {
            if let Ok(window_exe) = window.exe()
                && exe == window_exe
            {
                return Option::from(window.hwnd);
            }
        }

        None
    }

    pub fn idx_from_exe(&self, exe: &str) -> Option<usize> {
        for (idx, window) in self.windows().iter().enumerate() {
            if let Ok(window_exe) = window.exe()
                && exe == window_exe
            {
                return Option::from(idx);
            }
        }

        None
    }

    pub fn contains_window(&self, hwnd: isize) -> bool {
        for window in self.windows() {
            if window.hwnd == hwnd {
                return true;
            }
        }

        false
    }

    pub fn idx_for_window(&self, hwnd: isize) -> Option<usize> {
        for (i, window) in self.windows().iter().enumerate() {
            if window.hwnd == hwnd {
                return Option::from(i);
            }
        }

        None
    }

    pub fn remove_window_by_idx(&mut self, idx: usize) -> Option<ManagedWindow> {
        let focused_idx = self.focused_window_idx();
        let window = self.windows_mut().remove(idx);

        if let Some(window) = &window {
            self.focus_history.remove(&window.hwnd);
            self.focus_window(self.focus_after_removal(idx, focused_idx));
        }

        window
    }

    /// The stack position to focus once the window at `idx` has been taken out.
    ///
    /// Removing a window which was not the one being shown must not change what the container is
    /// showing, so the focused window keeps its identity and only its index moves. Removing the
    /// window which *was* being shown - closing the top of a stack is the ordinary case - hands
    /// focus to the next window which could actually take it, searching down the stack first and
    /// then up, because a minimized window cannot be focused without being restored and restoring
    /// it is a separate decision.
    fn focus_after_removal(&self, idx: usize, focused_idx: usize) -> usize {
        if self.windows().is_empty() {
            return 0;
        }

        if focused_idx != idx {
            return if focused_idx > idx {
                focused_idx - 1
            } else {
                focused_idx
            };
        }

        // Everything which was above the removed window has dropped one place, so the window
        // directly below it is at `idx - 1` and the one directly above it is at `idx`.
        let below = (0..idx)
            .rev()
            .find(|i| self.windows()[*i].visibility == Visibility::Visible);

        let above = || {
            (idx..self.windows().len())
                .find(|i| self.windows()[*i].visibility == Visibility::Visible)
        };

        below
            .or_else(above)
            .unwrap_or_else(|| idx.min(self.windows().len() - 1))
    }

    pub fn remove_focused_window(&mut self) -> Option<ManagedWindow> {
        let focused_idx = self.focused_window_idx();
        self.remove_window_by_idx(focused_idx)
    }

    pub fn add_window(&mut self, window: Window) {
        self.add_managed_window(ManagedWindow::capture(window, self.id.clone()));
    }

    /// Add a previously managed window while preserving its independent state dimensions.
    /// Ownership always changes to this container, regardless of the source container ID.
    pub fn add_managed_window(&mut self, mut window: ManagedWindow) {
        window.container_id.clone_from(&self.id);
        self.windows_mut().push_back(window);
        self.focus_window(self.windows().len().saturating_sub(1));
        let focused_window_idx = self.focused_window_idx();

        for (i, window) in self.windows().iter().enumerate() {
            if i != focused_window_idx {
                window.hide();
            }
        }
    }

    /// Take on a window underneath everything this container is already holding.
    ///
    /// This is the receiving half of a distribution: the windows of a destroyed container are
    /// shared out among the survivors, and an arrival must not displace what its new container was
    /// showing. So it goes to the bottom of the stack rather than the top, it takes the oldest
    /// place in the focus history rather than the most recent one, and a stored window which was
    /// visible is hidden, because only the top of a stack is on screen.
    ///
    /// Everything else about the window is left alone. Its placement, visibility, presentation and
    /// floating rectangle are its own state, not its container's, and only its ownership changes.
    pub fn receive_window_at_bottom(&mut self, mut window: ManagedWindow) {
        window.container_id.clone_from(&self.id);

        if window.placement == ManagedPlacement::Stored && window.visibility == Visibility::Visible
        {
            window.hide();
        }

        let hwnd = window.hwnd;
        let was_empty = self.windows().is_empty();
        let focused_idx = self.focused_window_idx();

        self.windows.elements_mut().push_front(window);

        // Everything which was already here moved up one place, the focused window included, so
        // the ring index has to move with it or the container would start showing its neighbour.
        self.windows
            .focus(if was_empty { 0 } else { focused_idx + 1 });

        self.focus_history.record_oldest(hwnd);
    }

    /// Move `hwnd` to the top of this container's stack.
    ///
    /// The top is the last element and it is what the container shows, so raising is a change of
    /// depth rather than of membership: nothing is added, nothing is removed, and every other
    /// window keeps its relative order. The ring focus moves with the windows it indexes so the
    /// container goes on showing what it was showing, unless the raised window is the one being
    /// shown, which stays shown at its new depth.
    ///
    /// Returns whether this container owns the window.
    pub fn raise_window(&mut self, hwnd: isize) -> bool {
        let Some(idx) = self.idx_for_window(hwnd) else {
            return false;
        };

        let top = self.windows().len().saturating_sub(1);

        if idx == top {
            return true;
        }

        let focused_idx = self.focused_window_idx();

        if let Some(window) = self.windows.elements_mut().remove(idx) {
            self.windows.elements_mut().push_back(window);
        }

        // Everything above the raised window dropped one place, and the raised window itself is
        // now on top.
        self.windows.focus(if focused_idx == idx {
            top
        } else if focused_idx > idx {
            focused_idx - 1
        } else {
            focused_idx
        });

        true
    }

    /// The window one place below the top of this container's stack which could take focus.
    ///
    /// The top of the stack is what the container shows, so the "next" window is the one under it,
    /// and minimized windows are passed over on the way down for the same reason they are passed
    /// over everywhere else: focusing one would mean restoring it, which the caller did not ask
    /// for. A container holding fewer than two windows, or none below the top which can be
    /// focused, answers `None`.
    #[must_use]
    pub fn next_stack_window(&self) -> Option<isize> {
        let top = self.windows().len().checked_sub(1)?;

        (0..top)
            .rev()
            .map(|idx| &self.windows()[idx])
            .find(|window| window.visibility == Visibility::Visible)
            .map(|window| window.hwnd)
    }

    /// Raise the window under the top of the stack and show it.
    ///
    /// This is the whole of the "raise the next window" operation at container level: the depth
    /// change and the focus change together, so no caller can perform one without the other. The
    /// window's handle is returned when there was one to raise.
    pub fn raise_next_stack_window(&mut self) -> Option<isize> {
        let hwnd = self.next_stack_window()?;

        self.raise_window(hwnd);
        self.focus_window_by_hwnd(hwnd);

        Some(hwnd)
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_window(&mut self, idx: usize) {
        tracing::info!("focusing window");
        self.windows.focus(idx);

        if let Some(window) = self.windows.elements().get(idx) {
            let hwnd = window.hwnd;
            self.focus_history.record(hwnd);
        }
    }

    /// Focus the window with `hwnd` if this container owns it.
    pub fn focus_window_by_hwnd(&mut self, hwnd: isize) -> bool {
        match self.idx_for_window(hwnd) {
            Some(idx) => {
                self.focus_window(idx);
                true
            }
            None => false,
        }
    }

    /// The index of the window a manual split takes from this container.
    ///
    /// The most recently focused window this container still owns, or the top of its stack when
    /// its history names nothing it still holds. Visibility is deliberately not a condition here,
    /// unlike [`Container::first_focusable_window`]: a floating or minimized window can perfectly
    /// well be split off into a container of its own, which simply starts out hidden.
    #[must_use]
    pub fn donor_window_idx(&self) -> Option<usize> {
        if self.windows().is_empty() {
            return None;
        }

        self.focus_history
            .iter()
            .find_map(|hwnd| self.idx_for_window(*hwnd))
            .or_else(|| Some(self.focused_window_idx()))
    }

    pub const fn focus_history(&self) -> &Mru<isize> {
        &self.focus_history
    }

    /// The most recently focused window which can currently take focus.
    ///
    /// Minimized windows are never focus candidates. A container whose history has been emptied
    /// still answers with the top of its stack so focus selection cannot dead-end.
    pub fn first_focusable_window(&self) -> Option<&ManagedWindow> {
        let by_history = self
            .focus_history
            .iter()
            .filter_map(|hwnd| self.windows().iter().find(|window| window.hwnd == *hwnd))
            .find(|window| window.visibility == Visibility::Visible);

        by_history.or_else(|| {
            self.windows()
                .iter()
                .rev()
                .find(|window| window.visibility == Visibility::Visible)
        })
    }

    pub fn has_focusable_window(&self) -> bool {
        self.first_focusable_window().is_some()
    }

    /// Drop history entries for windows this container no longer owns and give every owned window
    /// which is missing from the history an oldest-position entry.
    ///
    /// Serialized state may predate the history, may have been written by another container, or
    /// may reference windows which have since been closed.
    pub fn repair_focus_history(&mut self) {
        let hwnds = self
            .windows()
            .iter()
            .map(|window| window.hwnd)
            .collect::<Vec<_>>();

        // Collecting rebuilds the history through the deduplicating insertion path, so a
        // serialized list which repeated an entry cannot survive the repair.
        self.focus_history = self
            .focus_history
            .iter()
            .copied()
            .filter(|hwnd| hwnds.contains(hwnd))
            .collect();

        // Reverse stack order: the top of the stack is the better recency guess.
        for hwnd in hwnds.iter().rev() {
            self.focus_history.record_oldest(*hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Rect;
    use serde_json;

    fn container_with_windows(count: isize) -> Container {
        let mut container = Container::default();

        for hwnd in 0..count {
            container.add_window(Window::from(hwnd));
        }

        container
    }

    #[test]
    fn raising_a_window_puts_it_on_top_without_reordering_the_rest() {
        let mut container = container_with_windows(4);

        assert!(container.raise_window(1));

        assert_eq!(
            container
                .windows()
                .iter()
                .map(|w| w.hwnd)
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 1]
        );
    }

    #[test]
    fn raising_a_window_keeps_the_container_showing_what_it_was_showing() {
        let mut container = container_with_windows(4);
        container.focus_window(3);

        container.raise_window(0);

        assert_eq!(
            container.focused_window().map(|window| window.hwnd),
            Some(3),
            "the window which was being shown moved down a place, not out of focus"
        );

        container.focus_window(1);
        let shown = container.focused_window().map(|window| window.hwnd);

        container.raise_window(shown.unwrap());

        assert_eq!(container.focused_window().map(|window| window.hwnd), shown);
        assert_eq!(container.windows().back().map(|w| w.hwnd), shown);
    }

    #[test]
    fn raising_the_top_window_or_an_unowned_one_reports_ownership() {
        let mut container = container_with_windows(2);
        let before = container
            .windows()
            .iter()
            .map(|w| w.hwnd)
            .collect::<Vec<_>>();

        assert!(container.raise_window(1));
        assert!(!container.raise_window(9));

        assert_eq!(
            container
                .windows()
                .iter()
                .map(|w| w.hwnd)
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn the_next_stack_window_is_the_one_under_the_top() {
        let container = container_with_windows(3);

        assert_eq!(container.next_stack_window(), Some(1));
    }

    #[test]
    fn the_next_stack_window_passes_over_minimized_windows() {
        let mut container = container_with_windows(4);
        container.windows_mut()[2].set_minimized();
        container.windows_mut()[1].set_minimized();

        assert_eq!(
            container.next_stack_window(),
            Some(0),
            "a minimized window cannot be focused, so the search continues down the stack"
        );
    }

    #[test]
    fn a_stack_with_nothing_focusable_under_the_top_has_no_next_window() {
        let mut container = container_with_windows(1);
        assert_eq!(container.next_stack_window(), None);

        container.add_window(Window::from(1));
        container.windows_mut()[0].set_minimized();
        assert_eq!(container.next_stack_window(), None);

        assert_eq!(Container::default().next_stack_window(), None);
    }

    #[test]
    fn raising_the_next_stack_window_shows_it_and_records_it() {
        let mut container = container_with_windows(3);

        assert_eq!(container.raise_next_stack_window(), Some(1));

        assert_eq!(
            container
                .windows()
                .iter()
                .map(|w| w.hwnd)
                .collect::<Vec<_>>(),
            vec![0, 2, 1],
            "the raised window is on top and the rest keep their relative depth"
        );
        assert_eq!(container.focused_window().map(|w| w.hwnd), Some(1));
        assert_eq!(container.focus_history().iter().next(), Some(&1));
    }

    #[test]
    fn closing_the_top_of_a_stack_focuses_the_next_window_which_can_take_focus() {
        let mut container = container_with_windows(4);
        container.windows_mut()[2].set_minimized();
        container.focus_window(3);

        container.remove_window_by_idx(3);

        assert_eq!(
            container.focused_window().map(|w| w.hwnd),
            Some(1),
            "the minimized window below the closed one is passed over"
        );
    }

    #[test]
    fn closing_the_bottom_of_a_stack_leaves_the_shown_window_shown() {
        let mut container = container_with_windows(3);
        container.focus_window(2);

        container.remove_window_by_idx(0);

        assert_eq!(
            container.focused_window().map(|w| w.hwnd),
            Some(2),
            "removing a window which was not being shown must not change what is shown"
        );
    }

    #[test]
    fn closing_the_only_focusable_window_below_the_top_looks_back_up_the_stack() {
        let mut container = container_with_windows(3);
        container.windows_mut()[0].set_minimized();
        container.focus_window(1);

        container.remove_window_by_idx(1);

        assert_eq!(
            container.focused_window().map(|w| w.hwnd),
            Some(2),
            "with nothing focusable below it, the search continues up the stack"
        );
    }

    #[test]
    fn a_container_with_a_visible_stored_window_is_active() {
        let container = container_with_windows(2);

        assert_eq!(container.state(), ContainerState::Active);
        assert!(container.is_active());
        assert!(!container.is_hidden());
        assert_eq!(container.visible_stored_windows().count(), 2);
        assert_eq!(container.unpositioned_window_count(), 0);
    }

    #[test]
    fn a_container_whose_windows_all_float_is_hidden() {
        let mut container = container_with_windows(2);

        for window in container.windows_mut().iter_mut() {
            window.set_floating(Rect::default());
        }

        assert_eq!(container.state(), ContainerState::Hidden);
        // The windows are still owned by the container: hiding is about slots, not ownership.
        assert_eq!(container.windows().len(), 2);
        assert_eq!(container.unpositioned_window_count(), 2);
    }

    #[test]
    fn a_container_whose_windows_are_all_minimized_is_hidden() {
        let mut container = container_with_windows(2);

        for window in container.windows_mut().iter_mut() {
            window.set_minimized();
        }

        assert!(container.is_hidden());
        assert_eq!(container.windows().len(), 2);
    }

    #[test]
    fn one_visible_stored_window_is_enough_to_keep_a_container_active() {
        let mut container = container_with_windows(3);
        container.windows_mut()[0].set_floating(Rect::default());
        container.windows_mut()[1].set_minimized();

        assert!(container.is_active());
        assert_eq!(container.visible_stored_windows().count(), 1);
        assert_eq!(container.unpositioned_window_count(), 2);
    }

    #[test]
    fn maximized_and_fullscreen_windows_keep_their_container_active() {
        for presentation in [Presentation::Maximized, Presentation::Fullscreen] {
            let mut container = container_with_windows(1);
            container.windows_mut()[0].presentation = presentation;

            assert!(container.is_active(), "{presentation:?} must stay active");
        }
    }

    #[test]
    fn the_two_presentations_are_reported_separately() {
        let mut container = container_with_windows(2);
        container.windows_mut()[0].set_maximized(Rect::default());
        container.windows_mut()[1].set_fullscreen(Rect::default());

        assert_eq!(
            container.maximized_managed_window().map(|w| w.hwnd),
            Some(0)
        );
        assert_eq!(
            container.fullscreened_managed_window().map(|w| w.hwnd),
            Some(1)
        );
        assert_eq!(
            container.presented_managed_window().map(|w| w.hwnd),
            Some(0)
        );
    }

    #[test]
    fn a_minimized_window_presents_nothing() {
        let mut container = container_with_windows(1);
        container.windows_mut()[0].set_fullscreen(Rect::default());
        container.windows_mut()[0].set_minimized();

        assert!(container.fullscreened_managed_window().is_none());
        assert!(container.presented_managed_window().is_none());
        // The presentation itself is kept, so restoring the window brings it back fullscreen.
        assert!(container.windows()[0].is_fullscreen());
    }

    #[test]
    fn a_mixed_floating_and_minimized_container_is_hidden() {
        let mut container = container_with_windows(2);
        container.windows_mut()[0].set_floating(Rect::default());
        container.windows_mut()[1].set_minimized();

        assert!(container.is_hidden());
    }

    #[test]
    fn a_floating_and_minimized_window_alone_hides_its_container() {
        let mut container = container_with_windows(1);
        container.windows_mut()[0].set_floating(Rect::default());
        container.windows_mut()[0].set_minimized();

        assert!(container.is_hidden());
    }

    #[test]
    fn a_preselect_marker_stays_active() {
        // It owns no window by design; it is a reserved place in the arrangement.
        assert!(Container::preselect().is_active());
    }

    #[test]
    fn test_contains_window() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Should return true for existing windows
        assert!(container.contains_window(1));
        assert_eq!(container.idx_for_window(1), Some(1));

        // Should return false since window 4 doesn't exist
        assert!(!container.contains_window(4));
        assert_eq!(container.idx_for_window(4), None);
    }

    #[test]
    fn test_remove_window_by_idx() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Remove window 1
        container.remove_window_by_idx(1);

        // Should only have 2 windows left
        assert_eq!(container.windows().len(), 2);

        // Should return false since window 1 was removed
        assert!(!container.contains_window(1));
    }

    #[test]
    fn test_remove_focused_window() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Should be focused on the last created window
        assert_eq!(container.focused_window_idx(), 2);

        // Remove the focused window
        container.remove_focused_window();

        // Should be focused on the window before the removed one
        assert_eq!(container.focused_window_idx(), 1);

        // Should only have 2 windows left
        assert_eq!(container.windows().len(), 2);
    }

    #[test]
    fn test_add_window() {
        let mut container = Container::default();

        container.add_window(Window::from(1));

        assert_eq!(container.windows().len(), 1);
        assert_eq!(container.focused_window_idx(), 0);
        assert!(container.contains_window(1));
        assert_eq!(container.windows()[0].container_id, container.id);
    }

    #[test]
    fn mutable_window_ring_insertion_assigns_ownership() {
        let mut container = Container::default();

        container.windows_mut().push_back(Window::from(42));

        assert_eq!(container.windows()[0].container_id, container.id);
    }

    #[test]
    fn test_focus_window() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Should focus on the last created window
        assert_eq!(container.focused_window_idx(), 2);

        // focus on the window at index 1
        container.focus_window(1);

        // Should be focused on window 1
        assert_eq!(container.focused_window_idx(), 1);

        // focus on the window at index 0
        container.focus_window(0);

        // Should be focused on window 0
        assert_eq!(container.focused_window_idx(), 0);
    }

    #[test]
    fn test_idx_for_window() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Should return the index of the window
        assert_eq!(container.idx_for_window(1), Some(1));

        // Should return None since window 4 doesn't exist
        assert_eq!(container.idx_for_window(4), None);
    }

    #[test]
    fn deserializes_with_missing_locked_field_defaults_to_false() {
        let json = r#"{
            "id": "test-1",
            "windows": { "elements": [], "focused": 0 }
        }"#;
        let container: Container = serde_json::from_str(json).expect("Should deserialize");

        assert!(!container.locked);
        assert_eq!(container.id, "test-1");
        assert!(container.windows().is_empty());

        let json = r#"{
            "id": "test-2",
            "windows": { "elements": [ { "hwnd": 5 }, { "hwnd": 9 } ], "focused": 1 }
        }"#;
        let container: Container = serde_json::from_str(json).unwrap();
        assert_eq!(container.id, "test-2");
        assert!(!container.locked);
        assert_eq!(container.windows()[0].window, Window::from(5));
        assert_eq!(container.windows()[1].window, Window::from(9));
        assert!(
            container
                .windows()
                .iter()
                .all(|window| window.container_id == container.id)
        );
        assert_eq!(container.focused_window_idx(), 1);
    }

    #[test]
    fn add_and_move_managed_window_maintain_ownership() {
        let mut source = Container::default();
        let mut target = Container::default();
        let mut window =
            ManagedWindow::from_observed(Window::from(42), "stale-owner", true, true, false);
        window.set_floating(Default::default());

        source.add_managed_window(window);
        assert_eq!(source.windows()[0].container_id, source.id);

        let window = source.remove_focused_window().unwrap();
        target.add_managed_window(window);

        let moved = &target.windows()[0];
        assert_eq!(moved.container_id, target.id);
        assert_eq!(moved.placement, crate::ManagedPlacement::Floating);
        assert_eq!(moved.visibility, crate::Visibility::Minimized);
        assert_eq!(moved.presentation, crate::Presentation::Maximized);
    }

    #[test]
    fn deserialization_repairs_stale_current_owner() {
        let json = r#"{
            "id": "container-1",
            "windows": {
                "elements": [{
                    "window": {"hwnd": 42},
                    "container_id": "container-elsewhere",
                    "placement": "Floating"
                }],
                "focused": 0
            }
        }"#;
        let container: Container = serde_json::from_str(json).unwrap();

        assert_eq!(container.windows()[0].container_id, "container-1");
        assert_eq!(
            container.windows()[0].placement,
            crate::ManagedPlacement::Floating
        );
    }

    #[test]
    fn focusing_a_window_records_it_as_most_recent() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        // Adding focuses, so the history already reflects insertion order.
        assert_eq!(
            container
                .focus_history()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 1, 0]
        );

        container.focus_window(0);
        container.focus_window(1);

        assert_eq!(
            container
                .focus_history()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 0, 2]
        );
        assert_eq!(container.focus_history().len(), 3);
    }

    #[test]
    fn focusing_by_hwnd_only_succeeds_for_owned_windows() {
        let mut container = Container::default();
        container.add_window(Window::from(1));
        container.add_window(Window::from(2));

        assert!(container.focus_window_by_hwnd(1));
        assert_eq!(container.focused_window_idx(), 0);
        assert_eq!(container.focus_history().most_recent(), Some(&1));

        assert!(!container.focus_window_by_hwnd(9));
        assert_eq!(container.focused_window_idx(), 0);
    }

    #[test]
    fn removing_a_window_drops_its_history_entry() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        container.focus_window(1);
        container.remove_window_by_idx(1);

        assert!(!container.focus_history().contains(&1));
        assert_eq!(container.focus_history().len(), 2);
    }

    #[test]
    fn focus_selection_prefers_recency_and_skips_minimized_windows() {
        let mut container = Container::default();

        for i in 0..3 {
            container.add_window(Window::from(i));
        }

        container.focus_window(0);

        assert_eq!(container.first_focusable_window().unwrap().hwnd, 0);

        container.windows_mut()[0].set_minimized();

        // 2 was focused before 0, because adding a window focuses it.
        assert_eq!(container.first_focusable_window().unwrap().hwnd, 2);

        for window in container.windows_mut() {
            window.set_minimized();
        }

        assert!(container.first_focusable_window().is_none());
        assert!(!container.has_focusable_window());
    }

    #[test]
    fn focus_selection_falls_back_to_the_top_of_the_stack() {
        let mut container = Container::default();
        container.windows_mut().push_back(Window::from(1));
        container.windows_mut().push_back(Window::from(2));

        // Direct ring insertion does not record focus.
        assert!(container.focus_history().is_empty());
        assert_eq!(container.first_focusable_window().unwrap().hwnd, 2);
    }

    #[test]
    fn deserialization_repairs_a_stale_or_missing_focus_history() {
        let json = r#"{
            "id": "container-1",
            "windows": { "elements": [ { "hwnd": 5 }, { "hwnd": 9 } ], "focused": 0 },
            "focus_history": [ 404, 9 ]
        }"#;
        let container: Container = serde_json::from_str(json).unwrap();

        // 404 is not owned by this container and 5 was missing from the history.
        assert_eq!(
            container
                .focus_history()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![9, 5]
        );
    }

    #[test]
    fn legacy_container_json_without_a_focus_history_gets_one() {
        let json = r#"{
            "id": "container-1",
            "windows": { "elements": [ { "hwnd": 5 }, { "hwnd": 9 } ], "focused": 0 }
        }"#;
        let container: Container = serde_json::from_str(json).unwrap();

        assert_eq!(
            container
                .focus_history()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![9, 5]
        );
    }

    #[test]
    fn serializes_and_deserializes() {
        let mut container = Container::default();
        container.set_locked(true);
        let mut window =
            ManagedWindow::from_observed(Window::from(42), container.id.clone(), true, false, true);
        window.set_floating(Default::default());
        container.add_managed_window(window);

        let serialized = serde_json::to_string(&container).expect("Should serialize");
        let deserialized: Container =
            serde_json::from_str(&serialized).expect("Should deserialize");

        assert!(deserialized.locked);
        assert_eq!(deserialized, container);
    }
    #[test]
    fn a_donor_window_is_the_most_recent_one_the_container_still_owns() {
        let mut container = container_with_windows(3);
        container.focus_window_by_hwnd(0);

        assert_eq!(container.donor_window_idx(), Some(0));

        // A window the container no longer owns is skipped, however recent it is.
        container.remove_window_by_idx(0);
        container.focus_window_by_hwnd(2);
        container.focus_window_by_hwnd(1);

        assert_eq!(container.donor_window_idx(), Some(0));
        assert_eq!(container.windows()[0].hwnd, 1);
    }

    #[test]
    fn a_donor_without_a_usable_history_gives_away_the_top_of_its_stack() {
        let mut container = container_with_windows(3);
        container.focus_history.clear();

        assert_eq!(
            container.donor_window_idx(),
            Some(container.focused_window_idx())
        );
    }

    #[test]
    fn a_minimized_or_floating_window_can_still_be_split_off() {
        let mut container = container_with_windows(2);
        container
            .windows_mut()
            .iter_mut()
            .next()
            .unwrap()
            .set_minimized();
        container.focus_history.clear();
        container.focus_history.record(0);

        // Unlike focus selection, donor selection does not require a visible window.
        assert_eq!(container.donor_window_idx(), Some(0));
    }

    #[test]
    fn an_empty_container_has_no_donor_window() {
        assert_eq!(Container::default().donor_window_idx(), None);
    }
}
