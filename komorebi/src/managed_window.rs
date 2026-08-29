use std::ops::Deref;
use std::ops::DerefMut;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::core::Rect;
use crate::model::ContainerId;
use crate::window::Window;
use crate::windows_api::WindowsApi;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ManagedPlacement {
    #[default]
    Stored,
    Floating,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Visibility {
    #[default]
    Visible,
    Minimized,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Presentation {
    #[default]
    Normal,
    Maximized,
    Fullscreen,
}

/// Runtime and serialized state for a window owned by a container.
///
/// Container migration fills an empty or stale ID accepted from legacy state with the
/// deserializing container's stable ID.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ManagedWindow {
    pub window: Window,
    pub container_id: ContainerId,
    pub placement: ManagedPlacement,
    pub visibility: Visibility,
    pub presentation: Presentation,
    pub floating_rect: Option<Rect>,
    pub restore_rect: Option<Rect>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ManagedWindowRepr {
    Current {
        window: Window,
        #[serde(default)]
        container_id: ContainerId,
        #[serde(default)]
        placement: ManagedPlacement,
        #[serde(default)]
        visibility: Visibility,
        #[serde(default)]
        presentation: Presentation,
        #[serde(default)]
        floating_rect: Option<Rect>,
        #[serde(default)]
        restore_rect: Option<Rect>,
    },
    Legacy(Window),
}

impl<'de> Deserialize<'de> for ManagedWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ManagedWindowRepr::deserialize(deserializer)? {
            ManagedWindowRepr::Current {
                window,
                container_id,
                placement,
                visibility,
                presentation,
                floating_rect,
                restore_rect,
            } => Self {
                window,
                container_id,
                placement,
                visibility,
                presentation,
                floating_rect,
                restore_rect,
            },
            ManagedWindowRepr::Legacy(window) => Self::from_observed(
                window,
                ContainerId::default(),
                false,
                false,
                false,
            ),
        })
    }
}

impl Deref for ManagedWindow {
    type Target = Window;

    fn deref(&self) -> &Self::Target {
        &self.window
    }
}

impl DerefMut for ManagedWindow {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.window
    }
}

impl ManagedWindow {
    pub fn capture(window: Window, container_id: ContainerId) -> Self {
        Self::from_observed(
            window,
            container_id,
            window.is_miminized(),
            window.is_maximized(),
            WindowsApi::is_fullscreen(window.hwnd),
        )
    }

    pub fn from_observed(
        window: Window,
        container_id: impl Into<ContainerId>,
        minimized: bool,
        maximized: bool,
        fullscreen: bool,
    ) -> Self {
        let visibility = if minimized {
            Visibility::Minimized
        } else {
            Visibility::Visible
        };

        let presentation = if fullscreen {
            Presentation::Fullscreen
        } else if maximized {
            Presentation::Maximized
        } else {
            Presentation::Normal
        };

        Self {
            window,
            container_id: container_id.into(),
            placement: ManagedPlacement::Stored,
            visibility,
            presentation,
            floating_rect: None,
            restore_rect: None,
        }
    }

    pub fn is_visible_stored(&self) -> bool {
        self.visibility == Visibility::Visible && self.placement == ManagedPlacement::Stored
    }

    pub fn set_floating(&mut self, current_rect: Rect) -> bool {
        if self.placement == ManagedPlacement::Floating {
            return false;
        }

        self.placement = ManagedPlacement::Floating;
        self.floating_rect = Some(current_rect);
        true
    }

    pub fn set_stored(&mut self) -> bool {
        if self.placement == ManagedPlacement::Stored {
            return false;
        }

        self.placement = ManagedPlacement::Stored;
        true
    }

    pub fn set_minimized(&mut self) -> bool {
        if self.visibility == Visibility::Minimized {
            return false;
        }

        self.visibility = Visibility::Minimized;
        true
    }

    pub fn set_visible(&mut self) -> bool {
        if self.visibility == Visibility::Visible {
            return false;
        }

        self.visibility = Visibility::Visible;
        true
    }

    pub fn set_maximized(&mut self, current_rect: Rect) -> bool {
        self.set_presentation(Presentation::Maximized, current_rect)
    }

    pub fn set_fullscreen(&mut self, current_rect: Rect) -> bool {
        self.set_presentation(Presentation::Fullscreen, current_rect)
    }

    fn set_presentation(&mut self, presentation: Presentation, current_rect: Rect) -> bool {
        if self.presentation == presentation {
            return false;
        }

        if self.presentation == Presentation::Normal {
            self.restore_rect = Some(current_rect);
        }

        self.presentation = presentation;
        true
    }

    /// Return the rectangle to apply after leaving maximized or fullscreen presentation.
    pub fn set_normal(&mut self, stored_rect: Rect) -> Option<Rect> {
        if self.presentation == Presentation::Normal {
            return None;
        }

        let target = match self.placement {
            ManagedPlacement::Stored => stored_rect,
            ManagedPlacement::Floating => self.floating_rect.or(self.restore_rect)?,
        };
        self.presentation = Presentation::Normal;
        self.restore_rect = None;
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32) -> Rect {
        Rect {
            left,
            top: 20,
            right: 800,
            bottom: 600,
        }
    }

    fn managed() -> ManagedWindow {
        ManagedWindow::from_observed(Window::from(42), "container-1", false, false, false)
    }

    #[test]
    fn observed_state_keeps_visibility_and_presentation_independent() {
        let window = ManagedWindow::from_observed(
            Window::from(42),
            "container-1",
            true,
            true,
            false,
        );

        assert_eq!(window.placement, ManagedPlacement::Stored);
        assert_eq!(window.visibility, Visibility::Minimized);
        assert_eq!(window.presentation, Presentation::Maximized);
    }

    #[test]
    fn fullscreen_observation_is_distinct_from_maximized() {
        let window = ManagedWindow::from_observed(
            Window::from(42),
            "container-1",
            false,
            false,
            true,
        );

        assert_eq!(window.presentation, Presentation::Fullscreen);
    }

    #[test]
    fn floating_and_stored_transitions_preserve_ownership_and_other_state() {
        let mut window = managed();
        window.set_minimized();
        window.set_maximized(rect(1));

        assert!(window.set_floating(rect(2)));
        assert_eq!(window.container_id, "container-1");
        assert_eq!(window.floating_rect, Some(rect(2)));
        assert_eq!(window.visibility, Visibility::Minimized);
        assert_eq!(window.presentation, Presentation::Maximized);

        assert!(window.set_stored());
        assert_eq!(window.container_id, "container-1");
        assert_eq!(window.floating_rect, Some(rect(2)));
        assert_eq!(window.visibility, Visibility::Minimized);
        assert_eq!(window.presentation, Presentation::Maximized);
    }

    #[test]
    fn minimize_and_restore_preserve_presentation() {
        let mut window = managed();
        window.set_maximized(rect(1));

        assert!(window.set_minimized());
        assert_eq!(window.presentation, Presentation::Maximized);
        assert!(window.set_visible());
        assert_eq!(window.presentation, Presentation::Maximized);
    }

    #[test]
    fn leaving_presentation_uses_rect_for_current_placement() {
        let mut stored = managed();
        stored.set_maximized(rect(1));
        assert_eq!(stored.set_normal(rect(3)), Some(rect(3)));

        let mut floating = managed();
        floating.set_floating(rect(2));
        floating.set_fullscreen(rect(2));
        assert_eq!(floating.set_normal(rect(3)), Some(rect(2)));
    }

    #[test]
    fn leaving_presentation_is_atomic_without_a_floating_target() {
        let mut window = managed();
        window.placement = ManagedPlacement::Floating;
        window.presentation = Presentation::Fullscreen;

        assert_eq!(window.set_normal(rect(3)), None);
        assert_eq!(window.presentation, Presentation::Fullscreen);
    }

    #[test]
    fn legacy_window_json_gets_compatible_defaults() {
        let window: ManagedWindow = serde_json::from_str(r#"{"hwnd":42}"#).unwrap();

        assert_eq!(window.window, Window::from(42));
        assert!(window.container_id.is_empty());
        assert_eq!(window.placement, ManagedPlacement::Stored);
        assert_eq!(window.visibility, Visibility::Visible);
        assert_eq!(window.presentation, Presentation::Normal);
    }

    #[test]
    fn missing_current_state_fields_get_compatible_defaults() {
        let window: ManagedWindow = serde_json::from_str(
            r#"{"window":{"hwnd":42},"container_id":"container-1"}"#,
        )
        .unwrap();

        assert_eq!(window.container_id, "container-1");
        assert_eq!(window.placement, ManagedPlacement::Stored);
        assert_eq!(window.visibility, Visibility::Visible);
        assert_eq!(window.presentation, Presentation::Normal);
    }

    #[test]
    fn managed_window_state_roundtrips() {
        let mut expected = managed();
        expected.set_floating(rect(2));
        expected.set_fullscreen(rect(2));
        expected.set_minimized();

        let json = serde_json::to_string(&expected).unwrap();
        let actual: ManagedWindow = serde_json::from_str(&json).unwrap();

        assert_eq!(actual, expected);
    }
}
