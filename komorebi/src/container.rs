use std::collections::VecDeque;
use std::ops::Deref;
use std::ops::DerefMut;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::Lockable;
use crate::managed_window::ManagedWindow;
use crate::model::ContainerId;
use crate::ring::Ring;
use crate::window::Window;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Container {
    pub id: ContainerId,
    #[serde(default)]
    pub locked: bool,
    windows: Ring<ManagedWindow>,
}

#[derive(Deserialize)]
struct ContainerRepr {
    id: ContainerId,
    #[serde(default)]
    locked: bool,
    windows: Ring<ManagedWindow>,
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
        };

        // The container is the authority for ownership. This both migrates legacy serialized
        // windows (whose owner defaults to an empty ID) and repairs stale owner IDs.
        let container_id = container.id.clone();
        for window in container.windows_mut() {
            window.container_id.clone_from(&container_id);
        }

        Ok(container)
    }
}

impl Default for Container {
    fn default() -> Self {
        Self {
            id: ContainerId::new(),
            locked: false,
            windows: Ring::default(),
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
        self.windows
            .focused_mut()
            .map(|window| &mut window.window)
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
        }
    }

    pub fn is_preselect(&self) -> bool {
        self.id.as_str() == "PRESELECT"
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

    pub fn restore(&self) {
        if let Some(window) = self.focused_window() {
            window.restore();
        }
    }

    /// Hides the unfocused windows of the container and restores the focused one. This function
    /// is used to make sure we update the window that should be shown on a stack. If the container
    /// isn't a stack this function won't change anything.
    pub fn load_focused_window(&mut self) {
        let focused_idx = self.focused_window_idx();

        for (i, window) in self.windows_mut().iter_mut().enumerate() {
            if i == focused_idx {
                window.restore_with_border(false);
            } else {
                window.hide_with_border(false);
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
        let window = self.windows_mut().remove(idx);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

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
        assert!(container
            .windows()
            .iter()
            .all(|window| window.container_id == container.id));
        assert_eq!(container.focused_window_idx(), 1);
    }

    #[test]
    fn add_and_move_managed_window_maintain_ownership() {
        let mut source = Container::default();
        let mut target = Container::default();
        let mut window = ManagedWindow::from_observed(
            Window::from(42),
            "stale-owner",
            true,
            true,
            false,
        );
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
    fn serializes_and_deserializes() {
        let mut container = Container::default();
        container.set_locked(true);
        let mut window = ManagedWindow::from_observed(
            Window::from(42),
            container.id.clone(),
            true,
            false,
            true,
        );
        window.set_floating(Default::default());
        container.add_managed_window(window);

        let serialized = serde_json::to_string(&container).expect("Should serialize");
        let deserialized: Container =
            serde_json::from_str(&serialized).expect("Should deserialize");

        assert!(deserialized.locked);
        assert_eq!(deserialized, container);
    }
}
