use std::collections::HashMap;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;

use color_eyre::eyre;
use color_eyre::eyre::OptionExt;
use color_eyre::eyre::bail;
use serde::Deserialize;
use serde::Serialize;

use crate::border_manager::BORDER_ENABLED;
use crate::border_manager::BORDER_OFFSET;
use crate::border_manager::BORDER_WIDTH;
use crate::core::Rect;

use crate::CycleDirection;
use crate::DEFAULT_CONTAINER_PADDING;
use crate::DEFAULT_WORKSPACE_PADDING;
use crate::DefaultLayout;
use crate::FloatingLayerBehaviour;
use crate::Layout;
use crate::OperationDirection;
use crate::Wallpaper;
use crate::WindowsApi;
use crate::container::Container;
use crate::model::WorkspaceId;
use crate::ring::Ring;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceGlobals;
use crate::workspace::WorkspaceLayer;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Monitor {
    pub id: isize,
    pub name: String,
    pub device: String,
    pub device_id: String,
    pub serial_number_id: Option<String>,
    pub size: Rect,
    pub work_area_size: Rect,
    pub work_area_offset: Option<Rect>,
    pub window_based_work_area_offset: Option<Rect>,
    pub window_based_work_area_offset_limit: isize,
    pub workspaces: Ring<Workspace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_focused_workspace: Option<usize>,
    pub workspace_names: HashMap<usize, String>,
    pub container_padding: Option<i32>,
    pub workspace_padding: Option<i32>,
    pub wallpaper: Option<Wallpaper>,
    pub floating_layer_behaviour: Option<FloatingLayerBehaviour>,
}

impl_ring_elements!(Monitor, Workspace);

/// A rearrangement of one monitor's workspace list, as old index to new index.
///
/// A workspace's identity is its `WorkspaceId`, but several tables describe workspaces by
/// position: the monitor's configured names, its focused and last-focused indices, and the global
/// application routing rules. Reordering has to move all of them, and they do not all belong to
/// the monitor, so the move is reported instead of being applied in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceReorder {
    /// The new index of the workspace which was at each old index, or `None` for a workspace
    /// which no longer exists.
    positions: Vec<Option<usize>>,
    /// The old index of a workspace which was merged away, and the new index of the workspace
    /// which took its contents.
    merged: Option<(usize, usize)>,
}

impl WorkspaceReorder {
    /// The rearrangement produced by taking the workspace at `from` out of a list of `len`
    /// workspaces and putting it back at `to`.
    #[must_use]
    fn from_move(len: usize, from: usize, to: usize) -> Self {
        let positions = (0..len)
            .map(|idx| {
                Some(if idx == from {
                    to
                } else if from < to && idx > from && idx <= to {
                    idx - 1
                } else if from > to && idx >= to && idx < from {
                    idx + 1
                } else {
                    idx
                })
            })
            .collect();

        Self {
            positions,
            merged: None,
        }
    }

    /// The rearrangement produced by exchanging two workspaces and moving nothing else.
    #[must_use]
    fn from_swap(len: usize, i: usize, j: usize) -> Self {
        let positions = (0..len)
            .map(|idx| {
                Some(if idx == i {
                    j
                } else if idx == j {
                    i
                } else {
                    idx
                })
            })
            .collect();

        Self {
            positions,
            merged: None,
        }
    }

    /// The rearrangement produced by removing the workspace at `removed` from a list of `len`
    /// workspaces, its contents having gone to the workspace which now sits at `target`.
    #[must_use]
    fn from_merge(len: usize, removed: usize, target: usize) -> Self {
        let positions = (0..len)
            .map(|idx| {
                if idx == removed {
                    None
                } else if idx < removed {
                    Some(idx)
                } else {
                    Some(idx - 1)
                }
            })
            .collect();

        Self {
            positions,
            merged: Some((removed, target)),
        }
    }

    /// The index the workspace which was at `old` now occupies, or `None` if it is gone.
    ///
    /// This is what a table describing the workspace itself follows - its configured name, or the
    /// fact that it was focused - because none of that survives the workspace.
    #[must_use]
    pub fn new_idx(&self, old: usize) -> Option<usize> {
        self.positions.get(old).copied().flatten()
    }

    /// The index of the workspace which now holds the contents of the workspace at `old`.
    ///
    /// This is what a table describing a workspace's *windows* follows - the application routing
    /// rules - because a merged workspace's windows are still on the monitor, in the workspace
    /// which absorbed them.
    #[must_use]
    pub fn content_idx(&self, old: usize) -> Option<usize> {
        match self.merged {
            Some((removed, target)) if old == removed => Some(target),
            _ => self.new_idx(old),
        }
    }

    /// The index of the workspace which absorbed a merged workspace, if this was a merge.
    #[must_use]
    pub fn merged_into(&self) -> Option<usize> {
        self.merged.map(|(_, target)| target)
    }

    /// Whether every workspace stayed where it was.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.merged.is_none()
            && self
                .positions
                .iter()
                .enumerate()
                .all(|(old, new)| Some(old) == *new)
    }

    /// Rebuild an index-keyed table so each entry stays with the workspace it described.
    ///
    /// Entries whose index is outside the list are dropped rather than kept at a position they no
    /// longer describe: a table which has fallen behind the workspace list is not made more
    /// correct by moving part of it.
    #[must_use]
    pub fn remap_keys<T: Clone>(&self, table: &HashMap<usize, T>) -> HashMap<usize, T> {
        table
            .iter()
            .filter_map(|(idx, value)| Some((self.new_idx(*idx)?, value.clone())))
            .collect()
    }
}

#[derive(Serialize)]
pub struct MonitorInformation {
    pub id: isize,
    pub name: String,
    pub device: String,
    pub device_id: String,
    pub serial_number_id: Option<String>,
    pub size: Rect,
}

impl From<&Monitor> for MonitorInformation {
    fn from(monitor: &Monitor) -> Self {
        Self {
            id: monitor.id,
            name: monitor.name.clone(),
            device: monitor.device.clone(),
            device_id: monitor.device_id.clone(),
            serial_number_id: monitor.serial_number_id.clone(),
            size: monitor.size,
        }
    }
}

pub fn new(
    id: isize,
    size: Rect,
    work_area_size: Rect,
    name: String,
    device: String,
    device_id: String,
    serial_number_id: Option<String>,
) -> Monitor {
    let mut workspaces = Ring::default();
    workspaces.elements_mut().push_back(Workspace::default());

    Monitor {
        id,
        name,
        device,
        device_id,
        serial_number_id,
        size,
        work_area_size,
        work_area_offset: None,
        window_based_work_area_offset: None,
        window_based_work_area_offset_limit: 1,
        workspaces,
        last_focused_workspace: None,
        workspace_names: HashMap::default(),
        container_padding: None,
        workspace_padding: None,
        wallpaper: None,
        floating_layer_behaviour: None,
    }
}

impl Monitor {
    pub fn new(
        id: isize,
        size: Rect,
        work_area_size: Rect,
        name: String,
        device: String,
        device_id: String,
        serial_number_id: Option<String>,
    ) -> Self {
        new(
            id,
            size,
            work_area_size,
            name,
            device,
            device_id,
            serial_number_id,
        )
    }

    pub fn placeholder() -> Self {
        Self {
            id: 0,
            name: "PLACEHOLDER".to_string(),
            device: "".to_string(),
            device_id: "".to_string(),
            serial_number_id: None,
            size: Default::default(),
            work_area_size: Default::default(),
            work_area_offset: None,
            window_based_work_area_offset: None,
            window_based_work_area_offset_limit: 0,
            workspaces: Default::default(),
            last_focused_workspace: None,
            workspace_names: Default::default(),
            container_padding: None,
            workspace_padding: None,
            wallpaper: None,
            floating_layer_behaviour: None,
        }
    }

    pub fn focused_workspace_name(&self) -> Option<String> {
        self.focused_workspace()
            .map(|w| w.name.clone())
            .unwrap_or(None)
    }

    pub fn focused_workspace_layout(&self) -> Option<Layout> {
        self.focused_workspace().and_then(|workspace| {
            if workspace.tile {
                Some(workspace.layout.clone())
            } else {
                None
            }
        })
    }

    pub fn load_focused_workspace(&mut self, mouse_follows_focus: bool) -> eyre::Result<()> {
        let focused_idx = self.focused_workspace_idx();
        let hmonitor = self.id;
        let monitor_wp = self.wallpaper.clone();
        for (i, workspace) in self.workspaces_mut().iter_mut().enumerate() {
            if i == focused_idx {
                workspace.restore(mouse_follows_focus, hmonitor, &monitor_wp)?;
            } else {
                workspace.hide(None);
            }
        }

        Ok(())
    }

    /// Updates the `globals` field of all workspaces
    pub fn update_workspaces_globals(&mut self, offset: Option<Rect>) {
        let container_padding = self
            .container_padding
            .or(Some(DEFAULT_CONTAINER_PADDING.load(Ordering::SeqCst)));
        let workspace_padding = self
            .workspace_padding
            .or(Some(DEFAULT_WORKSPACE_PADDING.load(Ordering::SeqCst)));
        let (border_width, border_offset) = {
            let border_enabled = BORDER_ENABLED.load(Ordering::SeqCst);
            if border_enabled {
                let border_width = BORDER_WIDTH.load(Ordering::SeqCst);
                let border_offset = BORDER_OFFSET.load(Ordering::SeqCst);
                (border_width, border_offset)
            } else {
                (0, 0)
            }
        };
        let work_area = self.work_area_size;
        let monitor_size = self.size;
        let work_area_offset = self.work_area_offset.or(offset);
        let window_based_work_area_offset = self.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.window_based_work_area_offset_limit;
        let floating_layer_behaviour = self.floating_layer_behaviour;

        for workspace in self.workspaces_mut() {
            workspace.globals = WorkspaceGlobals {
                container_padding,
                workspace_padding,
                border_width,
                border_offset,
                work_area,
                monitor_size,
                work_area_offset,
                window_based_work_area_offset,
                window_based_work_area_offset_limit,
                floating_layer_behaviour,
            }
        }
    }

    /// Updates the `globals` field of workspace with index `workspace_idx`
    pub fn update_workspace_globals(&mut self, workspace_idx: usize, offset: Option<Rect>) {
        let container_padding = self
            .container_padding
            .or(Some(DEFAULT_CONTAINER_PADDING.load(Ordering::SeqCst)));
        let workspace_padding = self
            .workspace_padding
            .or(Some(DEFAULT_WORKSPACE_PADDING.load(Ordering::SeqCst)));
        let (border_width, border_offset) = {
            let border_enabled = BORDER_ENABLED.load(Ordering::SeqCst);
            if border_enabled {
                let border_width = BORDER_WIDTH.load(Ordering::SeqCst);
                let border_offset = BORDER_OFFSET.load(Ordering::SeqCst);
                (border_width, border_offset)
            } else {
                (0, 0)
            }
        };
        let work_area = self.work_area_size;
        let monitor_size = self.size;
        let work_area_offset = self.work_area_offset.or(offset);
        let window_based_work_area_offset = self.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.window_based_work_area_offset_limit;
        let floating_layer_behaviour = self.floating_layer_behaviour;

        if let Some(workspace) = self.workspaces_mut().get_mut(workspace_idx) {
            workspace.globals = WorkspaceGlobals {
                container_padding,
                workspace_padding,
                border_width,
                border_offset,
                work_area,
                monitor_size,
                work_area_offset,
                window_based_work_area_offset,
                window_based_work_area_offset_limit,
                floating_layer_behaviour,
            }
        }
    }

    pub fn add_container(
        &mut self,
        container: Container,
        workspace_idx: Option<usize>,
    ) -> eyre::Result<()> {
        let workspace = if let Some(idx) = workspace_idx {
            self.workspaces_mut()
                .get_mut(idx)
                .ok_or_eyre(format!("there is no workspace at index {idx}"))?
        } else {
            self.focused_workspace_mut()
                .ok_or_eyre("there is no workspace")?
        };

        workspace.add_container_to_back(container);

        Ok(())
    }

    /// Adds a container to this `Monitor` using the move direction to calculate if the container
    /// should be added in front of all containers, in the back or in place of the focused
    /// container, moving the rest along. The move direction should be from the origin monitor
    /// towards the target monitor or from the origin workspace towards the target workspace.
    pub fn add_container_with_direction(
        &mut self,
        container: Container,
        workspace_idx: Option<usize>,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let workspace = if let Some(idx) = workspace_idx {
            self.workspaces_mut()
                .get_mut(idx)
                .ok_or_eyre(format!("there is no workspace at index {idx}"))?
        } else {
            self.focused_workspace_mut()
                .ok_or_eyre("there is no workspace")?
        };

        match direction {
            OperationDirection::Left => {
                // insert the container into the workspace on the monitor at the back (or rightmost position)
                // if we are moving across a boundary to the left (back = right side of the target)
                match workspace.layout {
                    Layout::Default(layout) => match layout {
                        DefaultLayout::RightMainVerticalStack => {
                            workspace.add_container_to_front(container);
                        }
                        DefaultLayout::UltrawideVerticalStack
                            if workspace.containers().len() == 1 =>
                        {
                            workspace.insert_container_at_idx(0, container);
                        }
                        _ => {
                            workspace.add_container_to_back(container);
                        }
                    },
                    Layout::Custom(_) => {
                        workspace.add_container_to_back(container);
                    }
                }
            }
            OperationDirection::Right => {
                // insert the container into the workspace on the monitor at the front (or leftmost position)
                // if we are moving across a boundary to the right (front = left side of the target)
                match workspace.layout {
                    Layout::Default(layout) => {
                        let target_index = layout.leftmost_index(workspace.containers().len());

                        match layout {
                            DefaultLayout::RightMainVerticalStack
                            | DefaultLayout::UltrawideVerticalStack
                                if workspace.containers().len() == 1 =>
                            {
                                workspace.add_container_to_back(container);
                            }
                            _ => {
                                workspace.insert_container_at_idx(target_index, container);
                            }
                        }
                    }
                    Layout::Custom(_) => {
                        workspace.add_container_to_front(container);
                    }
                }
            }
            OperationDirection::Up | OperationDirection::Down => {
                // insert the container into the workspace on the monitor at the position
                // where the currently focused container on that workspace is
                workspace.insert_container_at_idx(workspace.focused_container_idx(), container);
            }
        };

        Ok(())
    }

    pub fn remove_workspace_by_idx(&mut self, idx: usize) -> Option<Workspace> {
        if idx < self.workspaces().len() {
            return self.workspaces_mut().remove(idx);
        }

        if idx == 0 {
            self.workspaces_mut().push_back(Workspace::default());
        } else {
            self.focus_workspace(idx.saturating_sub(1)).ok()?;
        };

        None
    }

    pub fn ensure_workspace_count(&mut self, ensure_count: usize) {
        if self.workspaces().len() < ensure_count {
            self.workspaces_mut()
                .resize(ensure_count, Workspace::default());
        }
    }

    pub fn remove_workspaces(&mut self) -> VecDeque<Workspace> {
        std::mem::take(self.workspaces_mut())
    }

    /// The index of the workspace with `id`, if this monitor owns it.
    ///
    /// The counterpart of `Workspace::container_idx_for_id`: a workspace's position is an ordering
    /// decision the user can change at any time, so anything which has to name a particular
    /// workspace across such a change names it by identity and resolves the index here.
    #[must_use]
    pub fn workspace_idx_for_id(&self, id: &WorkspaceId) -> Option<usize> {
        self.workspaces()
            .iter()
            .position(|workspace| workspace.id == *id)
    }

    #[must_use]
    pub fn workspace_for_id(&self, id: &WorkspaceId) -> Option<&Workspace> {
        self.workspaces().get(self.workspace_idx_for_id(id)?)
    }

    pub fn workspace_for_id_mut(&mut self, id: &WorkspaceId) -> Option<&mut Workspace> {
        let idx = self.workspace_idx_for_id(id)?;
        self.workspaces_mut().get_mut(idx)
    }

    /// Move the workspace at `from` so that it sits at `to`, shifting the workspaces in between.
    ///
    /// Ordering is a presentation decision: no workspace's containers, windows, slots, histories,
    /// ID or name change, so nothing here invalidates any geometry. Focus follows the workspace
    /// which had it, by identity rather than by position.
    ///
    /// Returns the permutation it performed so the caller can move the index-keyed tables which
    /// describe workspaces by position.
    pub fn reorder_workspace(&mut self, from: usize, to: usize) -> eyre::Result<WorkspaceReorder> {
        let len = self.workspaces().len();

        if from >= len || to >= len {
            bail!("this monitor has no workspace at index {from} or {to}");
        }

        let reorder = WorkspaceReorder::from_move(len, from, to);

        // Nothing above this point has written anything, and nothing below it can fail.
        if from != to {
            let workspace = self
                .workspaces_mut()
                .remove(from)
                .expect("the workspace index was checked against the length");

            self.workspaces_mut().insert(to, workspace);
        }

        self.apply_workspace_reorder(&reorder);

        Ok(reorder)
    }

    /// Exchange the positions of two workspaces, leaving every other workspace where it is.
    ///
    /// This is not two moves: moving A onto B's index and B onto A's would shift everything
    /// between them twice.
    pub fn swap_workspaces(&mut self, i: usize, j: usize) -> eyre::Result<WorkspaceReorder> {
        let len = self.workspaces().len();

        if i >= len || j >= len {
            bail!("this monitor has no workspace at index {i} or {j}");
        }

        let reorder = WorkspaceReorder::from_swap(len, i, j);

        // Nothing above this point has written anything, and nothing below it can fail.
        self.workspaces_mut().swap(i, j);
        self.apply_workspace_reorder(&reorder);

        Ok(reorder)
    }

    /// Move the workspace at `from` one position in `direction`, wrapping around the list.
    ///
    /// Wrapping keeps the operation total: every workspace can always be moved either way, and the
    /// first workspace moved left becomes the last.
    pub fn cycle_workspace_position(
        &mut self,
        from: usize,
        direction: CycleDirection,
    ) -> eyre::Result<WorkspaceReorder> {
        let len = NonZeroUsize::new(self.workspaces().len())
            .ok_or_eyre("this monitor has no workspaces")?;

        if from >= len.get() {
            bail!("this monitor has no workspace at index {from}");
        }

        self.reorder_workspace(from, direction.next_idx(from, len))
    }

    /// The workspace which would take the contents of the workspace at `idx` if it were deleted.
    ///
    /// A monitor must always have at least one workspace, so the only workspace has no target and
    /// cannot be deleted. Every other workspace merges into its left neighbour, except the first,
    /// which has none and merges into its right.
    #[must_use]
    pub fn merge_target_idx(&self, idx: usize) -> Option<usize> {
        let len = self.workspaces().len();

        if idx >= len || len < 2 {
            return None;
        }

        Some(if idx == 0 { 1 } else { idx - 1 })
    }

    /// Delete the workspace at `idx`, merging everything it owned into a neighbour.
    ///
    /// The neighbour becomes this monitor's focused workspace, because after a delete the user is
    /// looking at the workspace their windows just moved to. The returned rearrangement names it:
    /// its index is not `merge_target_idx`'s answer once the deleted workspace has left the list.
    ///
    /// Refuses a monitor's only workspace without changing anything.
    pub fn merge_workspace(&mut self, idx: usize) -> eyre::Result<WorkspaceReorder> {
        let target = self
            .merge_target_idx(idx)
            .ok_or_eyre("a monitor cannot delete its only workspace")?;

        // Nothing above this point has written anything, and nothing below it can fail.
        let len = self.workspaces().len();
        let target_idx = if target > idx { target - 1 } else { target };
        let reorder = WorkspaceReorder::from_merge(len, idx, target_idx);

        let source = self
            .workspaces_mut()
            .remove(idx)
            .expect("the workspace index was checked against the length");

        self.workspaces_mut()[target_idx].merge_from(source);
        self.apply_workspace_reorder(&reorder);
        self.workspaces.focus(target_idx);

        Ok(reorder)
    }

    /// Move every index-keyed description of a workspace to where its workspace went.
    ///
    /// The monitor's own tables are the configured names and the last-focused index; the focused
    /// index is not a table but is index-keyed in exactly the same way. The global application
    /// routing rules are keyed by monitor index as well, so they are remapped by the window
    /// manager, which is what knows this monitor's index.
    fn apply_workspace_reorder(&mut self, reorder: &WorkspaceReorder) {
        self.workspace_names = reorder.remap_keys(&self.workspace_names);

        if let Some(idx) = reorder.content_idx(self.workspaces.focused_idx()) {
            self.workspaces.focus(idx);
        }

        self.last_focused_workspace = self
            .last_focused_workspace
            .and_then(|idx| reorder.new_idx(idx));
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_to_workspace(
        &mut self,
        target_workspace_idx: usize,
        follow: bool,
        direction: Option<OperationDirection>,
    ) -> eyre::Result<()> {
        let workspace = self
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?;

        if workspace.focused_container_has_presented_window() {
            bail!("cannot move a maximized or fullscreen window to another monitor or workspace");
        }

        // The foreground window is only used to decide whether a floating window is being moved.
        // A session without a foreground window means there is no such floating window; it must
        // not abort a container move that does not depend on the query at all.
        let floating_window_index =
            WindowsApi::foreground_window()
                .ok()
                .and_then(|foreground_hwnd| {
                    workspace
                        .is_floating_window(foreground_hwnd)
                        .then_some(foreground_hwnd)
                });

        if let Some(hwnd) = floating_window_index {
            // The window keeps its floating placement and rectangle across the move; only the
            // container which owns it changes.
            if let Ok(window) = workspace.take_window(hwnd) {
                let workspaces = self.workspaces_mut();
                #[allow(clippy::option_if_let_else)]
                let target_workspace = match workspaces.get_mut(target_workspace_idx) {
                    None => {
                        workspaces.resize(target_workspace_idx + 1, Workspace::default());
                        workspaces.get_mut(target_workspace_idx).unwrap()
                    }
                    Some(workspace) => workspace,
                };

                target_workspace.adopt_managed_window(window);
                target_workspace.layer = WorkspaceLayer::Floating;
            }
        } else {
            let container = workspace
                .remove_focused_container()
                .ok_or_eyre("there is no container")?;

            let workspaces = self.workspaces_mut();

            #[allow(clippy::option_if_let_else)]
            let target_workspace = match workspaces.get_mut(target_workspace_idx) {
                None => {
                    workspaces.resize(target_workspace_idx + 1, Workspace::default());
                    workspaces.get_mut(target_workspace_idx).unwrap()
                }
                Some(workspace) => workspace,
            };

            if target_workspace.monocle_container().is_some() {
                for container in target_workspace.containers_mut() {
                    container.restore();
                }

                target_workspace.reintegrate_monocle_container()?;
            }

            target_workspace.layer = WorkspaceLayer::Tiling;

            if let Some(direction) = direction {
                self.add_container_with_direction(
                    container,
                    Some(target_workspace_idx),
                    direction,
                )?;
            } else {
                target_workspace.add_container_to_back(container);
            }
        }

        if follow {
            self.focus_workspace(target_workspace_idx)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_workspace(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("focusing workspace");

        {
            let workspaces = self.workspaces_mut();

            if workspaces.get(idx).is_none() {
                workspaces.resize(idx + 1, Workspace::default());
            }
            self.last_focused_workspace = Some(self.workspaces.focused_idx());
            self.workspaces.focus(idx);
        }

        // Always set the latest known name when creating the workspace for the first time
        {
            let name = { self.workspace_names.get(&idx).cloned() };
            if name.is_some() {
                self.workspaces_mut()
                    .get_mut(idx)
                    .ok_or_eyre("there is no workspace")?
                    .name = name;
            }
        }

        Ok(())
    }

    pub fn new_workspace_idx(&self) -> usize {
        self.workspaces().len()
    }

    pub fn update_focused_workspace(&mut self, offset: Option<Rect>) -> eyre::Result<()> {
        let offset = if self.work_area_offset.is_some() {
            self.work_area_offset
        } else {
            offset
        };

        let focused_workspace_idx = self.focused_workspace_idx();
        self.update_workspace_globals(focused_workspace_idx, offset);
        self.focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?
            .update()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Window;
    use crate::model::ContainerId;

    /// A monitor with `count` workspaces, focused back on the first.
    fn monitor_with_workspaces(count: usize) -> Monitor {
        let mut monitor = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        for idx in 0..count {
            monitor.focus_workspace(idx).unwrap();
        }

        monitor.focus_workspace(0).unwrap();
        monitor.last_focused_workspace = None;

        monitor
    }

    fn workspace_ids(monitor: &Monitor) -> Vec<WorkspaceId> {
        monitor
            .workspaces()
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect()
    }

    /// Give the workspace at `idx` a container holding one window, so a merge has something to
    /// carry and the workspace can be told apart afterwards.
    fn populate_workspace(monitor: &mut Monitor, idx: usize, hwnd: isize) -> ContainerId {
        let mut container = Container::default();
        container.add_window(Window::from(hwnd));
        let id = container.id.clone();

        monitor.workspaces_mut()[idx].add_container_to_back(container);

        id
    }

    #[test]
    fn a_middle_workspace_merges_into_its_left_neighbour() {
        let mut monitor = monitor_with_workspaces(3);
        let left = populate_workspace(&mut monitor, 0, 1);
        let deleted = populate_workspace(&mut monitor, 1, 2);
        let right = populate_workspace(&mut monitor, 2, 3);
        let survivor = monitor.workspaces()[0].id.clone();

        assert_eq!(monitor.merge_target_idx(1), Some(0));
        assert_eq!(monitor.merge_workspace(1).unwrap().merged_into(), Some(0));

        assert_eq!(monitor.workspaces().len(), 2);
        assert_eq!(monitor.workspaces()[0].id, survivor);
        assert_eq!(
            monitor.workspaces()[0]
                .containers()
                .iter()
                .map(|container| container.id.clone())
                .collect::<Vec<_>>(),
            vec![left, deleted]
        );
        assert_eq!(monitor.workspaces()[1].containers()[0].id, right);
        assert_eq!(monitor.focused_workspace_idx(), 0);
    }

    #[test]
    fn the_first_workspace_merges_into_its_right_neighbour() {
        let mut monitor = monitor_with_workspaces(3);
        let deleted = populate_workspace(&mut monitor, 0, 1);
        let right = populate_workspace(&mut monitor, 1, 2);
        let survivor = monitor.workspaces()[1].id.clone();

        assert_eq!(monitor.merge_target_idx(0), Some(1));
        assert_eq!(monitor.merge_workspace(0).unwrap().merged_into(), Some(0));

        assert_eq!(monitor.workspaces()[0].id, survivor);
        assert_eq!(
            monitor.workspaces()[0]
                .containers()
                .iter()
                .map(|container| container.id.clone())
                .collect::<Vec<_>>(),
            vec![right, deleted]
        );
        assert_eq!(monitor.focused_workspace_idx(), 0);
    }

    #[test]
    fn the_last_workspace_merges_into_its_left_neighbour() {
        let mut monitor = monitor_with_workspaces(3);
        populate_workspace(&mut monitor, 2, 1);
        let survivor = monitor.workspaces()[1].id.clone();

        assert_eq!(monitor.merge_workspace(2).unwrap().merged_into(), Some(1));

        assert_eq!(monitor.workspaces().len(), 2);
        assert_eq!(monitor.workspaces()[1].id, survivor);
        assert_eq!(monitor.workspaces()[1].containers().len(), 1);
        assert_eq!(monitor.focused_workspace_idx(), 1);
    }

    #[test]
    fn a_monitor_refuses_to_delete_its_only_workspace() {
        let mut monitor = monitor_with_workspaces(1);
        let id = monitor.workspaces()[0].id.clone();
        populate_workspace(&mut monitor, 0, 1);

        assert_eq!(monitor.merge_target_idx(0), None);
        assert!(monitor.merge_workspace(0).is_err());

        assert_eq!(monitor.workspaces().len(), 1);
        assert_eq!(monitor.workspaces()[0].id, id);
        assert_eq!(monitor.workspaces()[0].containers().len(), 1);
    }

    #[test]
    fn merging_drops_the_deleted_name_and_moves_the_ones_which_survive() {
        let mut monitor = monitor_with_workspaces(3);
        monitor.workspace_names.insert(0, "first".to_string());
        monitor.workspace_names.insert(1, "deleted".to_string());
        monitor.workspace_names.insert(2, "third".to_string());

        monitor.merge_workspace(1).unwrap();

        assert_eq!(monitor.workspace_names.get(&0), Some(&"first".to_string()));
        assert_eq!(monitor.workspace_names.get(&1), Some(&"third".to_string()));
        assert_eq!(monitor.workspace_names.len(), 2);
    }

    #[test]
    fn a_merge_sends_the_windows_of_the_deleted_workspace_to_the_survivor() {
        let reorder = WorkspaceReorder::from_merge(3, 1, 0);

        // The workspace itself is gone ...
        assert_eq!(reorder.new_idx(1), None);
        // ... but its windows are on the workspace which absorbed them.
        assert_eq!(reorder.content_idx(1), Some(0));
        assert_eq!(reorder.content_idx(2), Some(1));
        assert!(!reorder.is_identity());
    }

    #[test]
    fn a_workspace_is_found_by_its_stable_id_wherever_it_sits() {
        let mut monitor = monitor_with_workspaces(3);
        let id = monitor.workspaces()[2].id.clone();

        assert_eq!(monitor.workspace_idx_for_id(&id), Some(2));

        monitor.reorder_workspace(2, 0).unwrap();

        assert_eq!(monitor.workspace_idx_for_id(&id), Some(0));
        assert_eq!(
            monitor.workspace_for_id(&id).map(|w| w.id.clone()),
            Some(id)
        );
        assert_eq!(monitor.workspace_idx_for_id(&WorkspaceId::new()), None);
    }

    #[test]
    fn reordering_moves_one_workspace_and_shifts_only_the_ones_it_passed() {
        let mut monitor = monitor_with_workspaces(4);
        let before = workspace_ids(&monitor);

        let reorder = monitor.reorder_workspace(3, 1).unwrap();

        assert_eq!(
            workspace_ids(&monitor),
            vec![
                before[0].clone(),
                before[3].clone(),
                before[1].clone(),
                before[2].clone()
            ]
        );
        assert_eq!(reorder.new_idx(3), Some(1));
        assert_eq!(reorder.new_idx(1), Some(2));
        assert_eq!(reorder.new_idx(0), Some(0));
    }

    #[test]
    fn reordering_keeps_focus_on_the_workspace_which_had_it() {
        let mut monitor = monitor_with_workspaces(3);
        monitor.focus_workspace(2).unwrap();
        let focused = monitor.workspaces()[2].id.clone();

        monitor.reorder_workspace(0, 2).unwrap();

        assert_eq!(monitor.focused_workspace_idx(), 1);
        assert_eq!(
            monitor.focused_workspace().map(|w| w.id.clone()),
            Some(focused)
        );
    }

    #[test]
    fn reordering_moves_the_configured_names_with_their_workspaces() {
        let mut monitor = monitor_with_workspaces(3);
        monitor.workspace_names.insert(0, "first".to_string());
        monitor.workspace_names.insert(2, "third".to_string());

        monitor.reorder_workspace(0, 2).unwrap();

        assert_eq!(monitor.workspace_names.get(&2), Some(&"first".to_string()));
        assert_eq!(monitor.workspace_names.get(&1), Some(&"third".to_string()));
        assert_eq!(monitor.workspace_names.get(&0), None);
    }

    #[test]
    fn the_last_focused_workspace_index_follows_its_workspace() {
        let mut monitor = monitor_with_workspaces(3);
        monitor.focus_workspace(1).unwrap();
        monitor.focus_workspace(0).unwrap();
        assert_eq!(monitor.last_focused_workspace, Some(1));

        monitor.reorder_workspace(1, 2).unwrap();

        assert_eq!(monitor.last_focused_workspace, Some(2));
    }

    #[test]
    fn reordering_a_workspace_onto_its_own_index_changes_nothing() {
        let mut monitor = monitor_with_workspaces(3);
        let before = workspace_ids(&monitor);

        let reorder = monitor.reorder_workspace(1, 1).unwrap();

        assert!(reorder.is_identity());
        assert_eq!(workspace_ids(&monitor), before);
    }

    #[test]
    fn reordering_out_of_range_is_refused_without_moving_anything() {
        let mut monitor = monitor_with_workspaces(2);
        let before = workspace_ids(&monitor);

        assert!(monitor.reorder_workspace(0, 2).is_err());
        assert!(monitor.reorder_workspace(5, 0).is_err());
        assert!(monitor.swap_workspaces(0, 9).is_err());

        assert_eq!(workspace_ids(&monitor), before);
        assert_eq!(monitor.focused_workspace_idx(), 0);
    }

    #[test]
    fn swapping_exchanges_two_workspaces_and_leaves_the_rest_alone() {
        let mut monitor = monitor_with_workspaces(4);
        let before = workspace_ids(&monitor);

        monitor.swap_workspaces(0, 3).unwrap();

        assert_eq!(
            workspace_ids(&monitor),
            vec![
                before[3].clone(),
                before[1].clone(),
                before[2].clone(),
                before[0].clone()
            ]
        );
    }

    #[test]
    fn cycling_a_workspace_position_wraps_at_both_ends() {
        let mut monitor = monitor_with_workspaces(3);
        let before = workspace_ids(&monitor);

        monitor
            .cycle_workspace_position(0, CycleDirection::Previous)
            .unwrap();

        assert_eq!(
            workspace_ids(&monitor),
            vec![before[1].clone(), before[2].clone(), before[0].clone()]
        );

        monitor
            .cycle_workspace_position(2, CycleDirection::Next)
            .unwrap();

        assert_eq!(workspace_ids(&monitor), before);
    }

    #[test]
    fn a_reorder_drops_table_entries_which_describe_no_workspace() {
        let reorder = WorkspaceReorder::from_move(2, 0, 1);
        let mut table = HashMap::new();
        table.insert(0, "kept");
        table.insert(7, "stale");

        let remapped = reorder.remap_keys(&table);

        assert_eq!(remapped.get(&1), Some(&"kept"));
        assert_eq!(remapped.len(), 1);
    }

    #[test]
    fn test_add_container() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        // Add container to the default workspace
        m.add_container(Container::default(), Some(0)).unwrap();

        // Should contain a container in the current focused workspace
        let workspace = m.focused_workspace_mut().unwrap();
        assert_eq!(workspace.containers().len(), 1);
    }

    #[test]
    fn test_remove_workspace_by_idx() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        let new_workspace_index = m.new_workspace_idx();
        assert_eq!(new_workspace_index, 1);

        // Create workspace 2
        m.focus_workspace(new_workspace_index).unwrap();

        // Should have 2 workspaces
        assert_eq!(m.workspaces().len(), 2);

        // Create workspace 3
        m.focus_workspace(new_workspace_index + 1).unwrap();

        // Should have 3 workspaces
        assert_eq!(m.workspaces().len(), 3);

        // Remove workspace 1
        m.remove_workspace_by_idx(1);

        // Should have only 2 workspaces
        assert_eq!(m.workspaces().len(), 2);
    }

    #[test]
    fn test_remove_workspaces() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        let new_workspace_index = m.new_workspace_idx();
        assert_eq!(new_workspace_index, 1);

        // Create workspace 2
        m.focus_workspace(new_workspace_index).unwrap();

        // Should have 2 workspaces
        assert_eq!(m.workspaces().len(), 2);

        // Create workspace 3
        m.focus_workspace(new_workspace_index + 1).unwrap();

        // Should have 3 workspaces
        assert_eq!(m.workspaces().len(), 3);

        // Remove all workspaces
        m.remove_workspaces();

        // All workspaces should be removed
        assert_eq!(m.workspaces().len(), 0);
    }

    #[test]
    fn test_remove_nonexistent_workspace() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        // Try to remove a workspace that doesn't exist
        let removed_workspace = m.remove_workspace_by_idx(1);

        // Should return None since there is no workspace at index 1
        assert!(removed_workspace.is_none());
    }

    #[test]
    fn test_focus_workspace() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        let new_workspace_index = m.new_workspace_idx();
        assert_eq!(new_workspace_index, 1);

        // Focus workspace 2
        m.focus_workspace(new_workspace_index).unwrap();

        // Should have 2 workspaces
        assert_eq!(m.workspaces().len(), 2);

        // Should be focused on workspace 2
        assert_eq!(m.focused_workspace_idx(), 1);
    }

    #[test]
    fn test_new_workspace_idx() {
        let m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        let new_workspace_index = m.new_workspace_idx();

        // Should be the last workspace index: 1
        assert_eq!(new_workspace_index, 1);
    }

    #[test]
    fn test_move_container_to_workspace() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        let new_workspace_index = m.new_workspace_idx();
        assert_eq!(new_workspace_index, 1);

        {
            // Create workspace 1 and add 3 containers
            let workspace = m.focused_workspace_mut().unwrap();
            for _ in 0..3 {
                let container = Container::default();
                workspace.add_container_to_back(container);
            }

            // Should have 3 containers in workspace 1
            assert_eq!(m.focused_workspace().unwrap().containers().len(), 3);
        }

        // Create and focus workspace 2
        m.focus_workspace(new_workspace_index).unwrap();

        // Focus workspace 1
        m.focus_workspace(0).unwrap();

        // Move container to workspace 2
        m.move_container_to_workspace(1, true, None).unwrap();

        // Should be focused on workspace 2
        assert_eq!(m.focused_workspace_idx(), 1);

        // Workspace 2 should have 1 container now
        assert_eq!(m.focused_workspace().unwrap().containers().len(), 1);

        // Move to workspace 1
        m.focus_workspace(0).unwrap();

        // Workspace 1 should have 2 containers
        assert_eq!(m.focused_workspace().unwrap().containers().len(), 2);

        // Move a another container from workspace 1 to workspace 2 without following
        m.move_container_to_workspace(1, false, None).unwrap();

        // Should have 1 container
        assert_eq!(m.focused_workspace().unwrap().containers().len(), 1);

        // Should still be focused on workspace 1
        assert_eq!(m.focused_workspace_idx(), 0);

        // Switch to workspace 2
        m.focus_workspace(1).unwrap();

        // Workspace 2 should now have 2 containers
        assert_eq!(m.focused_workspace().unwrap().containers().len(), 2);
    }

    #[test]
    fn test_move_container_to_nonexistent_workspace() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        {
            // Create workspace 1 and add 3 containers
            let workspace = m.focused_workspace_mut().unwrap();
            for _ in 0..3 {
                let container = Container::default();
                workspace.add_container_to_back(container);
            }

            // Should have 3 containers in workspace 1
            assert_eq!(m.focused_workspace().unwrap().containers().len(), 3);
        }

        // Should only have 1 workspace
        assert_eq!(m.workspaces().len(), 1);

        // Try to move a container to a workspace that doesn't exist
        m.move_container_to_workspace(8, true, None).unwrap();

        // Should have 9 workspaces now
        assert_eq!(m.workspaces().len(), 9);

        // Should be focused on workspace 8
        assert_eq!(m.focused_workspace_idx(), 8);

        // Should have 1 container in workspace 8
        assert_eq!(m.focused_workspace().unwrap().containers().len(), 1);
    }

    #[test]
    fn test_ensure_workspace_count_workspace_contains_two_workspaces() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        // Create and focus another workspace
        let new_workspace_index = m.new_workspace_idx();
        m.focus_workspace(new_workspace_index).unwrap();

        // Should have 2 workspaces now
        assert_eq!(m.workspaces().len(), 2, "Monitor should have 2 workspaces");

        // Ensure the monitor has at least 5 workspaces
        m.ensure_workspace_count(5);

        // Monitor should have 5 workspaces
        assert_eq!(m.workspaces().len(), 5, "Monitor should have 5 workspaces");
    }

    #[test]
    fn test_ensure_workspace_count_only_default_workspace() {
        let mut m = Monitor::new(
            0,
            Rect::default(),
            Rect::default(),
            "TestMonitor".to_string(),
            "TestDevice".to_string(),
            "TestDeviceID".to_string(),
            Some("TestMonitorID".to_string()),
        );

        // Ensure the monitor has at least 5 workspaces
        m.ensure_workspace_count(5);

        // Monitor should have 5 workspaces
        assert_eq!(m.workspaces().len(), 5, "Monitor should have 5 workspaces");

        // Try to call the ensure workspace count again to ensure it doesn't change
        m.ensure_workspace_count(3);
        assert_eq!(m.workspaces().len(), 5, "Monitor should have 5 workspaces");
    }
}
