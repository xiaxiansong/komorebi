use std::collections::VecDeque;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Deref;
use std::ops::DerefMut;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::Lockable;
use crate::focus_history::Mru;
use crate::managed_window::ManagedPlacement;
use crate::managed_window::ManagedWindow;
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Container {
    pub id: ContainerId,
    #[serde(default)]
    pub locked: bool,
    windows: Ring<ManagedWindow>,
    /// Most-recently-used order of this container's window handles.
    ///
    /// The ring focus index is a position in the stack, so it cannot answer which window should
    /// be focused after the current one goes away. This history can, and it is the only source
    /// used for that decision.
    #[serde(default)]
    focus_history: Mru<isize>,
}

#[derive(Deserialize)]
struct ContainerRepr {
    id: ContainerId,
    #[serde(default)]
    locked: bool,
    windows: Ring<ManagedWindow>,
    #[serde(default)]
    focus_history: Mru<isize>,
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
        let mut container = Self {
            id: repr.id,
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
    /// because being minimized is a window state this container does not own.
    pub fn restore(&self) {
        if let Some(window) = self.focused_visible_stored_window() {
            window.restore();
        }

        for window in self.visible_floating_windows() {
            window.restore();
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
                (ManagedPlacement::Floating, _) => window.restore_with_border(false),
                (ManagedPlacement::Stored, _) => {
                    if i == focused_idx {
                        window.restore_with_border(false);
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
        self.focused_managed_window()
            .filter(|window| window.is_visible_stored())
            .or_else(|| self.visible_stored_windows().next())
            .map(|window| &window.window)
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
        let window = self.windows_mut().remove(idx);

        if let Some(window) = &window {
            self.focus_history.remove(&window.hwnd);
        }

        self.focus_window(idx.saturating_sub(1));
        window
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
    use crate::managed_window::Presentation;
    use serde_json;

    fn container_with_windows(count: isize) -> Container {
        let mut container = Container::default();

        for hwnd in 0..count {
            container.add_window(Window::from(hwnd));
        }

        container
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
}
