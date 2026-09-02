use std::fmt;
use std::ops::Deref;
use std::ops::DerefMut;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::border_manager;
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

impl Presentation {
    /// Fold a Win32 observation into a presentation.
    ///
    /// The precedence is the one [`ManagedWindow::from_observed`] uses when a window is first
    /// managed, and it is not arbitrary: `WindowsApi::is_fullscreen` refuses a maximized window,
    /// so the two observations cannot both be true for the same window.
    #[must_use]
    pub const fn observed(maximized: bool, fullscreen: bool) -> Self {
        if fullscreen {
            Self::Fullscreen
        } else if maximized {
            Self::Maximized
        } else {
            Self::Normal
        }
    }

    /// The presentation this record should become given an observation of the live window, or
    /// `None` when the observation changes nothing.
    ///
    /// Only one of the two observations is ever believed, and only in one direction.
    ///
    /// `is_zoomed` is a window state which Windows sets synchronously, so an observation that a
    /// window komorebi recorded as maximized is no longer maximized is a fact about the user or
    /// the application, not a race with komorebi's own call. Not believing it is what made
    /// komorebi re-maximize, at the next retile, a window the user had just restored by hand.
    ///
    /// Fullscreen is a rectangle rather than a window state, and it is the rectangle komorebi
    /// itself writes when it puts a window fullscreen. An observation which disagrees with a
    /// fullscreen record therefore cannot tell "the application left fullscreen" apart from "the
    /// rectangle has not landed yet", so it is never acted on; and an observation which agrees
    /// with no record cannot tell an application's own fullscreen apart from a window which simply
    /// fills its monitor. Both are left to the commands which own the presentation.
    ///
    /// Entering `Maximized` from `Normal` is not believed either. A tiled window is not maximized
    /// by hand in a tiling window manager, the retile already restores one which is, and believing
    /// it here would undo a command: an application which still reports itself maximized shortly
    /// after komorebi unmaximized it would drag the record straight back.
    #[must_use]
    pub const fn reconcile(self, observed: Self) -> Option<Self> {
        match (self, observed) {
            // A window which komorebi recorded as maximized is not maximized any more.
            (Self::Maximized, Self::Normal | Self::Fullscreen) => Some(Self::Normal),
            // A window which komorebi put fullscreen has been maximized out of it.
            (Self::Fullscreen, Self::Maximized) => Some(Self::Maximized),
            _ => None,
        }
    }
}

/// Why a floating geometry command did not act on a window.
///
/// These are refusals rather than failures: the command found a window and decided it was the
/// wrong kind of window, so nothing at all was changed. They are values instead of error strings
/// because the command surface has to be able to tell "you asked to move a tiled window" apart
/// from "komorebi could not do it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum FloatingRejection {
    /// There is no focused window for the command to act on.
    NoSubject,
    /// The window is positioned by its container, so it has no rectangle of its own to change.
    NotFloating,
    /// The window is minimized, so it has no rectangle on screen.
    Minimized,
    /// The window is drawn over the arrangement rather than at its own rectangle.
    Presented(Presentation),
    /// The window floats, but neither the model nor Win32 could say where it currently is.
    UnknownGeometry,
}

impl fmt::Display for FloatingRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSubject => write!(f, "there is no focused window"),
            Self::NotFloating => write!(f, "the focused window is not floating"),
            Self::Minimized => write!(f, "the focused window is minimized"),
            Self::Presented(presentation) => {
                write!(f, "the focused window is {presentation:?}")
            }
            Self::UnknownGeometry => {
                write!(f, "the focused window has no known floating rectangle")
            }
        }
    }
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
            ManagedWindowRepr::Legacy(window) => {
                Self::from_observed(window, ContainerId::default(), false, false, false)
            }
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

    /// Whether Win32 is holding this window on the taskbar, as far as the model knows.
    #[must_use]
    pub fn is_minimized(&self) -> bool {
        self.visibility == Visibility::Minimized
    }

    #[must_use]
    pub fn is_maximized(&self) -> bool {
        self.presentation == Presentation::Maximized
    }

    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.presentation == Presentation::Fullscreen
    }

    /// Whether this window is drawn over the arrangement instead of inside its container's slot.
    ///
    /// Maximized and fullscreen are separate presentations which are applied through separate
    /// Win32 calls, but they answer this question the same way: the container still owns its slot
    /// and the window is still its member, and neither is positioned into that slot.
    #[must_use]
    pub fn is_presented(&self) -> bool {
        self.presentation != Presentation::Normal
    }

    /// Bring this window back on screen the way its recorded presentation requires.
    ///
    /// A plain restore is `SW_RESTORE`, which also unmaximizes. Showing a maximized window that
    /// way would leave Win32 and the model disagreeing about a presentation the model owns, so a
    /// maximized window is shown by maximizing it instead. A fullscreen window is restored, not
    /// maximized: its presentation is a rectangle rather than a Win32 window state, and the
    /// retile which follows is what puts it back on the monitor bounds.
    ///
    /// A window this model records as `Visible` may still be minimized as far as Win32 is
    /// concerned - it has just been restored, or its state was reconciled from elsewhere - and
    /// under the default `Cloak` hiding behaviour uncloaking it would leave it on the taskbar. The
    /// recorded visibility is what licenses [`Window::unminimize`] here: a window the model
    /// believes minimized is never shown through this path at all, so this cannot un-minimize a
    /// window the user put away. Maximizing already restores, so only the other presentations need
    /// it.
    pub fn show(&self, with_border: bool) {
        match self.presentation {
            Presentation::Maximized => {
                self.window.maximize();

                if with_border {
                    border_manager::show_border(self.window.hwnd);
                }
            }
            Presentation::Normal | Presentation::Fullscreen => {
                if self.visibility == Visibility::Visible {
                    self.window.unminimize();
                }

                self.window.restore_with_border(with_border);
            }
        }
    }

    /// The rectangle a floating move or resize may act on.
    ///
    /// Only a visible, floating, `Normal` window has one. A stored window's rectangle belongs to
    /// its container's slot, a minimized window has none, and a maximized or fullscreen window is
    /// drawn somewhere its own rectangle does not describe; changing any of those here would
    /// silently contradict a state this model owns elsewhere.
    ///
    /// `observed` is what Win32 currently reports for the window and takes precedence over the
    /// recorded rectangle, because the user can drag a floating window with the mouse without any
    /// command passing through here. The record is where the last commanded geometry was stored,
    /// not a claim about where the window is now.
    pub fn floating_geometry(&self, observed: Option<Rect>) -> Result<Rect, FloatingRejection> {
        if self.placement != ManagedPlacement::Floating {
            return Err(FloatingRejection::NotFloating);
        }

        if self.visibility == Visibility::Minimized {
            return Err(FloatingRejection::Minimized);
        }

        if self.presentation != Presentation::Normal {
            return Err(FloatingRejection::Presented(self.presentation));
        }

        observed
            .or(self.floating_rect)
            .ok_or(FloatingRejection::UnknownGeometry)
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

    /// Enter a presentation, remembering the rectangle a return to Normal has to restore.
    ///
    /// The restore rectangle is captured only when leaving Normal. Going straight from one
    /// presentation to the other therefore keeps the rectangle the window had before any of this
    /// started, instead of recording the monitor-sized rectangle the previous presentation had
    /// given it and restoring to that.
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

    /// Bring the recorded presentation into agreement with what Win32 reports, changing nothing
    /// else about the window.
    ///
    /// This is not a command: the window is already where the observation says it is, so no Win32
    /// call is made and no rectangle is applied. Ownership, placement, visibility and stack
    /// position are untouched, which is what makes running it again for the same observation a
    /// no-op rather than a toggle.
    ///
    /// A floating window's rectangle follows the observation, because the record is where the last
    /// commanded geometry was stored and a window which left a presentation by itself has just
    /// chosen a new one.
    pub fn adopt_presentation(
        &mut self,
        observed: Presentation,
        observed_rect: Option<Rect>,
    ) -> bool {
        let Some(target) = self.presentation.reconcile(observed) else {
            return false;
        };

        self.presentation = target;

        if target == Presentation::Normal {
            self.restore_rect = None;
        }

        if self.placement == ManagedPlacement::Floating
            && target == Presentation::Normal
            && let Some(rect) = observed_rect
        {
            self.floating_rect = Some(rect);
        }

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
        let window =
            ManagedWindow::from_observed(Window::from(42), "container-1", true, true, false);

        assert_eq!(window.placement, ManagedPlacement::Stored);
        assert_eq!(window.visibility, Visibility::Minimized);
        assert_eq!(window.presentation, Presentation::Maximized);
    }

    #[test]
    fn fullscreen_observation_is_distinct_from_maximized() {
        let window =
            ManagedWindow::from_observed(Window::from(42), "container-1", false, false, true);

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
    fn an_observation_folds_into_a_presentation_with_fullscreen_winning() {
        assert_eq!(Presentation::observed(false, false), Presentation::Normal);
        assert_eq!(Presentation::observed(true, false), Presentation::Maximized);
        assert_eq!(
            Presentation::observed(false, true),
            Presentation::Fullscreen
        );
        assert_eq!(Presentation::observed(true, true), Presentation::Fullscreen);
    }

    #[test]
    fn an_agreeing_observation_changes_nothing() {
        for presentation in [
            Presentation::Normal,
            Presentation::Maximized,
            Presentation::Fullscreen,
        ] {
            assert_eq!(presentation.reconcile(presentation), None);
        }
    }

    #[test]
    fn a_window_which_stopped_being_maximized_returns_to_normal() {
        assert_eq!(
            Presentation::Maximized.reconcile(Presentation::Normal),
            Some(Presentation::Normal)
        );
        assert_eq!(
            Presentation::Maximized.reconcile(Presentation::Fullscreen),
            Some(Presentation::Normal)
        );
    }

    #[test]
    fn a_fullscreen_window_which_was_maximized_out_of_it_follows() {
        assert_eq!(
            Presentation::Fullscreen.reconcile(Presentation::Maximized),
            Some(Presentation::Maximized)
        );
    }

    #[test]
    fn an_observation_never_starts_a_presentation_komorebi_did_not_command() {
        // The retile restores a tiled window which was maximized by hand, and a fullscreen
        // rectangle cannot be told apart from one komorebi wrote itself.
        assert_eq!(
            Presentation::Normal.reconcile(Presentation::Maximized),
            None
        );
        assert_eq!(
            Presentation::Normal.reconcile(Presentation::Fullscreen),
            None
        );
        assert_eq!(
            Presentation::Fullscreen.reconcile(Presentation::Normal),
            None
        );
    }

    #[test]
    fn adopting_an_observation_keeps_everything_but_the_presentation() {
        let mut window = managed();
        window.set_maximized(rect(1));

        assert!(window.adopt_presentation(Presentation::Normal, Some(rect(5))));

        assert_eq!(window.presentation, Presentation::Normal);
        assert_eq!(window.container_id, "container-1");
        assert_eq!(window.placement, ManagedPlacement::Stored);
        assert_eq!(window.visibility, Visibility::Visible);
        assert_eq!(window.restore_rect, None);
        // A stored window returns to its container's slot, so nothing here records a rectangle.
        assert_eq!(window.floating_rect, None);
    }

    #[test]
    fn adopting_the_same_observation_twice_changes_nothing_the_second_time() {
        let mut window = managed();
        window.set_maximized(rect(1));

        assert!(window.adopt_presentation(Presentation::Normal, None));
        assert!(!window.adopt_presentation(Presentation::Normal, None));
    }

    #[test]
    fn a_floating_window_which_left_a_presentation_keeps_the_rectangle_it_landed_on() {
        let mut window = managed();
        window.set_floating(rect(2));
        window.set_maximized(rect(2));

        assert!(window.adopt_presentation(Presentation::Normal, Some(rect(7))));

        assert_eq!(window.placement, ManagedPlacement::Floating);
        assert_eq!(window.floating_rect, Some(rect(7)));
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
    fn entering_a_presentation_is_idempotent_in_both_directions() {
        let mut window = managed();

        assert!(window.set_maximized(rect(1)));
        assert!(!window.set_maximized(rect(9)));
        assert_eq!(window.restore_rect, Some(rect(1)));

        assert_eq!(window.set_normal(rect(3)), Some(rect(3)));
        assert_eq!(window.set_normal(rect(3)), None);

        assert!(window.set_fullscreen(rect(1)));
        assert!(!window.set_fullscreen(rect(9)));
        assert_eq!(window.restore_rect, Some(rect(1)));
    }

    #[test]
    fn switching_between_presentations_keeps_the_normal_restore_rect() {
        let mut window = managed();
        window.set_maximized(rect(1));

        assert!(window.set_fullscreen(rect(5)));
        assert_eq!(window.presentation, Presentation::Fullscreen);
        assert!(!window.is_maximized());
        assert_eq!(window.restore_rect, Some(rect(1)));

        assert!(window.set_maximized(rect(7)));
        assert_eq!(window.presentation, Presentation::Maximized);
        assert!(!window.is_fullscreen());
        assert_eq!(window.restore_rect, Some(rect(1)));
    }

    #[test]
    fn fullscreen_is_independent_of_minimizing_and_placement() {
        let mut window = managed();
        window.set_fullscreen(rect(1));

        assert!(window.set_minimized());
        assert_eq!(window.presentation, Presentation::Fullscreen);
        assert!(window.set_floating(rect(2)));
        assert_eq!(window.presentation, Presentation::Fullscreen);
        assert!(window.set_visible());
        assert_eq!(window.presentation, Presentation::Fullscreen);
        assert!(window.is_presented());
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
    fn only_a_visible_floating_normal_window_has_floating_geometry() {
        let mut window = managed();
        assert_eq!(
            window.floating_geometry(Some(rect(1))),
            Err(FloatingRejection::NotFloating)
        );

        window.set_floating(rect(2));
        assert_eq!(window.floating_geometry(Some(rect(1))), Ok(rect(1)));

        window.set_maximized(rect(2));
        assert_eq!(
            window.floating_geometry(Some(rect(1))),
            Err(FloatingRejection::Presented(Presentation::Maximized))
        );

        window.set_fullscreen(rect(2));
        assert_eq!(
            window.floating_geometry(Some(rect(1))),
            Err(FloatingRejection::Presented(Presentation::Fullscreen))
        );

        window.set_normal(rect(2));
        window.set_minimized();
        assert_eq!(
            window.floating_geometry(Some(rect(1))),
            Err(FloatingRejection::Minimized)
        );
    }

    #[test]
    fn floating_geometry_prefers_what_win32_reports_over_the_record() {
        let mut window = managed();
        window.set_floating(rect(2));

        // The user can drag a floating window without any command passing through the model, so
        // the recorded rectangle is only used when Win32 could not be asked.
        assert_eq!(window.floating_geometry(Some(rect(9))), Ok(rect(9)));
        assert_eq!(window.floating_geometry(None), Ok(rect(2)));

        window.floating_rect = None;
        assert_eq!(
            window.floating_geometry(None),
            Err(FloatingRejection::UnknownGeometry)
        );
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
        let window: ManagedWindow =
            serde_json::from_str(r#"{"window":{"hwnd":42},"container_id":"container-1"}"#).unwrap();

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
