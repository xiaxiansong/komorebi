use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use crate::container::Container;
use crate::geometry::SlotOrder;
use crate::monitor::Monitor;
use crate::window_manager::WindowManager;
use crate::workspace::Workspace;

/// A model rule which must hold whenever the window manager is at rest.
///
/// The variants are the ownership, lifecycle, focus, history, and slot-ownership rules.
///
/// The slot rules checked here are the ones which hold even when a background workspace has not
/// been retiled since its last structural change: a slot always belongs to a container the
/// workspace still owns, and recorded slots never overlap. Exact coverage of the work area and
/// "a hidden container owns no slot" are properties of a freshly recorded arrangement, so they
/// are validated where the arrangement is recorded rather than at every point of rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invariant {
    /// Every managed window belongs to exactly one container.
    WindowOwnership,
    /// Windows which are not managed belong to no container: an ignored window never enters the
    /// model, and a temporarily unmanaged window has already left it.
    UnmanagedExclusion,
    /// Every container owns at least one managed window.
    NonEmptyContainer,
    /// Every monitor owns at least one workspace.
    NonEmptyMonitor,
    /// A workspace focuses at most one container and a container focuses at most one window.
    FocusSelection,
    /// Removing an object clears every history entry and index which referenced it.
    HistoryIntegrity,
    /// A logical slot belongs to a container the workspace still owns, and no two recorded slots
    /// overlap.
    SlotOwnership,
}

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::WindowOwnership => "window ownership",
            Self::UnmanagedExclusion => "unmanaged exclusion",
            Self::NonEmptyContainer => "non-empty container",
            Self::NonEmptyMonitor => "non-empty monitor",
            Self::FocusSelection => "focus selection",
            Self::HistoryIntegrity => "history integrity",
            Self::SlotOwnership => "slot ownership",
        };

        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantViolation {
    pub invariant: Invariant,
    pub detail: String,
}

impl InvariantViolation {
    fn new(invariant: Invariant, detail: impl Into<String>) -> Self {
        Self {
            invariant,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.invariant, self.detail)
    }
}

pub trait ValidateInvariants {
    /// Report every violated invariant instead of stopping at the first, so a single run
    /// describes the whole inconsistency.
    fn validate_invariants(&self) -> Vec<InvariantViolation>;
}

/// Report violated invariants, and fail the run when this crate's own tests observe one.
///
/// Production builds log instead of panicking: a violation there is a defect to diagnose, not a
/// reason to take a user's desktop down.
pub fn assert_invariants(subject: &impl ValidateInvariants, context: &str) {
    let violations = subject.validate_invariants();

    if violations.is_empty() {
        return;
    }

    for violation in &violations {
        tracing::error!("{context}: invariant violation: {violation}");
    }

    #[cfg(test)]
    panic!(
        "{context}: {} invariant violation(s): {}",
        violations.len(),
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    );
}

impl ValidateInvariants for Container {
    fn validate_invariants(&self) -> Vec<InvariantViolation> {
        let mut violations = Vec::new();
        let id = &self.id;

        // A preselect container is an insertion marker, not a container of the model.
        if self.is_preselect() {
            return violations;
        }

        if self.windows().is_empty() {
            violations.push(InvariantViolation::new(
                Invariant::NonEmptyContainer,
                format!("container {id} owns no window"),
            ));
        } else if self.focused_window_idx() >= self.windows().len() {
            violations.push(InvariantViolation::new(
                Invariant::FocusSelection,
                format!(
                    "container {id} focuses window {} of {}",
                    self.focused_window_idx(),
                    self.windows().len()
                ),
            ));
        }

        let mut seen = HashSet::new();

        for window in self.windows() {
            if &window.container_id != id {
                violations.push(InvariantViolation::new(
                    Invariant::WindowOwnership,
                    format!(
                        "window {} in container {id} is owned by {}",
                        window.hwnd, window.container_id
                    ),
                ));
            }

            if !seen.insert(window.hwnd) {
                violations.push(InvariantViolation::new(
                    Invariant::WindowOwnership,
                    format!("window {} appears twice in container {id}", window.hwnd),
                ));
            }
        }

        let mut recorded = HashSet::new();

        for hwnd in self.focus_history().iter() {
            if !seen.contains(hwnd) {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!("container {id} focus history references unowned window {hwnd}"),
                ));
            }

            if !recorded.insert(*hwnd) {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!("container {id} focus history repeats window {hwnd}"),
                ));
            }
        }

        violations
    }
}

impl ValidateInvariants for Workspace {
    fn validate_invariants(&self) -> Vec<InvariantViolation> {
        let mut violations = Vec::new();
        let id = &self.id;
        let mut owners: HashMap<isize, String> = HashMap::new();

        for container in self.containers() {
            violations.extend(container.validate_invariants());

            for window in container.windows() {
                if let Some(previous) = owners.insert(window.hwnd, container.id.to_string()) {
                    violations.push(InvariantViolation::new(
                        Invariant::WindowOwnership,
                        format!(
                            "window {} is owned by containers {previous} and {}",
                            window.hwnd, container.id
                        ),
                    ));
                }
            }
        }

        if !self.containers().is_empty() && self.focused_container_idx() >= self.containers().len()
        {
            violations.push(InvariantViolation::new(
                Invariant::FocusSelection,
                format!(
                    "workspace {id} focuses container {} of {}",
                    self.focused_container_idx(),
                    self.containers().len()
                ),
            ));
        }

        // There is no alternate ownership left to tolerate. Every managed window of this
        // workspace is owned by a container in the ring, and monocle is a reference into that
        // ring rather than a second place a container can live. A reference which no longer
        // resolves is the remaining defect worth reporting.
        if let Some(monocle_id) = &self.monocle_container_id
            && !self
                .containers()
                .iter()
                .any(|container| &container.id == monocle_id)
        {
            violations.push(InvariantViolation::new(
                Invariant::HistoryIntegrity,
                format!(
                    "workspace {id} shows monocle container {monocle_id}, which it does not own"
                ),
            ));
        }

        let mut ids = HashSet::new();

        for container_id in self.container_focus_history.iter() {
            if !self
                .containers()
                .iter()
                .any(|container| &container.id == container_id)
            {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!(
                        "workspace {id} focus history references absent container {container_id}"
                    ),
                ));
            }

            if !ids.insert(container_id.clone()) {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!("workspace {id} focus history repeats container {container_id}"),
                ));
            }
        }

        let mut minimized = HashSet::new();

        for hwnd in self.minimize_history.iter() {
            if !owners.contains_key(hwnd) {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!("workspace {id} minimize history references unowned window {hwnd}"),
                ));
            }

            if !minimized.insert(*hwnd) {
                violations.push(InvariantViolation::new(
                    Invariant::HistoryIntegrity,
                    format!("workspace {id} minimize history repeats window {hwnd}"),
                ));
            }
        }

        violations.extend(self.validate_slot_ownership());

        violations
    }
}

impl Workspace {
    /// The slot rules which hold at every point of rest, not only right after a recalculation.
    fn validate_slot_ownership(&self) -> Vec<InvariantViolation> {
        let mut violations = Vec::new();
        let id = &self.id;
        let slots = self.logical_slots.ordered(SlotOrder::TopToBottom);

        for (container_id, _) in &slots {
            if !self
                .containers()
                .iter()
                .any(|container| &container.id == container_id)
            {
                violations.push(InvariantViolation::new(
                    Invariant::SlotOwnership,
                    format!("workspace {id} holds a slot for absent container {container_id}"),
                ));
            }
        }

        for (index, (container_id, slot)) in slots.iter().enumerate() {
            for (other_id, other) in slots.iter().skip(index + 1) {
                if slot.overlaps(*other) {
                    violations.push(InvariantViolation::new(
                        Invariant::SlotOwnership,
                        format!(
                            "workspace {id} slots for containers {container_id} and {other_id} overlap: {slot} and {other}"
                        ),
                    ));
                }
            }
        }

        violations
    }
}

impl ValidateInvariants for Monitor {
    fn validate_invariants(&self) -> Vec<InvariantViolation> {
        let mut violations = Vec::new();
        let mut owners: HashMap<isize, usize> = HashMap::new();

        if self.workspaces().is_empty() {
            violations.push(InvariantViolation::new(
                Invariant::NonEmptyMonitor,
                format!("monitor {} owns no workspace", self.device_id),
            ));
        }

        for (idx, workspace) in self.workspaces().iter().enumerate() {
            violations.extend(workspace.validate_invariants());

            for container in workspace.containers() {
                for window in container.windows() {
                    if let Some(previous) = owners.insert(window.hwnd, idx) {
                        violations.push(InvariantViolation::new(
                            Invariant::WindowOwnership,
                            format!(
                                "window {} is managed by workspaces {previous} and {idx} of monitor {}",
                                window.hwnd, self.device_id
                            ),
                        ));
                    }
                }
            }
        }

        violations
    }
}

impl ValidateInvariants for WindowManager {
    fn validate_invariants(&self) -> Vec<InvariantViolation> {
        let mut violations = Vec::new();
        let mut owners: HashMap<isize, (usize, usize)> = HashMap::new();

        for (monitor_idx, monitor) in self.monitors().iter().enumerate() {
            violations.extend(monitor.validate_invariants());

            for (workspace_idx, workspace) in monitor.workspaces().iter().enumerate() {
                for container in workspace.containers() {
                    for window in container.windows() {
                        if let Some(previous) =
                            owners.insert(window.hwnd, (monitor_idx, workspace_idx))
                        {
                            violations.push(InvariantViolation::new(
                                Invariant::WindowOwnership,
                                format!(
                                    "window {} is managed by {previous:?} and by ({monitor_idx}, {workspace_idx})",
                                    window.hwnd
                                ),
                            ));
                        }

                        if self.temporarily_unmanaged_hwnds.contains(&window.hwnd) {
                            violations.push(InvariantViolation::new(
                                Invariant::UnmanagedExclusion,
                                format!(
                                    "temporarily unmanaged window {} is owned by container {}",
                                    window.hwnd, container.id
                                ),
                            ));
                        }
                    }
                }
            }
        }

        violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Window;
    use crate::core::Rect;
    use crate::managed_window::ManagedWindow;
    use crate::model::ContainerId;

    fn container_with(hwnds: &[isize]) -> Container {
        let mut container = Container::default();

        for hwnd in hwnds {
            container.add_window(Window::from(*hwnd));
        }

        container
    }

    fn workspace_with(containers: Vec<Container>) -> Workspace {
        let mut workspace = Workspace::default();

        for container in containers {
            workspace.add_container_to_back(container);
        }

        workspace
    }

    fn invariants(violations: &[InvariantViolation]) -> Vec<Invariant> {
        violations
            .iter()
            .map(|violation| violation.invariant)
            .collect()
    }

    #[test]
    fn a_consistent_workspace_reports_nothing() {
        let mut workspace = workspace_with(vec![container_with(&[1, 2]), container_with(&[3])]);
        workspace.record_focused_window(1);
        workspace.record_minimized_window(2);

        assert_eq!(workspace.validate_invariants(), vec![]);
    }

    #[test]
    fn an_empty_container_is_reported() {
        let workspace = workspace_with(vec![Container::default()]);

        assert_eq!(
            invariants(&workspace.validate_invariants()),
            vec![Invariant::NonEmptyContainer]
        );
    }

    #[test]
    fn a_preselect_marker_is_not_treated_as_an_empty_container() {
        let mut workspace = workspace_with(vec![container_with(&[1])]);
        workspace.preselect_container_idx(0);

        assert_eq!(workspace.validate_invariants(), vec![]);
    }

    #[test]
    fn a_window_owned_by_another_container_is_reported() {
        let mut container = container_with(&[1]);
        container.windows_mut()[0].container_id = "elsewhere".into();

        assert_eq!(
            invariants(&container.validate_invariants()),
            vec![Invariant::WindowOwnership]
        );
    }

    #[test]
    fn a_window_in_two_containers_is_reported() {
        let workspace = workspace_with(vec![container_with(&[1]), container_with(&[1])]);

        assert_eq!(
            invariants(&workspace.validate_invariants()),
            vec![Invariant::WindowOwnership]
        );
    }

    #[test]
    fn a_monocle_reference_to_a_container_the_workspace_lost_is_reported() {
        let mut workspace = workspace_with(vec![container_with(&[1])]);
        workspace.monocle_container_id = Some(ContainerId::from("gone"));

        assert_eq!(
            invariants(&workspace.validate_invariants()),
            vec![Invariant::HistoryIntegrity]
        );
    }

    #[test]
    fn a_monocle_container_is_not_alternate_ownership() {
        let mut workspace = workspace_with(vec![container_with(&[1]), container_with(&[2])]);
        workspace.new_monocle_container().unwrap();

        assert!(workspace.is_monocle());
        assert_eq!(workspace.containers().len(), 2);
        assert!(workspace.validate_invariants().is_empty());
    }

    #[test]
    fn a_floating_window_in_its_own_container_is_not_a_violation() {
        let mut workspace = workspace_with(vec![container_with(&[1])]);
        workspace.float_window(1, Rect::default()).unwrap();

        assert!(workspace.is_floating_window(1));
        assert!(workspace.validate_invariants().is_empty());
    }

    #[test]
    fn a_container_focused_out_of_range_is_reported() {
        let mut container = container_with(&[1]);
        container.focus_window(4);

        assert_eq!(
            invariants(&container.validate_invariants()),
            vec![Invariant::FocusSelection]
        );
    }

    #[test]
    fn history_entries_for_removed_objects_are_reported() {
        let mut workspace = workspace_with(vec![container_with(&[1])]);
        workspace.container_focus_history.record("gone".into());
        workspace.minimize_history.record(404);

        assert_eq!(
            invariants(&workspace.validate_invariants()),
            vec![Invariant::HistoryIntegrity, Invariant::HistoryIntegrity]
        );

        workspace.prune_histories();

        assert_eq!(workspace.validate_invariants(), vec![]);
    }

    #[test]
    fn a_container_history_entry_for_an_unowned_window_is_reported() {
        let mut container = container_with(&[1]);
        let window = ManagedWindow::capture(Window::from(2), container.id.clone());
        container.windows_mut().push_back(window);
        container.focus_window(1);
        container.windows_mut().remove(1);
        // Raw ring removal leaves the history behind; focus is moved back so only the history
        // entry is under test.
        container.focus_window(0);

        assert_eq!(
            invariants(&container.validate_invariants()),
            vec![Invariant::HistoryIntegrity]
        );
    }

    #[test]
    fn removal_paths_leave_a_workspace_consistent() {
        let mut workspace = workspace_with(vec![container_with(&[1, 2]), container_with(&[3])]);
        workspace.record_focused_window(3);
        workspace.record_minimized_window(2);

        // detach_window is the removal path which does not talk to Win32 about focus, so this
        // test exercises history cleanup without depending on live window handles.
        workspace.detach_window(3).unwrap();
        workspace.detach_window(2).unwrap();

        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.validate_invariants(), vec![]);
    }

    #[test]
    fn a_monitor_without_a_workspace_is_reported() {
        let mut monitor = Monitor::placeholder();
        monitor.workspaces_mut().clear();

        assert_eq!(
            invariants(&monitor.validate_invariants()),
            vec![Invariant::NonEmptyMonitor]
        );
    }
}
