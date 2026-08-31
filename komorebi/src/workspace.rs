use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;

use crate::DATA_DIR;
use crate::DEFAULT_CONTAINER_PADDING;
use crate::DEFAULT_WORKSPACE_PADDING;
use crate::FloatingLayerBehaviour;
use crate::INITIAL_CONFIGURATION_LOADED;
use crate::KomorebiTheme;
use crate::NO_TITLEBAR;
use crate::REGEX_IDENTIFIERS;
use crate::REMOVE_TITLEBARS;
use crate::SocketMessage;
use crate::Wallpaper;
use crate::WindowContainerBehaviour;
use crate::border_manager;
use crate::container::Container;
use crate::container::ContainerState;
use crate::core::Axis;
use crate::core::CustomLayout;
use crate::core::CycleDirection;
use crate::core::DefaultLayout;
use crate::core::InitialWindowPlacementRules;
use crate::core::Layout;
use crate::core::LayoutDefaultEntry;
use crate::core::LayoutOptions;
use crate::core::OperationDirection;
use crate::core::PlacementMatchingRules;
use crate::core::PlacementTarget;
use crate::core::Rect;
use crate::core::Sizing;
use crate::core::WindowPlacement;
use crate::floating_geometry::FloatingBounds;
use crate::floating_geometry::FloatingLimits;
use crate::floating_geometry::plan_edge_resize;
use crate::floating_geometry::plan_move;
use crate::focus_history::Mru;
use crate::geometry::LogicalRect;
use crate::geometry::LogicalSlots;
use crate::geometry::RenderInsets;
use crate::geometry::SlotOrder;
use crate::geometry::SlotResize;
use crate::geometry::SlotShift;
use crate::geometry::SlotSplit;
use crate::geometry::SplitAxis;
use crate::lockable_sequence::LockableSequence;
use crate::managed_window::FloatingRejection;
use crate::managed_window::ManagedPlacement;
use crate::managed_window::ManagedWindow;
use crate::managed_window::Presentation;
use crate::managed_window::Visibility;
use crate::model::ContainerId;
use crate::model::WorkspaceId;
use crate::ring::Ring;
use crate::should_act;
use crate::should_act_individual;
use crate::stackbar_manager;
use crate::stackbar_manager::STACKBAR_TAB_HEIGHT;
use crate::static_config::WorkspaceConfig;
use crate::window::Window;
use crate::window::WindowDetails;
use crate::windows_api::WindowsApi;
use color_eyre::eyre;
use color_eyre::eyre::OptionExt;
use color_eyre::eyre::bail;
use komorebi_themes::Base16ColourPalette;
use komorebi_themes::KomorebiThemeCustom as Custom;
use serde::Deserialize;
use serde::Serialize;
use uds_windows::UnixStream;

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Workspace {
    #[serde(default = "WorkspaceId::new")]
    pub id: WorkspaceId,
    pub name: Option<String>,
    pub containers: Ring<Container>,
    /// Most-recently-used order of this workspace's container IDs.
    ///
    /// Container order is a spatial arrangement, so it cannot answer which container should be
    /// focused when this workspace is shown again or when the focused container goes away.
    #[serde(default)]
    pub container_focus_history: Mru<ContainerId>,
    /// Most-recently-used order of every window handle this workspace manages, across containers.
    ///
    /// The container history says which container was used last and each container says which of
    /// its own windows was; neither can answer a question about the workspace's windows as one
    /// list, which is what choosing the window a manual split moves needs. A window is recorded
    /// here by the same call which records it in the other two, so the three cannot disagree.
    #[serde(default)]
    pub window_focus_history: Mru<isize>,
    /// Most-recently-minimized window handles owned by this workspace.
    ///
    /// Minimizing keeps the window in its container, so this history is the only record of the
    /// order in which windows were minimized.
    #[serde(default)]
    pub minimize_history: Mru<isize>,
    /// The container this workspace currently shows alone, if it is in monocle mode.
    ///
    /// This is a reference into [`Self::containers`], not a second place a container can be
    /// stored: the monocle container keeps its position in the ring, its stable ID, its stack and
    /// both histories, and only stops sharing the work area with the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monocle_container_id: Option<ContainerId>,
    pub layout: Layout,
    pub layout_options: Option<LayoutOptions>,
    pub layout_rules: Vec<(usize, Layout)>,
    /// Threshold-based layout options rules (container_count >= threshold -> use these options).
    /// Sorted by threshold ascending at load time.
    #[serde(default)]
    pub layout_options_rules: Vec<(usize, LayoutOptions)>,
    /// Cached per-layout defaults from the global `layout_defaults` config setting.
    /// Pre-sorted at config load time; used as fallback when workspace has no overrides.
    #[serde(skip)]
    pub(crate) layout_defaults_cache: HashMap<DefaultLayout, CachedLayoutDefault>,
    pub work_area_offset_rules: Vec<(usize, Rect)>,
    pub layout_flip: Option<Axis>,
    pub workspace_padding: Option<i32>,
    pub container_padding: Option<i32>,
    /// The rendered rectangles of the containers which currently occupy an active logical slot,
    /// in container order.
    ///
    /// Hidden containers are absent, so this is not a per-container index into `containers`.
    pub latest_layout: Vec<Rect>,
    pub resize_dimensions: Vec<Option<Rect>>,
    /// Gap-free logical slots for this workspace's containers, keyed by stable container ID.
    ///
    /// This is the geometry authority. Splitting, adjacency, absorption and resizing read and
    /// write these rectangles; `latest_layout` only holds the rendered result derived from
    /// them, which is why it is keyed by index and these are keyed by identity.
    #[serde(default)]
    pub logical_slots: LogicalSlots,
    /// The gap-free area the current logical slots were calculated against.
    #[serde(default)]
    pub logical_work_area: Option<LogicalRect>,
    /// How to give a hidden container's slot back, keyed by the container which gave it up.
    ///
    /// A record only ever describes an absorption which actually happened, and it is consulted
    /// rather than trusted: the release is planned against the current slots and refused if the
    /// topology has moved underneath it.
    #[serde(default)]
    pub hidden_slot_restores: HashMap<ContainerId, HiddenSlotRestore>,
    /// Set when something has happened that local slot editing cannot express, so the next update
    /// recalculates the whole arrangement from the layout instead of reusing the current slots.
    #[serde(default = "default_true")]
    pub relayout_pending: bool,
    /// The arrangement inputs the current slots were calculated from.
    ///
    /// Not serialized: state restored from disk must recalculate once before its slots can be
    /// reused, which is the same thing `relayout_pending` defaulting to true says.
    #[serde(skip)]
    pub(crate) slot_inputs: Option<SlotInputs>,
    pub tile: bool,
    pub work_area_offset: Option<Rect>,
    pub apply_window_based_work_area_offset: bool,
    pub window_container_behaviour: Option<WindowContainerBehaviour>,
    pub window_container_behaviour_rules: Option<Vec<(usize, WindowContainerBehaviour)>>,
    pub float_override: Option<bool>,
    #[serde(skip)]
    pub globals: WorkspaceGlobals,
    pub layer: WorkspaceLayer,
    pub floating_layer_behaviour: Option<FloatingLayerBehaviour>,
    pub wallpaper: Option<Wallpaper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_config: Option<WorkspaceConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preselected_container_idx: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_swap_container_idx: Option<usize>,
    /// Initial window placement rules that determine where new tiled windows are placed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_window_placement_rules: Option<InitialWindowPlacementRules>,
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum WorkspaceLayer {
    #[default]
    Tiling,
    Floating,
}

impl Display for WorkspaceLayer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceLayer::Tiling => write!(f, "Tiling"),
            WorkspaceLayer::Floating => write!(f, "Floating"),
        }
    }
}

impl_ring_elements!(Workspace, Container);

/// A workspace restored from state which predates `relayout_pending` has to recalculate once
/// before its slots can be trusted, so the absent field means "pending" rather than "settled".
const fn default_true() -> bool {
    true
}

/// The arrangement inputs the current logical slots were produced from.
///
/// Compared, not interpreted: any difference means the layout would now arrange the workspace
/// differently, so the slots have to be recalculated instead of edited.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotInputs {
    layout: Layout,
    layout_flip: Option<Axis>,
    layout_options: Option<LayoutOptions>,
    resize_dimensions: Vec<Option<Rect>>,
    containers: Vec<ContainerId>,
    monocle: Option<ContainerId>,
}

/// Where a newly managed window ended up.
///
/// Returned rather than only logged because what follows differs: joining an existing container is
/// a stack change and the stackbar has to be told about it, while creating one is an arrangement
/// change and is not a stacking event at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewWindowPlacement {
    /// The window got a container of its own whose slot the layout will produce.
    ///
    /// This is the empty-workspace case, an explicit preselection, and the fallback taken when a
    /// donor's slot cannot be halved.
    NewContainer(ContainerId),
    /// A new container was split off `donor`'s slot along `axis`.
    Split {
        created: ContainerId,
        donor: ContainerId,
        axis: SplitAxis,
    },
    /// The window joined an existing active container's stack.
    Joined(ContainerId),
}

/// Where a container which arrived from another workspace ended up.
///
/// Distinct from [`NewWindowPlacement`] because adoption has an outcome placing a new window does
/// not: a container can arrive with nothing for the arrangement to place, and then it takes no
/// slot from anyone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerArrival {
    /// The container arrived hidden. It kept its windows and its identity and claimed no slot, so
    /// the containers already here kept the whole area between them.
    Hidden(ContainerId),
    /// The workspace had no slot to divide, so the arrangement gives this container the work area.
    ///
    /// This is the empty-workspace case and the case of a workspace whose containers are all
    /// hidden.
    Alone(ContainerId),
    /// The container took half of `donor`'s slot, divided along `axis`.
    Split {
        arrived: ContainerId,
        donor: ContainerId,
        axis: SplitAxis,
    },
    /// A donor's slot could not be halved, so the whole workspace is rearranged instead.
    Rearranged(ContainerId),
}

impl ContainerArrival {
    /// The container which arrived, whichever way it was placed.
    #[must_use]
    pub const fn arrived(&self) -> &ContainerId {
        match self {
            Self::Hidden(id) | Self::Alone(id) | Self::Rearranged(id) => id,
            Self::Split { arrived, .. } => arrived,
        }
    }
}

/// What a container's departure does to the arrangement it leaves behind.
///
/// Deliberately distinct from an absent plan: a hidden container leaving changes no geometry and
/// must not cost the workspace its manual boundaries, while a slot no edge can absorb must.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlotDeparture {
    /// The departing container held no active slot, so the tiling is already correct.
    Unaffected,
    /// The freed slot is taken by the complete edge group in this shift.
    Absorbed(SlotShift),
}

/// How to give one hidden container's logical slot back to it.
///
/// This is only ever written from an absorption which actually happened, so `absorbers` names the
/// containers which grew and `absorber_rects_before` holds exactly what they held before they did.
/// Restoration is planned against the current slots rather than replayed from here: the record says
/// what was done, and the geometry says whether undoing it is still possible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct HiddenSlotRestore {
    /// The slot the container occupied before it was hidden.
    pub old_rect: LogicalRect,
    /// The side the absorbers were on, and therefore the edge they have to move back.
    pub direction: OperationDirection,
    pub absorbers: Vec<ContainerId>,
    pub absorber_rects_before: Vec<(ContainerId, LogicalRect)>,
    /// The geometry generation the absorption was applied at.
    ///
    /// Diagnostic rather than decisive: the release is validated by comparing the absorbers'
    /// current rectangles against what the absorption gave them, which is strictly stronger than
    /// a generation comparison and cannot be fooled by a change that happens to restore the count.
    pub geometry_generation: u64,
    /// Whether an exact reverse was possible at all when the container was hidden.
    ///
    /// False when no edge could absorb the slot and the workspace was fully relaid out instead, so
    /// `old_rect` is only a position anchor for the fallback placement.
    pub exact_restore_valid: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            id: WorkspaceId::new(),
            name: None,
            containers: Ring::default(),
            container_focus_history: Mru::default(),
            window_focus_history: Mru::default(),
            minimize_history: Mru::default(),
            monocle_container_id: None,
            layout: Layout::Default(DefaultLayout::BSP),
            layout_options: None,
            layout_rules: vec![],
            layout_options_rules: vec![],
            layout_defaults_cache: HashMap::new(),
            work_area_offset_rules: vec![],
            layout_flip: None,
            workspace_padding: Option::from(DEFAULT_WORKSPACE_PADDING.load(Ordering::SeqCst)),
            container_padding: Option::from(DEFAULT_CONTAINER_PADDING.load(Ordering::SeqCst)),
            latest_layout: vec![],
            resize_dimensions: vec![],
            logical_slots: LogicalSlots::default(),
            logical_work_area: None,
            hidden_slot_restores: HashMap::new(),
            relayout_pending: true,
            slot_inputs: None,
            tile: true,
            work_area_offset: None,
            apply_window_based_work_area_offset: true,
            window_container_behaviour: None,
            window_container_behaviour_rules: None,
            float_override: None,
            layer: Default::default(),
            floating_layer_behaviour: Default::default(),
            globals: Default::default(),
            workspace_config: None,
            wallpaper: None,
            preselected_container_idx: None,
            promotion_swap_container_idx: None,
            initial_window_placement_rules: None,
        }
    }
}

#[derive(Debug)]
pub enum WorkspaceWindowLocation {
    Monocle(usize),          // window_idx
    Container(usize, usize), // container_idx, window_idx
    Floating(usize),         // idx in the derived floating window order
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Settings setup either by the parent monitor or by the `WindowManager`
pub struct WorkspaceGlobals {
    pub container_padding: Option<i32>,
    pub workspace_padding: Option<i32>,
    pub border_width: i32,
    pub border_offset: i32,
    pub work_area: Rect,
    /// The parent monitor's full bounds, which is the rectangle a fullscreen window is drawn into.
    ///
    /// This is deliberately not the work area: a fullscreen window covers the taskbar, and this is
    /// also the rectangle `WindowsApi::is_fullscreen` recognises when an application puts itself
    /// into borderless fullscreen without being asked.
    #[serde(default)]
    pub monitor_size: Rect,
    pub work_area_offset: Option<Rect>,
    pub window_based_work_area_offset: Option<Rect>,
    pub window_based_work_area_offset_limit: isize,
    pub floating_layer_behaviour: Option<FloatingLayerBehaviour>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Cached per-layout default options (pre-sorted rules) derived from the global `layout_defaults`.
pub(crate) struct CachedLayoutDefault {
    pub layout_options: Option<LayoutOptions>,
    /// Threshold-based rules, sorted by threshold ascending at load time
    pub layout_options_rules: Vec<(usize, LayoutOptions)>,
}

/// Convert an optional HashMap of threshold-based layout options rules into a Vec sorted by
/// threshold ascending.
fn sorted_layout_options_rules(
    rules: Option<&HashMap<usize, LayoutOptions>>,
) -> Vec<(usize, LayoutOptions)> {
    match rules {
        Some(rules) => {
            let mut sorted: Vec<(usize, LayoutOptions)> =
                rules.iter().map(|(t, o)| (*t, *o)).collect();
            sorted.sort_by_key(|(t, _)| *t);
            sorted
        }
        None => vec![],
    }
}

/// Find the highest matching threshold rule for the given container count.
/// Rules must be sorted by threshold ascending.
fn resolve_threshold_match(
    rules: &[(usize, LayoutOptions)],
    container_count: usize,
) -> Option<LayoutOptions> {
    rules
        .iter()
        .rev()
        .find(|(threshold, _)| container_count >= *threshold)
        .map(|(_, opts)| *opts)
}

/// A floating window and the rectangle a geometry command produced for it.
///
/// `changed` distinguishes a command which moved a window from one which was already against the
/// clamp, so a caller can report a no-op without comparing rectangles itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingGeometryChange {
    pub hwnd: isize,
    pub rect: Rect,
    pub changed: bool,
}

impl Workspace {
    pub fn load_static_config(
        &mut self,
        config: &WorkspaceConfig,
        layout_defaults: Option<&HashMap<DefaultLayout, LayoutDefaultEntry>>,
    ) -> eyre::Result<()> {
        self.name = Option::from(config.name.clone());

        self.container_padding = config.container_padding;

        self.workspace_padding = config.workspace_padding;

        if let Some(layout) = &config.layout {
            self.layout = Layout::Default(*layout);
        }

        #[allow(deprecated)]
        if let Some(pathbuf) = &config.custom_layout {
            let layout = CustomLayout::from_path(pathbuf)?;
            self.layout = Layout::Custom(layout);
        }

        #[allow(deprecated)]
        {
            self.tile = !(config.custom_layout.is_none()
                && config.layout.is_none()
                && config.tile.is_none()
                || config.tile.is_some_and(|tile| !tile));
        }

        let mut all_layout_rules = vec![];
        if let Some(layout_rules) = &config.layout_rules {
            for (count, rule) in layout_rules {
                all_layout_rules.push((*count, Layout::Default(*rule)));
            }

            all_layout_rules.sort_by_key(|(i, _)| *i);
            self.tile = true;
        }

        self.layout_rules = all_layout_rules.clone();

        #[allow(deprecated)]
        if let Some(layout_rules) = &config.custom_layout_rules {
            for (count, pathbuf) in layout_rules {
                let rule = CustomLayout::from_path(pathbuf)?;
                all_layout_rules.push((*count, Layout::Custom(rule)));
            }

            all_layout_rules.sort_by_key(|(i, _)| *i);
            self.tile = true;
            self.layout_rules = all_layout_rules;
        }

        let mut all_work_area_offset_rules = vec![];
        if let Some(work_area_offset_rules) = &config.work_area_offset_rules {
            for (count, rect) in work_area_offset_rules {
                all_work_area_offset_rules.push((*count, *rect));
            }
            all_work_area_offset_rules.sort_by_key(|(i, _)| *i);
            self.work_area_offset_rules = all_work_area_offset_rules;
        }

        self.work_area_offset = config.work_area_offset;

        self.apply_window_based_work_area_offset =
            config.apply_window_based_work_area_offset.unwrap_or(true);

        self.window_container_behaviour = config.window_container_behaviour;

        if let Some(window_container_behaviour_rules) = &config.window_container_behaviour_rules {
            if window_container_behaviour_rules.is_empty() {
                self.window_container_behaviour_rules = None;
            } else {
                let mut all_rules = vec![];
                for (count, behaviour) in window_container_behaviour_rules {
                    all_rules.push((*count, *behaviour));
                }

                all_rules.sort_by_key(|(i, _)| *i);
                self.window_container_behaviour_rules = Some(all_rules);
            }
        } else {
            self.window_container_behaviour_rules = None;
        }

        self.float_override = config.float_override;
        self.layout_flip = config.layout_flip;
        self.floating_layer_behaviour = config.floating_layer_behaviour;
        self.wallpaper = config.wallpaper.clone();

        // Load layout options directly (LayoutOptions is used in both config and runtime)
        self.layout_options = config.layout_options;
        self.initial_window_placement_rules = config.initial_window_placement_rules.clone();

        // Load threshold-based layout options rules, sorted by threshold ascending
        self.layout_options_rules =
            sorted_layout_options_rules(config.layout_options_rules.as_ref());

        tracing::debug!(
            "Workspace '{}' loaded layout_options: {:?}, layout_options_rules: {} entries",
            self.name.as_deref().unwrap_or("unnamed"),
            self.layout_options,
            self.layout_options_rules.len(),
        );

        // Cache per-layout defaults from global layout_defaults, pre-sorting rules
        self.layout_defaults_cache = if let Some(defaults) = layout_defaults {
            defaults
                .iter()
                .map(|(layout, entry)| {
                    (
                        *layout,
                        CachedLayoutDefault {
                            layout_options: entry.layout_options,
                            layout_options_rules: sorted_layout_options_rules(
                                entry.layout_options_rules.as_ref(),
                            ),
                        },
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };

        self.workspace_config = Some(config.clone());

        Ok(())
    }

    /// Compute effective layout options using the complete-replacement cascade:
    ///
    /// If the workspace defines EITHER `layout_options` OR `layout_options_rules`,
    /// it completely replaces the global `layout_defaults` for this layout.
    /// Global defaults are only used when the workspace has NEITHER setting.
    ///
    /// Within the effective source (workspace or global):
    ///   1. Try threshold match from rules (highest matching threshold wins)
    ///   2. If a rule matches -> use it (full replacement of base)
    ///   3. Else -> use the base `layout_options`
    fn effective_layout_options(&self) -> Option<LayoutOptions> {
        let container_count = self.containers().len();

        let has_workspace_overrides =
            self.layout_options.is_some() || !self.layout_options_rules.is_empty();

        let (effective_base, effective_rules): (Option<LayoutOptions>, &[(usize, LayoutOptions)]) =
            if has_workspace_overrides {
                (self.layout_options, &self.layout_options_rules)
            } else {
                match &self.layout {
                    Layout::Default(dl) => match self.layout_defaults_cache.get(dl) {
                        Some(entry) => (entry.layout_options, &entry.layout_options_rules),
                        None => (None, &[]),
                    },
                    Layout::Custom(_) => (None, &[]),
                }
            };

        resolve_threshold_match(effective_rules, container_count).or(effective_base)
    }

    pub fn hide(&mut self, omit: Option<isize>) {
        // Floating windows are hidden by the container which owns them, in the same pass as the
        // stored windows, so leaving a workspace cannot leave one of them on screen.
        for container in self.containers_mut() {
            container.hide(omit)
        }
    }

    pub fn apply_wallpaper(
        &self,
        hmonitor: isize,
        monitor_wp: &Option<Wallpaper>,
    ) -> eyre::Result<()> {
        if let Some(wallpaper) = self.wallpaper.as_ref().or(monitor_wp.as_ref()) {
            if let Err(error) = WindowsApi::set_wallpaper(&wallpaper.path, hmonitor) {
                tracing::error!("failed to set wallpaper: {error}");
            }

            if wallpaper.generate_theme.unwrap_or(true) {
                let variant = wallpaper
                    .theme_options
                    .as_ref()
                    .and_then(|t| t.theme_variant)
                    .unwrap_or_default();

                let cached_palette = DATA_DIR.join(format!(
                    "{}.base16.{variant}.json",
                    wallpaper
                        .path
                        .file_name()
                        .unwrap_or(OsStr::new("tmp"))
                        .to_string_lossy()
                ));

                let mut base16_palette = None;

                if cached_palette.is_file() {
                    tracing::info!(
                        "colour palette for wallpaper {} found in cache",
                        cached_palette.display()
                    );

                    // this code is VERY slow on debug builds - should only be a one-time issue when loading
                    // an uncached wallpaper
                    if let Ok(palette) = serde_json::from_str::<Base16ColourPalette>(
                        &std::fs::read_to_string(&cached_palette)?,
                    ) {
                        base16_palette = Some(palette);
                    }
                };

                if base16_palette.is_none() {
                    base16_palette =
                        komorebi_themes::generate_base16_palette(&wallpaper.path, variant).ok();

                    std::fs::write(
                        &cached_palette,
                        serde_json::to_string_pretty(&base16_palette)?,
                    )?;

                    tracing::info!(
                        "colour palette for wallpaper {} cached",
                        cached_palette.display()
                    );
                }

                if let Some(palette) = base16_palette {
                    let komorebi_theme = KomorebiTheme::Custom(Custom {
                        colours: Box::new(palette),
                        single_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.single_border),
                        stack_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.stack_border),
                        monocle_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.monocle_border),
                        floating_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.floating_border),
                        unfocused_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.unfocused_border),
                        unfocused_locked_border: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.unfocused_locked_border),
                        stackbar_focused_text: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.stackbar_focused_text),
                        stackbar_unfocused_text: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.stackbar_unfocused_text),
                        stackbar_background: wallpaper
                            .theme_options
                            .as_ref()
                            .and_then(|o| o.stackbar_background),
                        bar_accent: wallpaper.theme_options.as_ref().and_then(|o| o.bar_accent),
                    });

                    let bytes = SocketMessage::Theme(Box::new(komorebi_theme)).as_bytes()?;

                    let socket = DATA_DIR.join("komorebi.sock");
                    match UnixStream::connect(socket) {
                        Ok(mut stream) => {
                            if let Err(error) = stream.write_all(&bytes) {
                                tracing::error!("failed to send theme update message: {error}")
                            }
                        }
                        Err(error) => {
                            tracing::error!("{error}")
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn restore(
        &mut self,
        mouse_follows_focus: bool,
        hmonitor: isize,
        monitor_wp: &Option<Wallpaper>,
    ) -> eyre::Result<()> {
        if let Some(container) = self.monocle_container()
            && let Some(window) = container.focused_window()
        {
            container.restore();
            window.focus(mouse_follows_focus)?;
            return self.apply_wallpaper(hmonitor, monitor_wp);
        }

        let idx = self.focused_container_idx();
        let mut to_focus = None;

        for (i, container) in self.containers_mut().iter_mut().enumerate() {
            if let Some(window) = container.focused_window_mut()
                && idx == i
            {
                to_focus = Option::from(*window);
            }

            container.restore();
        }

        if let Some(container) = self.focused_container_mut() {
            container.focus_window(container.focused_window_idx());
        }

        // Do this here to make sure that an error doesn't stop the restoration of other windows.
        // Presented windows and floating windows should always be drawn at the top of the Z order
        // when switching to a workspace. A presented window has already been shown by the
        // container which owns it, so only the Z order is still left to settle here.
        let presented_window = self.presented_window();

        if let Some(window) = to_focus {
            if presented_window.is_none() && matches!(self.layer, WorkspaceLayer::Tiling) {
                window.focus(mouse_follows_focus)?;
            } else if let Some(presented_window) = presented_window {
                presented_window.focus(mouse_follows_focus)?;
            } else if let Some(floating_window) = self.focused_floating_window() {
                floating_window.focus(mouse_follows_focus)?;
            }
        } else if let Some(presented_window) = presented_window {
            presented_window.focus(mouse_follows_focus)?;
        } else if let Some(floating_window) = self.focused_floating_window() {
            floating_window.focus(mouse_follows_focus)?;
        }

        self.apply_wallpaper(hmonitor, monitor_wp)
    }

    pub fn update(&mut self) -> eyre::Result<()> {
        if !INITIAL_CONFIGURATION_LOADED.load(Ordering::SeqCst) {
            return Ok(());
        }

        // make sure we are never holding on to empty containers
        self.containers_mut()
            .retain(|c| c.is_preselect() || !c.windows().is_empty());
        self.prune_monocle_reference();

        let container_padding = self
            .container_padding
            .or(self.globals.container_padding)
            .unwrap_or_default();
        let workspace_padding = self
            .workspace_padding
            .or(self.globals.workspace_padding)
            .unwrap_or_default();
        let border_width = self.globals.border_width;
        let border_offset = self.globals.border_offset;
        let work_area = self.globals.work_area;
        let fullscreen_rect = self.fullscreen_rect();
        let window_based_work_area_offset = self.globals.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.globals.window_based_work_area_offset_limit;
        let mut rules_work_area_offset = None;

        if !self.work_area_offset_rules.is_empty() {
            let count = if self.is_monocle() {
                1
            } else {
                self.containers().len()
            };

            for (threshold, work_area_offset_rule) in &self.work_area_offset_rules {
                if count >= *threshold {
                    rules_work_area_offset = Some(*work_area_offset_rule);
                }
            }
        };

        let work_area_offset = rules_work_area_offset
            .or(self.work_area_offset)
            .or(self.globals.work_area_offset);

        let mut adjusted_work_area = work_area_offset.map_or_else(
            || work_area,
            |offset| {
                let mut with_offset = work_area;
                with_offset.left += offset.left;
                with_offset.top += offset.top;
                with_offset.right -= offset.right;
                with_offset.bottom -= offset.bottom;

                with_offset
            },
        );
        if (self.containers().len() <= window_based_work_area_offset_limit as usize
            || self.is_monocle() && window_based_work_area_offset_limit > 0)
            && self.apply_window_based_work_area_offset
        {
            adjusted_work_area = window_based_work_area_offset.map_or_else(
                || adjusted_work_area,
                |offset| {
                    let mut with_offset = adjusted_work_area;
                    with_offset.left += offset.left;
                    with_offset.top += offset.top;
                    with_offset.right -= offset.right;
                    with_offset.bottom -= offset.bottom;

                    with_offset
                },
            );
        }

        adjusted_work_area.add_padding(workspace_padding);

        self.enforce_resize_constraints();

        if !self.layout_rules.is_empty() {
            let mut updated_layout = None;

            for (threshold, layout) in &self.layout_rules {
                if self.containers().len() >= *threshold {
                    updated_layout = Option::from(layout.clone());
                }
            }

            if let Some(updated_layout) = updated_layout {
                self.layout = updated_layout;
            }
        }

        if let Some(window_container_behaviour_rules) = &self.window_container_behaviour_rules {
            let mut updated_behaviour = None;
            for (threshold, behaviour) in window_container_behaviour_rules {
                if self.containers().len() >= *threshold {
                    updated_behaviour = Option::from(*behaviour);
                }
            }

            self.window_container_behaviour = updated_behaviour;
        }

        if self.tile {
            if let Some(monocle_idx) = self.monocle_container_idx() {
                // The monocle container is the only container which owns a slot while monocle is
                // on, and that slot is the whole work area. Recording it keeps the geometry
                // authority consistent instead of leaving the previous arrangement behind.
                self.record_monocle_slot(monocle_idx, adjusted_work_area);

                adjusted_work_area.add_padding(container_padding);
                adjusted_work_area.add_padding(border_offset);
                adjusted_work_area.add_padding(border_width);

                if let Some(container) = self.containers_mut().get_mut(monocle_idx)
                    && let Some(window) = container.focused_window_mut()
                {
                    window.set_position(&adjusted_work_area, true)?;
                }
            } else if !self.containers().is_empty() {
                let effective_layout_options = self.effective_layout_options();

                tracing::debug!(
                    "Workspace '{}' update() - effective_layout_options: {:?} (base: {:?}, rules: {})",
                    self.name.as_deref().unwrap_or("unnamed"),
                    effective_layout_options,
                    self.layout_options,
                    self.layout_options_rules.len(),
                );
                // The gap-free slots are the geometry authority; the container gap is applied
                // per slot in the render conversion below and nowhere else.
                self.record_logical_slots(adjusted_work_area);

                let should_remove_titlebars = REMOVE_TITLEBARS.load(Ordering::SeqCst);
                let no_titlebar = NO_TITLEBAR.lock().clone();
                let regex_identifiers = REGEX_IDENTIFIERS.lock().clone();
                let stackbar_tab_height = STACKBAR_TAB_HEIGHT.load(Ordering::SeqCst);

                // A hidden container owns no slot, so it is absent here and is never repositioned
                // by its container; its floating windows keep their own rectangles and its
                // minimized windows stay invisible.
                let mut layouts = Vec::new();
                let mut rendered = HashMap::new();

                for (i, container) in self.containers().iter().enumerate() {
                    let Some(slot) = self.logical_slots.get(&container.id) else {
                        continue;
                    };

                    let stackbar_height =
                        if stackbar_manager::should_have_stackbar(container.windows().len()) {
                            stackbar_tab_height + container_padding
                        } else {
                            0
                        };

                    let render_rect = slot.to_render_rect(RenderInsets {
                        container_padding,
                        border_offset,
                        border_width,
                        stackbar_height,
                    });

                    rendered.insert(i, render_rect);
                    layouts.push(render_rect);
                }

                let containers = self.containers_mut();

                for (i, container) in containers.iter_mut().enumerate() {
                    if let Some(layout) = rendered.get(&i) {
                        // A container positions the windows it controls. A floating window keeps
                        // its own rectangle and a minimized window has none, so neither is moved
                        // into the container's slot.
                        for window in container.visible_stored_windows() {
                            if container
                                .focused_window()
                                .is_some_and(|w| w.hwnd == window.hwnd)
                            {
                                let should_remove_titlebar_for_window = should_act(
                                    &window.title().unwrap_or_default(),
                                    &window.exe().unwrap_or_default(),
                                    &window.class().unwrap_or_default(),
                                    &window.path().unwrap_or_default(),
                                    &no_titlebar,
                                    &regex_identifiers,
                                )
                                .is_some();

                                if should_remove_titlebars && should_remove_titlebar_for_window {
                                    window.remove_title_bar()?;
                                } else if should_remove_titlebar_for_window {
                                    window.add_title_bar()?;
                                }

                                // The model owns the presentation, so a window Win32 still has
                                // maximized after the model returned it to Normal is restored
                                // here rather than left disagreeing with its own state.
                                if window.window.is_maximized() && !window.is_maximized() {
                                    WindowsApi::restore_window(window.hwnd);
                                }
                            }

                            // A presented window keeps its container's slot but is not drawn in
                            // it; drawing it into the slot would silently drop its presentation.
                            // Each presentation is reapplied by its own call, never by the other.
                            match window.presentation {
                                Presentation::Maximized => window.window.maximize(),
                                Presentation::Fullscreen => {
                                    window.set_position(&fullscreen_rect, false)?;
                                }
                                Presentation::Normal => window.set_position(layout, false)?,
                            }
                        }
                    }
                }

                self.latest_layout = layouts;
            }
        }

        // Always make sure that the length of the resize dimensions vec is the same as the
        // number of layouts / containers. This should never actually truncate as the remove_window
        // function takes care of cleaning up resize dimensions when destroying empty containers
        let container_count = self.containers().len();

        // A monocle container stays in the ring, so the container count does not change while
        // monocle is on and this no longer has to be skipped to keep a resize adjustment which
        // the container would otherwise lose on reintegration.
        self.resize_dimensions.resize(container_count, None);

        Ok(())
    }

    pub fn container_for_window(&self, hwnd: isize) -> Option<&Container> {
        self.containers().get(self.container_idx_for_window(hwnd)?)
    }

    /// If there is a container which holds the window with `hwnd` it will focus that container.
    /// This function will only emit a focus on the window if it isn't the focused window of that
    /// container already.
    pub fn focus_container_by_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("there is no container/window")?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;

        let mut should_load = false;

        if container.focused_window_idx() != window_idx {
            should_load = true
        }

        container.focus_window(window_idx);

        if should_load {
            container.load_focused_window();
        }

        self.focus_container(container_idx);

        Ok(())
    }

    /// Every floating window this workspace owns, in container order and then stack order.
    ///
    /// A floating window is a fully managed window which its container does not position. It is
    /// derived from container membership rather than stored in a workspace-level list, so it
    /// cannot exist without a container and cannot be lost when its container changes.
    pub fn floating_managed_windows(&self) -> impl Iterator<Item = &ManagedWindow> {
        self.containers()
            .iter()
            .flat_map(|container| container.floating_windows())
    }

    pub fn floating_managed_windows_mut(&mut self) -> impl Iterator<Item = &mut ManagedWindow> {
        self.containers_mut()
            .iter_mut()
            .flat_map(Container::floating_windows_mut)
    }

    /// The floating windows in the order the floating layer cycles through them.
    #[must_use]
    pub fn floating_windows(&self) -> Vec<Window> {
        self.floating_managed_windows()
            .map(|window| window.window)
            .collect()
    }

    #[must_use]
    pub fn is_floating_window(&self, hwnd: isize) -> bool {
        self.floating_managed_windows()
            .any(|window| window.hwnd == hwnd)
    }

    /// The position of `hwnd` in [`Workspace::floating_windows`].
    #[must_use]
    pub fn floating_window_idx(&self, hwnd: isize) -> Option<usize> {
        self.floating_managed_windows()
            .position(|window| window.hwnd == hwnd)
    }

    /// The floating window the floating layer currently acts on.
    ///
    /// The focused container's focused window wins when it floats, because that is the window
    /// the user last selected. Otherwise the first floating window in cycle order is used, which
    /// is what makes the floating layer usable right after it is entered.
    #[must_use]
    pub fn focused_floating_window(&self) -> Option<Window> {
        if let Some(window) = self
            .focused_container()
            .and_then(Container::focused_managed_window)
            && window.placement == ManagedPlacement::Floating
        {
            return Some(window.window);
        }

        self.floating_managed_windows()
            .next()
            .map(|window| window.window)
    }

    #[must_use]
    pub fn focused_floating_window_idx(&self) -> usize {
        self.focused_floating_window()
            .and_then(|window| self.floating_window_idx(window.hwnd))
            .unwrap_or_default()
    }

    /// The window this workspace currently presents maximized, if it has one.
    ///
    /// Maximizing is a presentation change, so the maximized window is an ordinary member of the
    /// container which has owned it all along. This is a listing derived from that ownership, not
    /// a second place a window can be stored.
    #[must_use]
    pub fn maximized_managed_window(&self) -> Option<&ManagedWindow> {
        self.containers()
            .iter()
            .find_map(Container::maximized_managed_window)
    }

    #[must_use]
    pub fn maximized_window(&self) -> Option<Window> {
        self.maximized_managed_window().map(|window| window.window)
    }

    #[must_use]
    pub fn is_maximized_window(&self, hwnd: isize) -> bool {
        self.maximized_managed_window()
            .is_some_and(|window| window.hwnd == hwnd)
    }

    /// The window this workspace currently presents fullscreen, if it has one.
    #[must_use]
    pub fn fullscreened_managed_window(&self) -> Option<&ManagedWindow> {
        self.containers()
            .iter()
            .find_map(Container::fullscreened_managed_window)
    }

    #[must_use]
    pub fn fullscreened_window(&self) -> Option<Window> {
        self.fullscreened_managed_window()
            .map(|window| window.window)
    }

    /// The window this workspace draws over the arrangement, in either presentation.
    ///
    /// Maximized and fullscreen are separate presentations, but every caller which asks "is a
    /// window covering the arrangement right now" — Z order on workspace entry, directional
    /// selection, cross-monitor focus — means both of them and must not have to name them
    /// individually.
    #[must_use]
    pub fn presented_managed_window(&self) -> Option<&ManagedWindow> {
        self.containers()
            .iter()
            .find_map(Container::presented_managed_window)
    }

    #[must_use]
    pub fn presented_window(&self) -> Option<Window> {
        self.presented_managed_window().map(|window| window.window)
    }

    #[must_use]
    pub fn is_presented_window(&self, hwnd: isize) -> bool {
        self.presented_managed_window()
            .is_some_and(|window| window.hwnd == hwnd)
    }

    /// Whether the container a move would take away currently presents one of its windows.
    ///
    /// A maximized window used to be held outside every container, so any maximized window in the
    /// workspace blocked a container move. Now that it stays where it belongs, only the container
    /// actually being moved can block one.
    #[must_use]
    pub fn focused_container_has_presented_window(&self) -> bool {
        self.focused_container()
            .and_then(Container::presented_managed_window)
            .is_some()
    }

    /// The rendered rectangle the container at `idx` currently occupies, if it owns a slot.
    fn render_rect_at(&self, idx: usize) -> Option<Rect> {
        let container = self.containers().get(idx)?;
        let slot = self.logical_slots.get(&container.id)?;

        let container_padding = self
            .container_padding
            .or(self.globals.container_padding)
            .unwrap_or_default();

        let stackbar_height = if stackbar_manager::should_have_stackbar(container.windows().len()) {
            STACKBAR_TAB_HEIGHT.load(Ordering::SeqCst) + container_padding
        } else {
            0
        };

        Some(slot.to_render_rect(RenderInsets {
            container_padding,
            border_offset: self.globals.border_offset,
            border_width: self.globals.border_width,
            stackbar_height,
        }))
    }

    /// The window a presentation request acts on.
    ///
    /// The floating layer acts on the floating window it is cycling; otherwise the focused
    /// container's focused window is the subject. A minimized window is never a subject, because
    /// presenting it would make the model claim a presentation nothing is drawing.
    fn presentation_subject(&self) -> Option<isize> {
        if matches!(self.layer, WorkspaceLayer::Floating)
            && let Some(window) = self.focused_floating_window()
        {
            return Some(window.hwnd);
        }

        self.focused_container()
            .and_then(Container::focused_managed_window)
            .filter(|window| window.visibility == Visibility::Visible)
            .map(|window| window.hwnd)
    }

    /// Maximize the window this workspace currently acts on, in place.
    pub fn maximize_focused_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .presentation_subject()
            .ok_or_eyre("there is no window to maximize")?;

        self.maximize_window(hwnd)
    }

    pub fn maximize_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        self.enter_presentation(hwnd, Presentation::Maximized)
    }

    /// Present the window this workspace currently acts on fullscreen, in place.
    pub fn fullscreen_focused_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .presentation_subject()
            .ok_or_eyre("there is no window to make fullscreen")?;

        self.fullscreen_window(hwnd)
    }

    pub fn fullscreen_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        self.enter_presentation(hwnd, Presentation::Fullscreen)
    }

    /// The rectangle a fullscreen window is drawn into: the parent monitor's whole bounds.
    #[must_use]
    pub const fn fullscreen_rect(&self) -> Rect {
        self.globals.monitor_size
    }

    /// Move an owned window into `presentation`, in place.
    ///
    /// The window keeps its container, its position in that container's stack and both of its
    /// history entries. Only its presentation changes, which is why a presentation toggle no
    /// longer destroys a container and rebuilds a different one in its place.
    ///
    /// The two presentations are applied through different Win32 calls and never share one:
    /// maximizing is a window state, and fullscreen is the monitor rectangle. That separation is
    /// what stops one from being observed as the other.
    fn enter_presentation(&mut self, hwnd: isize, presentation: Presentation) -> eyre::Result<()> {
        if presentation == Presentation::Normal {
            bail!("normal is left through unmaximize or unfullscreen, not entered");
        }

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        // Read before the mutable borrow: the rectangle the window has now is the one it should
        // come back to when it stops being presented.
        let current_rect = WindowsApi::window_rect(hwnd).unwrap_or_default();
        let fullscreen_rect = self.fullscreen_rect();

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        let managed = &mut container.windows_mut()[window_idx];
        let previous = managed.presentation;
        let changed = match presentation {
            Presentation::Maximized => managed.set_maximized(current_rect),
            Presentation::Fullscreen => managed.set_fullscreen(current_rect),
            Presentation::Normal => unreachable!("refused above"),
        };

        let window = container.windows()[window_idx].window;
        container.focus_window(window_idx);
        self.focus_container(container_idx);

        // Reapplying the Win32 state of a window which is already presented this way is what makes
        // a duplicated command or event converge instead of toggling.
        match presentation {
            Presentation::Maximized => window.maximize(),
            Presentation::Fullscreen => {
                // A window Win32 still has maximized cannot be positioned reliably, so the window
                // state is dropped before the rectangle is applied. The model has already
                // committed; a failed Win32 call is reported rather than rolled back, because the
                // retile which follows reapplies the same rectangle from the same state.
                if previous == Presentation::Maximized || window.is_maximized() {
                    window.unmaximize();
                }

                if let Err(error) = window.set_position(&fullscreen_rect, true) {
                    tracing::warn!("could not make window {hwnd} fullscreen: {error}");
                }
            }
            Presentation::Normal => unreachable!("refused above"),
        }

        if !changed {
            tracing::debug!("window {hwnd} was already presented as {presentation:?}");
        }

        Ok(())
    }

    /// Return the maximized window to the presentation its placement implies.
    pub fn unmaximize_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .maximized_window()
            .ok_or_eyre("there is no maximized window")?
            .hwnd;

        self.leave_presentation(hwnd)
    }

    /// Return the fullscreen window to the presentation its placement implies.
    pub fn unfullscreen_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .fullscreened_window()
            .ok_or_eyre("there is no fullscreen window")?
            .hwnd;

        self.leave_presentation(hwnd)
    }

    fn leave_presentation(&mut self, hwnd: isize) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        let fallback = self.render_rect_at(container_idx).unwrap_or_default();

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        let managed = &mut container.windows_mut()[window_idx];
        let placement = managed.placement;
        let previous = managed.presentation;
        let target = managed.set_normal(fallback);
        let window = container.windows()[window_idx].window;

        // Only a maximized window has Win32 window state to drop. Dropping it for a fullscreen
        // window would be a no-op at best and would restore a rectangle the model does not own at
        // worst; a fullscreen window leaves its presentation purely by being repositioned.
        if previous == Presentation::Maximized {
            window.unmaximize();
        }

        // A stored window which had been maximized is put back in its slot by `unmaximize` and the
        // retile which follows. Everything else has to be positioned here: a floating window has
        // no slot to be retiled into, and a window leaving fullscreen has had no Win32 state
        // dropped and would otherwise stay on the monitor bounds until something else moved it.
        // The model transition has already committed, so a failed Win32 call is reported rather
        // than rolled back: the next retile reapplies the same rectangle from the same state.
        let reposition =
            placement == ManagedPlacement::Floating || previous == Presentation::Fullscreen;

        if reposition
            && let Some(target) = target
            && let Err(error) = window.set_position(&target, false)
        {
            tracing::warn!("could not restore the rectangle of window {hwnd}: {error}");
        }

        Ok(())
    }

    /// Make an owned window float without changing which container owns it.
    ///
    /// The window keeps its place in the container's stack, its focus history entry and its
    /// minimize history entry; only the placement changes, and the rectangle it currently has
    /// becomes its floating rectangle.
    pub fn float_window(&mut self, hwnd: isize, current_rect: Rect) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        container.windows_mut()[window_idx].set_floating(current_rect);

        Ok(())
    }

    /// Return an owned floating window to its container's control.
    pub fn unfloat_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        container.windows_mut()[window_idx].set_stored();
        container.focus_window(window_idx);
        self.focus_container(container_idx);

        Ok(())
    }

    /// The indices of the containers which currently occupy an active logical slot.
    ///
    /// Container order is preserved, because the arrangement is calculated from the position of
    /// an active container among the other active containers, not from its position among every
    /// container the workspace owns.
    #[must_use]
    pub fn active_container_indices(&self) -> Vec<usize> {
        self.containers()
            .iter()
            .enumerate()
            .filter(|(_, container)| container.is_active())
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn active_containers(&self) -> impl Iterator<Item = &Container> {
        self.containers()
            .iter()
            .filter(|container| container.is_active())
    }

    pub fn hidden_containers(&self) -> impl Iterator<Item = &Container> {
        self.containers()
            .iter()
            .filter(|container| container.is_hidden())
    }

    /// The `N` which the new-window placement thresholds are counted against.
    #[must_use]
    pub fn active_container_count(&self) -> usize {
        self.active_containers().count()
    }

    #[must_use]
    pub fn container_state(&self, idx: usize) -> Option<ContainerState> {
        self.containers().get(idx).map(Container::state)
    }

    /// The container a geometry operation should start from.
    ///
    /// Directional slot work needs an active container. A focused container with no active slot
    /// is what happens when the focus is on a floating window in a hidden container; the most
    /// recently used active container is then the documented starting point, and container order
    /// is the last resort when the history holds nothing usable.
    #[must_use]
    pub fn active_container_idx_for_geometry(&self) -> Option<usize> {
        let focused_idx = self.focused_container_idx();

        if self
            .containers()
            .get(focused_idx)
            .is_some_and(Container::is_active)
        {
            return Some(focused_idx);
        }

        for id in self.container_focus_history.iter() {
            if let Some(idx) = self
                .containers()
                .iter()
                .position(|container| &container.id == id && container.is_active())
            {
                return Some(idx);
            }
        }

        self.active_container_indices().first().copied()
    }

    /// Calculate this workspace's gap-free logical slots against `available_area`.
    ///
    /// Pure geometry: no Win32 call is made, so the slot authority can be exercised without a
    /// desktop session. The container gap is deliberately not passed to the arrangement - it is
    /// applied per slot by [`LogicalRect::to_render_rect`], so the slots returned here tile
    /// `available_area` exactly.
    ///
    /// Only active containers are arranged. A hidden container keeps its windows, its ID and its
    /// history, but it is absent from this calculation, so the active containers expand over the
    /// area it would otherwise have taken.
    #[must_use]
    pub fn calculate_logical_slots(&self, available_area: Rect) -> Vec<(ContainerId, LogicalRect)> {
        let active = self.active_container_indices();

        let Some(len) = NonZeroUsize::new(active.len()) else {
            return vec![];
        };

        // Both of these are keyed by container index, so they have to be projected onto the
        // active subset before the arrangement can consume them.
        let focused_idx = active
            .iter()
            .position(|idx| *idx == self.focused_container_idx())
            .unwrap_or(0);

        let resize_dimensions = active
            .iter()
            .map(|idx| self.resize_dimensions.get(*idx).copied().flatten())
            .collect::<Vec<_>>();

        let arranged = self.layout.as_boxed_arrangement().calculate(
            &available_area,
            len,
            None,
            self.layout_flip,
            &resize_dimensions,
            focused_idx,
            self.effective_layout_options(),
            &self.latest_layout,
        );

        active
            .iter()
            .filter_map(|idx| self.containers().get(*idx))
            .zip(arranged)
            .map(|(container, slot)| (container.id.clone(), LogicalRect::from(slot)))
            .collect()
    }

    /// The IDs of the containers which currently occupy an active logical slot, in container order.
    #[must_use]
    pub fn active_container_ids(&self) -> Vec<ContainerId> {
        self.containers()
            .iter()
            .filter(|container| container.is_active())
            .map(|container| container.id.clone())
            .collect()
    }

    /// Everything except the work area which decides what the layout would arrange.
    ///
    /// The slots are compared against this rather than against a flag set by each of the twenty-odd
    /// places which assign a layout, a flip, a resize adjustment or a container order. A flag can be
    /// forgotten at one of them and leave the arrangement stale; a fingerprint cannot.
    ///
    /// The focused container is deliberately absent. Only the scrolling layout arranges differently
    /// depending on it, and including it would let an ordinary focus change discard a local
    /// absorption; that one layout invalidates the geometry explicitly instead.
    #[must_use]
    fn slot_inputs(&self) -> SlotInputs {
        SlotInputs {
            layout: self.layout.clone(),
            layout_flip: self.layout_flip,
            layout_options: self.effective_layout_options(),
            resize_dimensions: self.resize_dimensions.clone(),
            containers: self
                .containers()
                .iter()
                .map(|container| container.id.clone())
                .collect(),
            monocle: self.monocle_container_id.clone(),
        }
    }

    /// Whether this workspace's layout arranges differently depending on which container is focused.
    #[must_use]
    const fn layout_follows_focus(&self) -> bool {
        matches!(self.layout, Layout::Default(DefaultLayout::Scrolling))
    }

    /// Declare that the current slots can no longer be edited locally.
    ///
    /// Every operation which changes the arrangement as a whole rather than one container's slot -
    /// switching layout, flipping it, resizing a boundary, reordering containers, moving across
    /// monitors, merging workspaces - calls this. The next update recalculates from the layout, and
    /// the hidden restore records go with it, because none of them can still describe a reversible
    /// absorption once the whole arrangement has been replaced.
    pub fn invalidate_slot_geometry(&mut self) {
        self.relayout_pending = true;
        self.hidden_slot_restores.clear();
        self.slot_inputs = None;
    }

    /// Bring the logical slots into agreement with the workspace's active containers.
    ///
    /// The slots are the geometry authority, not a cache of the layout, so they are only
    /// recalculated when they cannot be edited into agreement. A container which just became
    /// hidden gives its slot to a complete edge group; one which just became active takes its slot
    /// back from exactly the containers which absorbed it. Anything else - a changed work area, a
    /// container appearing without a restore record, an invalidated arrangement, or a mix of
    /// arrivals and departures - falls back to a full recalculation, which is always a valid
    /// tiling even when it is not the one the user had.
    ///
    /// Returns the active slots in container order so the caller can render them.
    pub fn record_logical_slots(
        &mut self,
        available_area: Rect,
    ) -> Vec<(ContainerId, LogicalRect)> {
        let area = LogicalRect::from(available_area);
        let active = self.active_container_ids();

        if self.try_local_slot_update(area, &active) {
            return self.active_slots_in_order(&active);
        }

        self.recalculate_logical_slots(available_area)
    }

    /// Edit the current slots into agreement with `active`, or report that it cannot be done.
    ///
    /// Nothing is written unless every step of the transition is possible, so a refusal here
    /// leaves the previous slots exactly as they were for the recalculation to replace.
    fn try_local_slot_update(&mut self, area: LogicalRect, active: &[ContainerId]) -> bool {
        if !self.slots_are_authoritative() || self.logical_work_area != Some(area) {
            return false;
        }

        let departed: Vec<ContainerId> = self
            .logical_slots
            .ordered(SlotOrder::TopToBottom)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !active.contains(id))
            .collect();

        let arrived: Vec<ContainerId> = active
            .iter()
            .filter(|id| !self.logical_slots.contains(id))
            .cloned()
            .collect();

        match (departed.is_empty(), arrived.is_empty()) {
            (true, true) => true,
            (false, true) => self.absorb_departed_slots(&departed),
            (true, false) => self.release_arrived_slots(&arrived),
            // A container leaving and another arriving in the same update is a rearrangement, not
            // two independent local edits: absorbing first would give away area the arrival needs.
            (false, false) => false,
        }
    }

    /// Give each departed container's slot to a complete edge group.
    ///
    /// A container which is still owned by this workspace gets a restore record; one which has
    /// been removed does not, because there is nothing left to restore it to.
    fn absorb_departed_slots(&mut self, departed: &[ContainerId]) -> bool {
        let mut shifts = Vec::with_capacity(departed.len());

        // Plan every absorption against the running result before writing any of it, so a group
        // which becomes incomplete because of an earlier absorption is caught here rather than
        // leaving the arrangement half-collapsed.
        let mut planned = self.logical_slots.clone();

        for id in departed {
            let Some(shift) = planned.plan_absorption(id) else {
                return false;
            };

            planned.apply_absorption(&shift);
            shifts.push(shift);
        }

        for shift in shifts {
            let still_owned = self.containers().iter().any(|c| c.id == shift.container);

            self.logical_slots.apply_absorption(&shift);

            if still_owned {
                self.hidden_slot_restores.insert(
                    shift.container.clone(),
                    HiddenSlotRestore {
                        old_rect: shift.slot,
                        direction: shift.direction,
                        absorbers: shift
                            .movers
                            .iter()
                            .map(|mover| mover.container.clone())
                            .collect(),
                        absorber_rects_before: shift.rects_before(),
                        geometry_generation: self.logical_slots.generation(),
                        exact_restore_valid: true,
                    },
                );
            } else {
                self.hidden_slot_restores.remove(&shift.container);
            }
        }

        true
    }

    /// Give each arrived container the slot it had before it was hidden.
    fn release_arrived_slots(&mut self, arrived: &[ContainerId]) -> bool {
        let mut shifts = Vec::with_capacity(arrived.len());
        let mut planned = self.logical_slots.clone();

        for id in arrived {
            let Some(record) = self.hidden_slot_restores.get(id) else {
                return false;
            };

            if !record.exact_restore_valid {
                return false;
            }

            let Some(shift) = planned.plan_release(
                id,
                record.old_rect,
                record.direction,
                &record.absorber_rects_before,
            ) else {
                return false;
            };

            planned.apply_release(&shift);
            shifts.push(shift);
        }

        for shift in shifts {
            self.logical_slots.apply_release(&shift);
            self.hidden_slot_restores.remove(&shift.container);
        }

        true
    }

    /// Whether the current slots are the arrangement this workspace is actually in.
    ///
    /// False means the layout has to be consulted again before anything is read from the slots, so
    /// no local edit may be applied and none may be adopted. The work area is deliberately not part
    /// of this: a caller which has one compares it, and a caller which is only editing the topology
    /// does not need one.
    #[must_use]
    fn slots_are_authoritative(&self) -> bool {
        !self.relayout_pending && self.slot_inputs.as_ref() == Some(&self.slot_inputs())
    }

    /// A split of the current slots, or `None` when the slots are not the arrangement this
    /// workspace is in.
    ///
    /// Editing slots which are about to be replaced would be harmless on its own. Adopting the
    /// result as the arrangement is not: it clears a recalculation something else asked for -
    /// a layout change, a merge, a monitor move - and freezes whatever the slots happened to hold,
    /// which after any of those need not even cover the work area. A caller which cannot get a
    /// split here inserts its container and lets the recalculation place it.
    fn plan_authoritative_split(
        &self,
        donor: &ContainerId,
        created: &ContainerId,
        axis: Option<SplitAxis>,
    ) -> Option<SlotSplit> {
        if !self.slots_are_authoritative() {
            return None;
        }

        self.logical_slots.plan_split(donor, created, axis)
    }

    /// What happens to the arrangement when `id` leaves this workspace.
    ///
    /// Planned while the departing slot is still in the map, because that is the only moment the
    /// group which can absorb it is knowable, and applied only once the container has actually left
    /// the ring, because the arrangement fingerprint contains the container list.
    #[must_use]
    fn plan_departure(&self, id: &ContainerId) -> Option<SlotDeparture> {
        if !self.slots_are_authoritative() {
            return None;
        }

        // A hidden container holds no slot, so its departure changes nothing about the tiling and
        // needs no expansion at all.
        if !self.logical_slots.contains(id) {
            return Some(SlotDeparture::Unaffected);
        }

        self.logical_slots
            .plan_absorption(id)
            .map(SlotDeparture::Absorbed)
    }

    /// Apply a planned departure and answer with the container focus should move to.
    ///
    /// `None` - no plan at all - is the signal that no single edge could take the freed slot, so
    /// the whole workspace is rearranged rather than left with a hole in it.
    fn apply_departure(
        &mut self,
        id: &ContainerId,
        departure: Option<SlotDeparture>,
    ) -> Option<ContainerId> {
        match departure {
            Some(SlotDeparture::Absorbed(shift)) => {
                let mut changed: Vec<ContainerId> = shift
                    .movers
                    .iter()
                    .map(|mover| mover.container.clone())
                    .collect();
                changed.push(id.clone());

                self.logical_slots.apply_absorption(&shift);
                self.invalidate_restores_touching(&changed);
                self.adopt_slot_geometry();

                shift.first_mover().cloned()
            }
            Some(SlotDeparture::Unaffected) => {
                self.invalidate_restores_touching(std::slice::from_ref(id));
                self.adopt_slot_geometry();

                None
            }
            None => {
                self.invalidate_slot_geometry();

                None
            }
        }
    }

    /// Mark every hidden restore record which named one of `changed` as no longer exactly
    /// reversible.
    ///
    /// A record is a promise that its absorbers still hold exactly the rectangles the absorption
    /// gave them. Once one of them has grown again, or left the workspace altogether, that promise
    /// is broken and the container it belongs to has to come back through a recalculation. The
    /// record is kept rather than dropped because its `old_rect` is still the anchor the fallback
    /// placement uses.
    fn invalidate_restores_touching(&mut self, changed: &[ContainerId]) {
        for record in self.hidden_slot_restores.values_mut() {
            if record
                .absorbers
                .iter()
                .any(|absorber| changed.contains(absorber))
            {
                record.exact_restore_valid = false;
            }
        }
    }

    /// The container focus moves to once the container at `idx` has been deleted.
    ///
    /// The first member of the group which expands over the freed slot, in the deterministic order
    /// for the direction it expanded from: top to bottom along a vertical edge, left to right along
    /// a horizontal one. `None` when nothing expands, which is a hidden container leaving, a
    /// rearrangement, or the last active container of the workspace.
    #[must_use]
    pub fn expansion_focus_target(&self, idx: usize) -> Option<ContainerId> {
        let id = &self.containers().get(idx)?.id;

        match self.plan_departure(id)? {
            SlotDeparture::Absorbed(shift) => shift.first_mover().cloned(),
            SlotDeparture::Unaffected => None,
        }
    }

    /// Move focus to the container an expansion chose, or to the previous one when it chose none.
    ///
    /// The recipient's own most recent focusable window is what gets focused, which is the window
    /// it was already showing. A minimized window is never restored to satisfy this.
    fn focus_after_removal(&mut self, target: Option<ContainerId>) {
        let Some(idx) = target.and_then(|id| self.container_idx_for_id(&id)) else {
            self.focus_previous_container();

            return;
        };

        self.focus_container(idx);

        let hwnd = self
            .containers()
            .get(idx)
            .and_then(|container| container.first_focusable_window())
            .map(|window| window.hwnd);

        if let Some(hwnd) = hwnd
            && let Some(container) = self.containers_mut().get_mut(idx)
        {
            container.focus_window_by_hwnd(hwnd);
        }
    }

    /// The slots of `active`, in container order, for the renderer.
    fn active_slots_in_order(&self, active: &[ContainerId]) -> Vec<(ContainerId, LogicalRect)> {
        active
            .iter()
            .filter_map(|id| Some((id.clone(), self.logical_slots.get(id)?)))
            .collect()
    }

    /// Replace every slot from the layout and report any tiling violation.
    ///
    /// This is the fallback, and it is what makes a hidden container's exact restore impossible:
    /// the arrangement it was recorded against no longer exists, so every record is dropped.
    pub fn recalculate_logical_slots(
        &mut self,
        available_area: Rect,
    ) -> Vec<(ContainerId, LogicalRect)> {
        let slots = self.calculate_logical_slots(available_area);
        let area = LogicalRect::from(available_area);

        self.logical_work_area = Some(area);
        self.logical_slots.replace_all(slots.clone());
        self.hidden_slot_restores.clear();
        self.relayout_pending = false;
        self.slot_inputs = Some(self.slot_inputs());

        if let Err(violations) = self.logical_slots.validate_coverage(area) {
            for violation in violations {
                tracing::warn!(
                    "workspace '{}' logical slots do not tile the work area: {violation}",
                    self.name.as_deref().unwrap_or("unnamed")
                );
            }
        }

        slots
    }

    /// The gap-free slot the container at `idx` currently occupies.
    #[must_use]
    pub fn logical_slot_at(&self, idx: usize) -> Option<LogicalRect> {
        self.logical_slots.get(&self.containers().get(idx)?.id)
    }

    /// The index of the container whose logical slot contains `point`.
    ///
    /// Slots are gap-free, so unlike the rendered rectangles they cannot leave a point in the
    /// gutter between two containers unattributed.
    #[must_use]
    pub fn container_idx_from_logical_point(&self, point: (i32, i32)) -> Option<usize> {
        self.containers().iter().position(|container| {
            self.logical_slots
                .get(&container.id)
                .is_some_and(|slot| slot.contains_point(point))
        })
    }

    pub fn container_idx_from_current_point(&self) -> Option<usize> {
        let point = WindowsApi::cursor_pos().ok()?;

        if let Some(idx) = self.container_idx_from_logical_point((point.x, point.y)) {
            return Some(idx);
        }

        let mut idx = None;

        for (i, _container) in self.containers().iter().enumerate() {
            if let Some(rect) = self.latest_layout.get(i)
                && rect.contains_point((point.x, point.y))
            {
                idx = Option::from(i);
            }
        }

        idx
    }

    pub fn hwnd_from_exe(&self, exe: &str) -> Option<isize> {
        for container in self.containers() {
            if let Some(hwnd) = container.hwnd_from_exe(exe) {
                return Option::from(hwnd);
            }
        }

        if let Some(container) = self.monocle_container()
            && let Some(hwnd) = container.hwnd_from_exe(exe)
        {
            return Option::from(hwnd);
        }

        None
    }

    pub fn location_from_exe(&self, exe: &str) -> Option<WorkspaceWindowLocation> {
        for (container_idx, container) in self.containers().iter().enumerate() {
            if let Some(window_idx) = container.idx_from_exe(exe) {
                return Some(WorkspaceWindowLocation::Container(
                    container_idx,
                    window_idx,
                ));
            }
        }

        if let Some(container) = self.monocle_container()
            && let Some(window_idx) = container.idx_from_exe(exe)
        {
            return Some(WorkspaceWindowLocation::Monocle(window_idx));
        }

        for (window_idx, window) in self.floating_managed_windows().enumerate() {
            if let Ok(window_exe) = window.exe()
                && exe == window_exe
            {
                return Some(WorkspaceWindowLocation::Floating(window_idx));
            }
        }

        None
    }

    pub fn contains_managed_window(&self, hwnd: isize) -> bool {
        for container in self.containers() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        if let Some(container) = self.monocle_container()
            && container.contains_window(hwnd)
        {
            return true;
        }

        false
    }

    pub fn is_focused_window_monocle_or_maximized(&self) -> eyre::Result<bool> {
        let hwnd = WindowsApi::foreground_window()?;
        if self.is_presented_window(hwnd) {
            return Ok(true);
        }

        if let Some(container) = self.monocle_container()
            && container.contains_window(hwnd)
        {
            return Ok(true);
        }

        Ok(false)
    }

    pub fn is_empty(&self) -> bool {
        self.containers().is_empty()
    }

    pub fn contains_window(&self, hwnd: isize) -> bool {
        for container in self.containers() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        if let Some(container) = self.monocle_container()
            && container.contains_window(hwnd)
        {
            return true;
        }

        false
    }

    pub fn promote_container(&mut self) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();
        let container = self
            .containers_mut()
            .remove_respecting_locks(focused_idx)
            .ok_or_eyre("there is no container")?;

        let primary_idx = match &self.layout {
            Layout::Default(_) => 0,
            Layout::Custom(layout) => layout.first_container_idx(
                layout
                    .primary_idx()
                    .ok_or_eyre("this custom layout does not have a primary column")?,
            ),
        };

        let insertion_idx = self
            .containers_mut()
            .insert_respecting_locks(primary_idx, container);
        self.focus_container(insertion_idx);

        Ok(())
    }

    pub fn add_container_to_back(&mut self, container: Container) {
        self.containers_mut().push_back(container);
        self.focus_last_container();
    }

    pub fn add_container_to_front(&mut self, container: Container) {
        self.containers_mut().push_front(container);
        self.focus_first_container();
    }

    // this fn respects locked container indexes - we should use it for pretty much everything
    // except monocle and maximize toggles
    pub fn insert_container_at_idx(&mut self, idx: usize, container: Container) -> usize {
        let insertion_idx = self
            .containers_mut()
            .insert_respecting_locks(idx, container);

        if insertion_idx > self.resize_dimensions.len() {
            self.resize_dimensions.push(None);
        } else {
            self.resize_dimensions.insert(insertion_idx, None);
        }

        self.focus_container(insertion_idx);

        insertion_idx
    }

    // this fn respects locked container indexes - we should use it for pretty much everything
    // except monocle and maximize toggles
    pub fn remove_container_by_idx(&mut self, idx: usize) -> Option<Container> {
        // Planned here, while the departing slot is still in the map and can still be seen by the
        // neighbours which are going to take it.
        let departure = self
            .containers()
            .get(idx)
            .map(|container| container.id.clone())
            .map(|id| (self.plan_departure(&id), id));

        let container = self.containers_mut().remove_respecting_locks(idx);

        if idx < self.resize_dimensions.len() {
            self.resize_dimensions.remove(idx);
        }

        if let Some(container) = &container {
            self.forget_container(container);
        }

        // Applied only now: the arrangement fingerprint contains the container list, so adopting
        // the edited slots before the container had left would adopt a list it is still in.
        if container.is_some()
            && let Some((departure, id)) = departure
        {
            self.apply_departure(&id, departure);
        }

        container
    }

    /// Drop every history entry belonging to a container which has left this workspace.
    ///
    /// The container keeps its own window history so a move can restore it at the destination;
    /// only this workspace's references are dropped.
    fn forget_container(&mut self, container: &Container) {
        self.container_focus_history.remove(&container.id);
        self.logical_slots.remove(&container.id);
        self.hidden_slot_restores.remove(&container.id);

        // The monocle reference points into the ring, so a container leaving the ring must take
        // the reference with it or the workspace would stay stuck in monocle mode with nothing
        // to show.
        if self.monocle_container_id.as_ref() == Some(&container.id) {
            self.monocle_container_id = None;
        }

        for window in container.windows() {
            self.minimize_history.remove(&window.hwnd);
            self.window_focus_history.remove(&window.hwnd);
        }
    }

    pub fn container_idx_for_window(&self, hwnd: isize) -> Option<usize> {
        let mut idx = None;
        for (i, x) in self.containers().iter().enumerate() {
            if x.contains_window(hwnd) {
                idx = Option::from(i);
            }
        }

        idx
    }

    pub fn remove_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        border_manager::delete_border(hwnd);
        self.minimize_history.remove(&hwnd);
        self.window_focus_history.remove(&hwnd);

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;

        // Planned before the window is taken out. Removing a window cannot change which neighbours
        // border this container's slot, so the answer is the same whether or not the container
        // turns out to be emptied by it.
        let expansion = self.expansion_focus_target(container_idx);

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .windows()
            .iter()
            .position(|window| window.hwnd == hwnd)
            .ok_or_eyre("there is no window")?;

        container
            .remove_window_by_idx(window_idx)
            .ok_or_eyre("there is no window")?;

        if container.windows().is_empty() {
            self.remove_container_by_idx(container_idx);
            self.focus_after_removal(expansion);
        } else {
            container.load_focused_window();
            if let Some(window) = container.focused_window() {
                window.focus(false)?;
            }
        }

        Ok(())
    }

    /// Detach a window from this workspace without changing the detached window's Win32 state.
    ///
    /// This is intentionally separate from [`Self::remove_window`]. Lifecycle removal may restore,
    /// unmaximize, or focus windows while a temporarily unmanaged window must be left exactly where
    /// Windows currently has it. Surviving stack members may still be shown because they remain
    /// managed by this workspace.
    pub fn detach_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        self.take_window(hwnd).map(|_| ())
    }

    /// Remove `hwnd` from this workspace and hand back its complete managed state.
    ///
    /// The placement, visibility, presentation and floating rectangle travel with the window, so
    /// a move to another workspace or monitor does not silently reset a floating window to a
    /// stored one.
    pub fn take_window(&mut self, hwnd: isize) -> eyre::Result<ManagedWindow> {
        border_manager::delete_border(hwnd);
        self.minimize_history.remove(&hwnd);
        self.window_focus_history.remove(&hwnd);

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;

        let expansion = self.expansion_focus_target(container_idx);

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;
        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;

        let window = container
            .remove_window_by_idx(window_idx)
            .ok_or_eyre("there is no window")?;

        if container.windows().is_empty() {
            self.remove_container_by_idx(container_idx);
            self.focus_after_removal(expansion);
        } else {
            container.load_focused_window();
        }

        Ok(window)
    }

    /// Take on a brand-new window as a floating window of a container of its own.
    ///
    /// The container is Hidden the moment it is created, because its only window is floating, so
    /// this adds no logical slot and does not disturb the tiled arrangement.
    pub fn add_floating_window(&mut self, window: Window) {
        let current_rect = WindowsApi::window_rect(window.hwnd).unwrap_or_default();
        let mut managed = ManagedWindow::capture(window, ContainerId::default());
        managed.set_floating(current_rect);

        self.adopt_managed_window(managed);
    }

    /// Adopt a window which already carries its managed state into a container of its own.
    ///
    /// This is the receiving half of [`Workspace::take_window`]: the new container gets a new
    /// stable ID and stamps its ownership on the window, and nothing else about the window's
    /// state is touched.
    pub fn adopt_managed_window(&mut self, window: ManagedWindow) {
        let mut container = Container::default();
        container.add_managed_window(window);
        self.add_container_to_back(container);
    }

    /// Take in a container which belongs to another workspace, keeping everything it is.
    ///
    /// The container arrives whole: its stable ID, its stack order, its window focus history and
    /// each window's placement, visibility, presentation and floating rectangle are the container
    /// value itself, so they survive by being moved rather than by being copied across. Its
    /// windows already name it as their container, so nothing is restamped either.
    ///
    /// Only where it goes is decided here, and the rule follows the container's own state. One
    /// which arrives hidden has no visible stored window for the arrangement to place, so it takes
    /// no slot and the containers already here keep the whole area. One which arrives active takes
    /// half of the geometry-focused container's slot, divided along its longer edge unless `axis`
    /// says otherwise - the same division a new window's container gets - and takes the work area
    /// outright when there is no active slot to divide.
    ///
    /// The target's exact hidden restores are discarded. They describe which containers absorbed a
    /// hidden slot and by how much, and a container arriving from elsewhere is exactly the
    /// topology change that description cannot survive.
    pub fn adopt_container(
        &mut self,
        container: Container,
        axis: Option<SplitAxis>,
    ) -> ContainerArrival {
        let arrived = container.id.clone();

        if container.is_hidden() {
            // Focus is not moved by a hidden arrival: it has no focusable window to move to, and
            // the container which was being shown goes on being shown.
            let restore = self
                .focused_container()
                .map(|container| container.id.clone());
            let idx = self.containers().len();

            self.insert_container_at_idx(idx, container);
            self.hidden_slot_restores.clear();

            if let Some(idx) = restore.and_then(|id| self.container_idx_for_id(&id)) {
                self.focus_container(idx);
            }

            return ContainerArrival::Hidden(arrived);
        }

        // The scrolling layout defines its own arrangement from the focused container, so a local
        // edit to its slots would not survive its next recalculation; it gets the rearrangement.
        let donor = if self.layout_follows_focus() {
            None
        } else {
            self.active_container_idx_for_geometry()
                .and_then(|idx| Some((idx, self.containers().get(idx)?.id.clone())))
        };

        let Some((donor_idx, donor_id)) = donor else {
            let idx = self.containers().len();
            self.insert_container_at_idx(idx, container);
            self.invalidate_slot_geometry();

            return ContainerArrival::Alone(arrived);
        };

        // Planned before anything is inserted, so a slot which cannot be halved costs nothing but
        // the rearrangement.
        let Some(split) = self.plan_authoritative_split(&donor_id, &arrived, axis) else {
            let idx = self.containers().len();
            self.insert_container_at_idx(idx, container);
            self.invalidate_slot_geometry();

            return ContainerArrival::Rearranged(arrived);
        };

        // Container order is what the layout would arrange, so the arrival is inserted where its
        // half actually is: to the left of the donor, or below it.
        let insertion_idx = match split.axis {
            SplitAxis::LeftRight => donor_idx,
            SplitAxis::TopBottom => donor_idx + 1,
        };

        self.insert_container_at_idx(insertion_idx, container);
        self.logical_slots.apply_split(&split);
        self.hidden_slot_restores.clear();
        self.adopt_slot_geometry();

        ContainerArrival::Split {
            arrived,
            donor: donor_id,
            axis: split.axis,
        }
    }

    pub fn remove_focused_container(&mut self) -> Option<Container> {
        let focused_idx = self.focused_container_idx();
        let expansion = self.expansion_focus_target(focused_idx);
        let container = self.remove_container_by_idx(focused_idx);
        self.focus_after_removal(expansion);

        container
    }

    pub fn remove_container(&mut self, idx: usize) -> Option<Container> {
        let expansion = self.expansion_focus_target(idx);
        let container = self.remove_container_by_idx(idx);
        self.focus_after_removal(expansion);

        container
    }

    /// The containers a destroyed container's windows are shared out among, in the order they are
    /// offered windows.
    ///
    /// An active container hands its windows to the same group which takes its area, so the windows
    /// and the space they occupied travel together. A hidden container has no area to give, so it
    /// falls back to the containers which absorbed it when it was hidden - they are where its space
    /// went - and then to the workspace's own most-recently-used order, active containers before
    /// hidden ones. `source` is never offered a window back.
    #[must_use]
    fn distribution_recipients(&self, source: &ContainerId) -> Vec<ContainerId> {
        let mut recipients = Vec::new();
        let offer = |id: &ContainerId, recipients: &mut Vec<ContainerId>| {
            if id != source && !recipients.contains(id) {
                recipients.push(id.clone());
            }
        };

        if let Some(SlotDeparture::Absorbed(shift)) = self.plan_departure(source) {
            for mover in &shift.movers {
                offer(&mover.container, &mut recipients);
            }
        } else if let Some(record) = self.hidden_slot_restores.get(source) {
            for absorber in &record.absorbers {
                if self.container_idx_for_id(absorber).is_some() {
                    offer(absorber, &mut recipients);
                }
            }
        }

        // Most recent first, active containers before hidden ones, and finally anything the
        // history has never seen so an empty history cannot refuse a workable operation.
        let by_recency = |active: bool, recipients: &mut Vec<ContainerId>| {
            for id in self.container_focus_history.iter() {
                if let Some(idx) = self.container_idx_for_id(id)
                    && self.containers()[idx].is_active() == active
                {
                    offer(id, recipients);
                }
            }

            for container in self.containers() {
                if container.is_active() == active && !container.is_preselect() {
                    offer(&container.id, recipients);
                }
            }
        };

        by_recency(true, &mut recipients);
        by_recency(false, &mut recipients);

        recipients
    }

    /// Destroy the container at `idx`, sharing out every window it still holds.
    ///
    /// The windows are taken from the top of the source stack downwards and dealt round-robin to
    /// the recipients, each arriving at the bottom of its new stack, so the recipients keep showing
    /// what they were showing. Every window keeps its placement, visibility, presentation and
    /// floating rectangle, and the workspace's minimize history keeps its order, because the
    /// windows have not left the workspace - only the container they belonged to has.
    ///
    /// Focus does not move. Destroying a container changes how many there are, not what the user
    /// is working on, so a container which was not holding the focus leaves the focus where it
    /// was. The one case where something has to happen is the container which *was* holding it:
    /// its focused window is dealt out like every other, and is then raised to the top of whichever
    /// container received it and focused there, so the window the user was working on is still the
    /// window they are working on.
    ///
    /// Refuses without changing anything when the container still holds windows and there is
    /// nowhere to send them, which is the last container of a workspace.
    pub fn destroy_container(&mut self, idx: usize) -> eyre::Result<()> {
        let source = self
            .containers()
            .get(idx)
            .ok_or_eyre("there is no container at that index")?
            .id
            .clone();

        let recipients = self.distribution_recipients(&source);

        if recipients.is_empty() && !self.containers()[idx].windows().is_empty() {
            eyre::bail!("this workspace has nowhere to send the windows of {source}");
        }

        // Nothing above this point has written anything, and nothing below it can fail.
        let expansion = self.expansion_focus_target(idx);

        let focused_container = self
            .focused_container()
            .map(|container| container.id.clone());
        let focus_travels = focused_container.as_ref() == Some(&source);
        let focused_hwnd = self
            .focused_container()
            .and_then(Container::focused_managed_window)
            .map(|window| window.hwnd);

        // Both window histories are about windows, not containers, and these windows are staying
        // in this workspace; removing their container must not silently drop or reorder them.
        let minimize_history = self.minimize_history.clone();
        let window_focus_history = self.window_focus_history.clone();

        let container = self
            .remove_container_by_idx(idx)
            .ok_or_eyre("the container disappeared while it was being destroyed")?;

        // Top of the stack first: the ring holds a stack bottom-up, so the last element is the
        // window the source container was showing.
        let windows: Vec<ManagedWindow> = container.windows().iter().rev().cloned().collect();

        for (position, window) in windows.into_iter().enumerate() {
            let recipient = &recipients[position % recipients.len()];

            if let Some(recipient_idx) = self.container_idx_for_id(recipient)
                && let Some(container) = self.containers_mut().get_mut(recipient_idx)
            {
                container.receive_window_at_bottom(window);
            }
        }

        self.minimize_history = minimize_history;
        self.window_focus_history = window_focus_history;
        self.prune_histories();

        match (focus_travels, focused_hwnd) {
            // The focused window has been dealt to one of the recipients, underneath what that
            // container was showing. It is raised there and focused, which is the one place a
            // recipient's shown window is allowed to change.
            (true, Some(hwnd)) => match self.container_idx_for_window(hwnd) {
                Some(recipient_idx) => {
                    if let Some(recipient) = self.containers_mut().get_mut(recipient_idx) {
                        recipient.raise_window(hwnd);
                        recipient.focus_window_by_hwnd(hwnd);
                        recipient.load_focused_window();
                    }

                    self.focus_container(recipient_idx);
                    self.window_focus_history.record(hwnd);
                }
                None => self.focus_after_removal(expansion),
            },
            // An empty container was holding the focus, so there is no window to follow and the
            // ordinary post-removal selection decides.
            (true, None) => self.focus_after_removal(expansion),
            (false, _) => {
                if let Some(id) = focused_container {
                    self.restore_container_focus(&id);
                }
            }
        }

        Ok(())
    }

    /// Destroy the focused container, sharing out every window it still holds.
    ///
    /// The focus is on this container by definition, so its focused window travels with the focus
    /// into whichever container receives it.
    pub fn destroy_focused_container(&mut self) -> eyre::Result<()> {
        self.destroy_container(self.focused_container_idx())
    }

    /// The container this workspace made most recently.
    ///
    /// A preselect container is an insertion marker rather than a container the user asked for, so
    /// it is never the newest one for this purpose.
    #[must_use]
    fn newest_container_idx(&self) -> Option<usize> {
        self.containers()
            .iter()
            .enumerate()
            .filter(|(_, container)| !container.is_preselect())
            .max_by_key(|(_, container)| container.sequence())
            .map(|(idx, _)| idx)
    }

    /// Destroy the container this workspace made most recently.
    ///
    /// This is the inverse of the manual split: a workspace which has just had a container added
    /// gives that same container back, so the pair of keys can be pressed in either order and
    /// leave the workspace where it started. Which container holds the focus has nothing to do
    /// with it - destroying the focused container is a command of its own.
    pub fn destroy_newest_container(&mut self) -> eyre::Result<()> {
        let idx = self
            .newest_container_idx()
            .ok_or_eyre("this workspace has no container to destroy")?;

        self.destroy_container(idx)
    }

    pub fn preselect_container_idx(&mut self, insertion_idx: usize) {
        self.preselected_container_idx = Some(insertion_idx);
        self.insert_container_at_idx(insertion_idx, Container::preselect());
    }

    pub fn cancel_preselect(&mut self) {
        if let Some(idx) = self.preselected_container_idx {
            self.containers_mut().remove_respecting_locks(idx);
            self.preselected_container_idx = None;
        }
    }

    pub fn new_idx_for_direction(&self, direction: OperationDirection) -> Option<usize> {
        let len = NonZeroUsize::new(self.containers().len())?;

        direction.destination(
            self.layout.as_boxed_direction().as_ref(),
            self.layout_flip,
            self.focused_container_idx(),
            len,
            self.layout_options,
        )
    }

    pub fn new_idx_for_cycle_direction(&self, direction: CycleDirection) -> Option<usize> {
        Option::from(direction.next_idx(
            self.focused_container_idx(),
            NonZeroUsize::new(self.containers().len())?,
        ))
    }

    // this is what we use for stacking
    pub fn move_window_to_container(&mut self, target_container_idx: usize) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_eyre("there is no container")?;

        let window = container
            .remove_focused_window()
            .ok_or_eyre("there is no window")?;

        // This is a little messy
        let adjusted_target_container_index = if container.windows().is_empty() {
            self.remove_container_by_idx(focused_idx);

            if focused_idx < target_container_idx {
                target_container_idx.saturating_sub(1)
            } else {
                target_container_idx
            }
        } else {
            container.load_focused_window();
            target_container_idx
        };

        let target_container = self
            .containers_mut()
            .get_mut(adjusted_target_container_index)
            .ok_or_eyre("there is no container")?;

        target_container.add_managed_window(window);

        self.focus_container(adjusted_target_container_index);
        self.focused_container_mut()
            .ok_or_eyre("there is no container")?
            .load_focused_window();

        Ok(())
    }

    pub fn new_container_for_focused_window(&mut self) -> eyre::Result<()> {
        let focused_container_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_eyre("there is no container")?;

        let window = container
            .remove_focused_window()
            .ok_or_eyre("there is no window")?;

        if container.windows().is_empty() {
            self.remove_container_by_idx(focused_container_idx);
        } else {
            container.load_focused_window();
        }

        self.new_container_for_managed_window(window);
        Ok(())
    }

    /// Return the foreground floating window to the control of its own container.
    ///
    /// The window stays in the container which already owns it, so unfloating no longer creates
    /// a container and no longer moves the window between owners.
    pub fn new_container_for_floating_window(&mut self) -> eyre::Result<()> {
        let hwnd = WindowsApi::foreground_window()
            .ok()
            .filter(|hwnd| self.is_floating_window(*hwnd))
            .or_else(|| self.focused_floating_window().map(|window| window.hwnd))
            .ok_or_eyre("there is no floating window")?;

        self.unfloat_window(hwnd)
    }

    /// Minimize an owned window without changing which container owns it.
    ///
    /// Returns whether the window's visibility actually changed, so a repeated Win32 event
    /// cannot cause a repeated retile. Container focus moves off the minimized window because a
    /// minimized window is not a focus target, but the window keeps its place in the stack and
    /// stays in the container's window count.
    pub fn minimize_window(&mut self, hwnd: isize) -> eyre::Result<bool> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        let changed = container.windows_mut()[window_idx].set_minimized();

        if changed && container.focused_window_idx() == window_idx {
            let successor = container.first_focusable_window().map(|window| window.hwnd);

            if let Some(successor) = successor {
                container.focus_window_by_hwnd(successor);
            }
        }

        self.record_minimized_window(hwnd);

        Ok(changed)
    }

    /// Mark an owned window as no longer minimized.
    ///
    /// This is the reconciliation half of [`Workspace::minimize_window`]: it is what a window
    /// restored from the taskbar goes through, so it neither focuses nor moves anything.
    pub fn unminimize_window(&mut self, hwnd: isize) -> eyre::Result<bool> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        let changed = container.windows_mut()[window_idx].set_visible();

        self.forget_minimized_window(hwnd);

        Ok(changed)
    }

    /// Bring an owned window's recorded presentation into agreement with what Win32 reports.
    ///
    /// A user can restore a maximized window without asking komorebi - the title bar, the system
    /// menu, a keyboard shortcut the application handles itself - and the record has to follow, or
    /// the next retile maximizes the window again over a user who just asked for the opposite.
    ///
    /// Only the presentation changes. The window keeps its container, its stack position, its
    /// placement, its visibility and both histories, so this cannot move a container between
    /// Active and Hidden and does not disturb the arrangement; the caller retiles because the
    /// window now needs to be drawn differently, not because the slots moved.
    pub fn reconcile_window_presentation(
        &mut self,
        hwnd: isize,
        observed: Presentation,
        observed_rect: Option<Rect>,
    ) -> bool {
        let Some(container_idx) = self.container_idx_for_window(hwnd) else {
            return false;
        };

        let Some(container) = self.containers_mut().get_mut(container_idx) else {
            return false;
        };

        let Some(window_idx) = container.idx_for_window(hwnd) else {
            return false;
        };

        container.windows_mut()[window_idx].adopt_presentation(observed, observed_rect)
    }

    /// Raise the window under the top of the focused container's stack and focus it.
    ///
    /// This is the operator's way through a stack which does not disturb its order: the raised
    /// window becomes what its container shows and every other window keeps its relative depth.
    /// Both histories are updated, because the raise goes through the ordinary focus path rather
    /// than around it. `None` means the focused container has nothing under its top window which
    /// could be focused.
    pub fn raise_next_stack_window(&mut self) -> Option<isize> {
        let container_idx = self.focused_container_idx();

        let hwnd = self
            .containers_mut()
            .get_mut(container_idx)?
            .raise_next_stack_window()?;

        self.focus_container_by_window(hwnd).ok()?;

        Some(hwnd)
    }

    /// Restore the most recently minimized window this workspace still owns, and focus it.
    ///
    /// The window returns with the placement and presentation it had, so a floating window comes
    /// back floating and a maximized window comes back maximized. It returns to the top of its
    /// container's stack, because a window which is being restored is a window the user is asking
    /// to see, and its container becomes active again by the ordinary derivation if it was the
    /// only thing keeping it hidden. Both histories are updated through the ordinary focus path.
    pub fn restore_last_minimized_window(&mut self) -> Option<isize> {
        let hwnd = self.take_last_minimized_window()?;

        self.unminimize_window(hwnd).ok()?;

        if let Some(idx) = self.container_idx_for_window(hwnd) {
            self.containers_mut()[idx].raise_window(hwnd);
        }

        self.focus_container_by_window(hwnd).ok()?;

        Some(hwnd)
    }

    /// Whether this workspace owns `hwnd` and it could take focus as it stands.
    ///
    /// A minimized window is owned and remembered, but it cannot be focused without being restored
    /// first, which is a separate decision from choosing what to focus.
    #[must_use]
    pub fn managed_window_is_focusable(&self, hwnd: isize) -> bool {
        self.containers()
            .iter()
            .flat_map(|container| container.windows().iter())
            .any(|window| window.hwnd == hwnd && window.visibility == Visibility::Visible)
    }

    /// Take everything `source` owns into this workspace.
    ///
    /// This is how a workspace is deleted without stranding the windows living on it. Containers
    /// are re-parented rather than rebuilt: each arrives with its stable ID, its stack order, its
    /// window states and its own window focus history intact, which is what keeps a stack, a
    /// hidden container or a floating window's rectangle meaningful on the other side of a merge.
    ///
    /// Both histories are merged with the source's entries first, so the workspace which was just
    /// deleted decides what is focused next, and deduplicated, because a history entry names an
    /// object rather than an event.
    ///
    /// The arrangement is the one thing which cannot come along: these containers are about to be
    /// tiled by a different layout in a different work area. Every exact hidden restore is
    /// invalidated and the manual resize dimensions are discarded, so the next update recalculates
    /// the slots of the active containers alone. Hidden containers stay hidden, because their
    /// state is derived from their windows and none of those have changed.
    pub fn merge_from(&mut self, mut source: Self) {
        // A preselect container is a transient insertion marker whose index means nothing once the
        // ring it indexes has changed, so neither side carries one across the merge.
        source.cancel_preselect();
        self.cancel_preselect();

        // The source's monocle is a claim to show one container alone in a work area which is
        // about to hold this workspace's containers as well; the container arrives like any other.
        source.monocle_container_id = None;

        let inherited = source.focused_container().map(|container| {
            (
                container.id.clone(),
                container.focused_managed_window().map(|window| window.hwnd),
            )
        });

        let container_focus_history = source
            .container_focus_history
            .iter()
            .cloned()
            .chain(self.container_focus_history.iter().cloned())
            .collect::<Mru<_>>();

        let minimize_history = source
            .minimize_history
            .iter()
            .copied()
            .chain(self.minimize_history.iter().copied())
            .collect::<Mru<_>>();

        let window_focus_history = source
            .window_focus_history
            .iter()
            .copied()
            .chain(self.window_focus_history.iter().copied())
            .collect::<Mru<_>>();

        for container in std::mem::take(source.containers_mut()) {
            self.containers_mut().push_back(container);
        }

        self.container_focus_history = container_focus_history;
        self.minimize_history = minimize_history;
        self.window_focus_history = window_focus_history;

        // Manual boundaries describe an arrangement which no longer exists.
        self.resize_dimensions = vec![None; self.containers().len()];
        self.invalidate_slot_geometry();
        self.prune_histories();

        let Some((container, window)) = inherited else {
            return;
        };

        if let Some(hwnd) = window.filter(|hwnd| self.managed_window_is_focusable(*hwnd)) {
            self.record_focused_window(hwnd);
        } else if let Some(idx) = self.container_idx_for_id(&container) {
            self.focus_container(idx);
        }
    }

    /// Fold every container on this workspace into the first one.
    ///
    /// This is the shape komorebi is asked to open in: the desktop enumeration gives each window
    /// it finds a container of its own, and adoption then wants one container holding all of them.
    /// Nothing about a window changes but which container owns it - placement, visibility,
    /// presentation and floating rectangle are the window's own state, not its container's.
    ///
    /// The stack reads the way the ring did. Each container arrives underneath what is already
    /// there and hands its windows over from the top of its own stack downwards, so the first
    /// container's windows end up on top and the order inside every container survives; the
    /// enumeration lists the desktop front to back, which is what makes that the foreground
    /// window's stack position.
    ///
    /// A locked container is left where it is, because its position is a decision the user made
    /// and folding it away would silently discard it. A preselect container is an insertion marker
    /// rather than a place windows live, so it is left alone as well.
    ///
    /// The windows do not leave the workspace, only the containers holding them do, so the
    /// minimize history outlives them. Returns the container everything was folded into.
    pub fn consolidate_containers(&mut self) -> Option<ContainerId> {
        let target_idx = self
            .containers()
            .iter()
            .position(|container| !container.locked && !container.is_preselect())?;

        let target_id = self.containers()[target_idx].id.clone();

        // A container leaving a workspace normally means its windows left too, which is what drops
        // their history entries. Here only the container goes.
        let minimize_history = self.minimize_history.clone();
        let window_focus_history = self.window_focus_history.clone();

        let mut idx = target_idx + 1;
        while idx < self.containers().len() {
            let container = &self.containers()[idx];

            if container.locked || container.is_preselect() {
                idx += 1;
                continue;
            }

            let Some(container) = self.remove_container_by_idx(idx) else {
                break;
            };

            // Resolved again after every removal: the ring re-anchors locked containers when one
            // is taken out, so the target's position is only guaranteed by its identity.
            let Some(target) = self
                .container_idx_for_id(&target_id)
                .and_then(|idx| self.containers_mut().get_mut(idx))
            else {
                break;
            };

            for window in container.windows().iter().rev() {
                target.receive_window_at_bottom(window.clone());
            }
        }

        self.minimize_history = minimize_history;
        self.window_focus_history = window_focus_history;

        // Manual boundaries describe an arrangement of containers which no longer exists.
        self.resize_dimensions = vec![None; self.containers().len()];
        self.invalidate_slot_geometry();

        let target_idx = self.container_idx_for_id(&target_id)?;
        self.focus_container(target_idx);

        if let Some(hwnd) = self
            .containers()
            .get(target_idx)
            .and_then(Container::focused_managed_window)
            .map(|window| window.hwnd)
        {
            self.record_focused_window(hwnd);
        }

        Some(target_id)
    }

    /// Carry every floating rectangle in this workspace from one work area to another.
    ///
    /// A workspace which changes monitor changes coordinate system, and a floating rectangle is
    /// the only rectangle it holds which nothing else will correct: slots are recalculated for the
    /// new work area and manual resize dimensions are discarded, but a floating window is shown
    /// exactly where its own rectangle says. Carrying them is therefore what stops a floating
    /// window being left behind on the monitor its workspace came from.
    pub fn transfer_floating_rects(&mut self, from: Rect, to: Rect) {
        if from == to {
            return;
        }

        for container in self.containers_mut() {
            container.transfer_floating_rects(from, to);
        }
    }

    /// The window a floating geometry command acts on.
    ///
    /// This is the focused window, and only the focused window. It is deliberately not
    /// [`Workspace::focused_floating_window`], whose fallback to the first floating window in
    /// cycle order is what entering the floating layer needs and would here quietly act on a
    /// window the user never selected.
    #[must_use]
    pub fn floating_geometry_subject(&self) -> Option<&ManagedWindow> {
        self.focused_container()
            .and_then(Container::focused_managed_window)
    }

    /// Move the focused floating window, changing nothing else in the workspace.
    pub fn move_focused_floating_window(
        &mut self,
        direction: OperationDirection,
        delta: i32,
        bounds: FloatingBounds,
        observed: Option<Rect>,
    ) -> Result<FloatingGeometryChange, FloatingRejection> {
        self.change_focused_floating_geometry(observed, |rect| {
            plan_move(rect, direction, delta, bounds)
        })
    }

    /// Move one edge of the focused floating window, changing nothing else in the workspace.
    pub fn resize_focused_floating_window(
        &mut self,
        direction: OperationDirection,
        sizing: Sizing,
        delta: i32,
        limits: FloatingLimits,
        observed: Option<Rect>,
    ) -> Result<FloatingGeometryChange, FloatingRejection> {
        self.change_focused_floating_geometry(observed, |rect| {
            plan_edge_resize(rect, direction, sizing, delta, limits)
        })
    }

    /// Plan and record new geometry for the focused floating window.
    ///
    /// Nothing here reads or writes a logical slot, a container's state, a stack, a focus history
    /// or any other window. A floating window's own rectangle is the entire extent of what these
    /// commands own, which is what makes them independent of container movement rather than a
    /// variant of it.
    ///
    /// A rejected subject leaves the workspace exactly as it was: the state is inspected before
    /// anything is written, so there is no half-applied case to undo.
    fn change_focused_floating_geometry(
        &mut self,
        observed: Option<Rect>,
        plan: impl FnOnce(Rect) -> Rect,
    ) -> Result<FloatingGeometryChange, FloatingRejection> {
        let subject = self
            .floating_geometry_subject()
            .ok_or(FloatingRejection::NoSubject)?;

        let hwnd = subject.hwnd;
        let current = subject.floating_geometry(observed)?;
        let planned = plan(current);

        self.set_floating_rect(hwnd, planned);

        Ok(FloatingGeometryChange {
            hwnd,
            rect: planned,
            changed: planned != current,
        })
    }

    /// Record the rectangle Win32 ended up giving a floating window.
    ///
    /// An application is free to refuse a size, and a resize which asked for less than an
    /// application's own minimum comes back larger. Storing what was accepted rather than what
    /// was asked for is what stops the next command from starting off a rectangle the window
    /// never had.
    pub fn confirm_floating_geometry(&mut self, hwnd: isize, accepted: Rect) -> bool {
        self.set_floating_rect(hwnd, accepted)
    }

    /// Record the rectangle a floating window actually occupies.
    pub fn set_floating_rect(&mut self, hwnd: isize, rect: Rect) -> bool {
        for window in self.floating_managed_windows_mut() {
            if window.hwnd == hwnd {
                window.floating_rect = Some(rect);
                return true;
            }
        }

        false
    }

    /// Focus the floating window at `idx` in [`Workspace::floating_windows`] order.
    pub fn focus_floating_window(&mut self, idx: usize) -> Option<Window> {
        let hwnd = self.floating_windows().get(idx).map(|window| window.hwnd)?;

        self.focus_container_by_window(hwnd).ok()?;
        self.floating_windows().get(idx).copied()
    }

    /// The position in the ring of the container with this identity.
    #[must_use]
    pub fn container_idx_for_id(&self, id: &ContainerId) -> Option<usize> {
        self.containers()
            .iter()
            .position(|container| &container.id == id)
    }

    /// Place a newly managed window according to the active container count.
    ///
    /// The rule is a threshold, not a layout: with no active container the window's container takes
    /// the whole work area; with one or two the focused active container's slot is halved and the
    /// window gets a container of its own; from three onwards no container is created and the window
    /// joins an active neighbour instead. Hidden containers are not counted, because they hold no
    /// slot for anything to be split off.
    ///
    /// The split is applied to the slots here rather than left to the layout so that the halves the
    /// user was promised are the halves they get, whatever arrangement the layout would have chosen.
    pub fn place_new_window(&mut self, window: Window) -> NewWindowPlacement {
        // A preselection is an instruction about this particular window, so it outranks the
        // automatic rule.
        if self.preselected_container_idx.is_some() {
            return self.new_container_for_new_window(window);
        }

        match self.active_container_ids().len() {
            0 => self.new_container_for_new_window(window),
            1 | 2 => self.split_for_new_window(window, None),
            _ => self.join_neighbour_for_new_window(window),
        }
    }

    /// Give the window a container of its own and let the arrangement place it.
    fn new_container_for_new_window(&mut self, window: Window) -> NewWindowPlacement {
        self.new_container_for_window(window);

        let id = self
            .focused_container()
            .map_or_else(ContainerId::new, |container| container.id.clone());

        NewWindowPlacement::NewContainer(id)
    }

    /// Halve the geometry-focused active container's slot and give one half to a new container.
    ///
    /// `axis` forces the dividing line; `None` divides the longer edge. Falls back to an ordinary
    /// container creation - and therefore to a full recalculation - when there is no donor slot to
    /// halve, so a refusal here can never leave a container without a slot.
    pub fn split_for_new_window(
        &mut self,
        window: Window,
        axis: Option<SplitAxis>,
    ) -> NewWindowPlacement {
        // The scrolling layout defines its own arrangement from the focused container, so a local
        // edit to its slots would be overwritten by the next recalculation anyway.
        if self.layout_follows_focus() {
            return self.new_container_for_new_window(window);
        }

        let Some(donor_idx) = self.active_container_idx_for_geometry() else {
            return self.new_container_for_new_window(window);
        };

        let Some(donor) = self
            .containers()
            .get(donor_idx)
            .map(|container| container.id.clone())
        else {
            return self.new_container_for_new_window(window);
        };

        let mut container = Container::default();
        let created = container.id.clone();

        // Planned before anything is inserted, so a slot which cannot be halved costs nothing but
        // the fallback path.
        let Some(split) = self.plan_authoritative_split(&donor, &created, axis) else {
            return self.new_container_for_new_window(window);
        };

        container.add_window(window);

        // Container order is what the layout would arrange, so the new container is inserted where
        // its half actually is: a left/right split puts it on the left, before the donor, and a
        // top/bottom split puts it below, after it.
        let insertion_idx = match split.axis {
            SplitAxis::LeftRight => donor_idx,
            SplitAxis::TopBottom => donor_idx + 1,
        };

        self.insert_container_at_idx(insertion_idx, container);
        self.logical_slots.apply_split(&split);
        self.adopt_slot_geometry();

        NewWindowPlacement::Split {
            created,
            donor,
            axis: split.axis,
        }
    }

    /// Add the window to the top of an active neighbour's stack.
    ///
    /// The neighbour is chosen from the slots in left, right, up, down order, so the same
    /// arrangement always sends a new window to the same container.
    fn join_neighbour_for_new_window(&mut self, window: Window) -> NewWindowPlacement {
        let Some(focused_idx) = self.active_container_idx_for_geometry() else {
            return self.new_container_for_new_window(window);
        };

        let Some(focused) = self
            .containers()
            .get(focused_idx)
            .map(|container| container.id.clone())
        else {
            return self.new_container_for_new_window(window);
        };

        let target = self
            .logical_slots
            .adjacent_neighbour(&focused)
            .unwrap_or_else(|| {
                // Three or more active containers tile the work area, so at least one of them
                // borders this one. Reaching this arm means the slots and the containers disagree.
                tracing::warn!(
                    "container {focused} has no adjacent active container; the new window joins it instead"
                );

                focused.clone()
            });

        let Some(target_idx) = self.container_idx_for_id(&target) else {
            return self.new_container_for_new_window(window);
        };

        let Some(container) = self.containers_mut().get_mut(target_idx) else {
            return self.new_container_for_new_window(window);
        };

        // The top of the stack is the focused window of the container, which is what `add_window`
        // makes the added window.
        container.add_window(window);
        self.focus_container(target_idx);

        NewWindowPlacement::Joined(target)
    }

    /// The container whose slot a manual split divides.
    ///
    /// The largest active slot, because halving the biggest rectangle is what keeps an arrangement
    /// even, and the most recently created container when two slots are exactly the same size, so
    /// repeated splits walk across the workspace instead of cutting the same half forever. Only
    /// active containers qualify: a hidden container has no slot to divide.
    ///
    /// This is a question about geometry alone. The window the split moves comes from the focus
    /// history and may belong to a completely different container.
    #[must_use]
    fn split_donor_idx(&self) -> Option<usize> {
        let largest = self
            .containers()
            .iter()
            .enumerate()
            .filter(|(_, container)| container.is_active() && !container.is_preselect())
            .filter_map(|(idx, container)| {
                self.logical_slots
                    .get(&container.id)
                    .map(|slot| (idx, slot.area(), container.sequence()))
            })
            .max_by_key(|(_, area, sequence)| (*area, *sequence))
            .map(|(idx, _, _)| idx);

        // A workspace which has not been arranged yet has no slots to compare. The split cannot be
        // planned against them either, so the operation is going to fall back to a rearrangement
        // and this only decides where the created container is inserted.
        largest.or_else(|| self.active_container_idx_for_geometry())
    }

    /// The window a manual split moves into the container it creates.
    ///
    /// The second most recent window in this workspace's focus history, and then the next, until
    /// one is found which is not the window its own container is currently showing. A container
    /// shows one window at a time, so moving the shown one would change what two containers
    /// display for a command which was only asked to change how many there are.
    ///
    /// The most recent window is skipped outright: it is the focused window, which is shown by
    /// definition, and skipping it is what makes this "the second most recent" rather than "the
    /// first eligible".
    ///
    /// The history does not always cover the workspace - straight after a restart, an import or an
    /// adoption, no window has been focused yet - so the stack answers when it cannot: the first
    /// window below the top of the container holding the most windows.
    ///
    /// `None` means every window in the workspace is the one its container is showing, which is
    /// exactly the case where there are already as many containers as there are windows.
    #[must_use]
    fn split_window_hwnd(&self) -> Option<isize> {
        let shown = self
            .containers()
            .iter()
            .filter_map(Container::focused_managed_window)
            .map(|window| window.hwnd)
            .collect::<Vec<_>>();

        let movable =
            |hwnd: isize| !shown.contains(&hwnd) && self.container_idx_for_window(hwnd).is_some();

        if let Some(hwnd) = self
            .window_focus_history
            .iter()
            .skip(1)
            .copied()
            .find(|hwnd| movable(*hwnd))
        {
            return Some(hwnd);
        }

        self.containers()
            .iter()
            .filter(|container| !container.is_preselect())
            .max_by_key(|container| (container.windows().len(), container.sequence()))
            .and_then(|container| {
                let shown = container.focused_managed_window().map(|window| window.hwnd);

                container
                    .windows()
                    .iter()
                    .rev()
                    .map(|window| window.hwnd)
                    .find(|hwnd| Some(*hwnd) != shown)
            })
    }

    /// Put the ring focus back on `id` without recording a focus which did not happen.
    ///
    /// Changing how many containers a workspace has is not a focus change, but inserting and
    /// removing containers moves the ring's focused index and writes to the container history on
    /// the way. This puts both back.
    fn restore_container_focus(&mut self, id: &ContainerId) {
        if let Some(idx) = self.container_idx_for_id(id) {
            self.containers.focus(idx);
        }
    }

    /// Split a new container off the largest active slot, moving one window into it.
    ///
    /// `axis` forces the dividing line; `None` divides the divided slot's longer edge. Two
    /// different containers are involved and they are chosen for different reasons: the slot comes
    /// from the largest active container, and the window comes from wherever this workspace's
    /// focus history says it can be taken from without changing what a container is showing. They
    /// are frequently the same container and nothing depends on that.
    ///
    /// The moved window keeps its placement, visibility, presentation and floating rectangle, so a
    /// container which receives a floating or minimized window is created hidden and gives its half
    /// straight back on the next reconciliation. That is the ordinary hidden transition, not a
    /// special case here. The container the window came from cannot be emptied by this, because the
    /// window taken is never the only one it has.
    ///
    /// Focus does not move. Adding a container is a change to how many there are, not to what the
    /// user is working on, so the created container is shown but not focused and takes the oldest
    /// place in the container history rather than the newest.
    ///
    /// Everything which can refuse the operation is decided before anything is written, so a
    /// refusal leaves the workspace exactly as it was.
    pub fn create_container_from_donor(
        &mut self,
        axis: Option<SplitAxis>,
    ) -> eyre::Result<ContainerId> {
        let hwnd = self
            .split_window_hwnd()
            .ok_or_eyre("every window in this workspace is the one its container is showing")?;

        let source_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("the window to be moved belongs to no container")?;

        let window_idx = self
            .containers()
            .get(source_idx)
            .and_then(|container| container.idx_for_window(hwnd))
            .ok_or_eyre("the window to be moved disappeared from its container")?;

        let donor_idx = self
            .split_donor_idx()
            .ok_or_eyre("this workspace has no active container to divide")?;

        let donor = self.containers()[donor_idx].id.clone();
        let focused_before = self
            .focused_container()
            .map(|container| container.id.clone());

        let mut container = Container::default();
        let created = container.id.clone();

        // The scrolling layout arranges from the focused container, so a local edit to its slots
        // would not survive its next recalculation; it gets the rearrangement instead.
        let split = if self.layout_follows_focus() {
            None
        } else {
            self.plan_authoritative_split(&donor, &created, axis)
        };

        if split.is_none() {
            tracing::warn!(
                "the slot of {donor} cannot be divided as asked; the workspace will be rearranged instead"
            );
        }

        let window = self
            .containers_mut()
            .get_mut(source_idx)
            .and_then(|source| source.remove_window_by_idx(window_idx))
            .ok_or_eyre("the window disappeared while it was being moved")?;

        // A stack only ever shows the window on top of it, so everything below the window which
        // has just been taken away has been hidden all along. The window moved is never the one
        // its container was showing, so this is a guarantee rather than a correction.
        if let Some(source) = self.containers_mut().get_mut(source_idx) {
            source.load_focused_window();
        }

        container.add_managed_window(window);

        // The window which was pulled out of the stack was a hidden member of it, and the container
        // it has landed in is the only thing which knows it is now the window being shown.
        container.load_focused_window();

        // The created container is inserted where its half actually is, and after the divided
        // container when there is no half because the arrangement is about to be recalculated.
        let insertion_idx = match split.as_ref().map(|split| split.axis) {
            Some(SplitAxis::LeftRight) => donor_idx,
            Some(SplitAxis::TopBottom) | None => donor_idx + 1,
        };

        self.insert_container_at_idx(insertion_idx, container);

        // Inserting focused the created container and recorded it as the most recently used one.
        // Neither happened: a container was added, and the user goes on working where they were.
        self.container_focus_history.remove(&created);
        self.container_focus_history.record_oldest(created.clone());

        if let Some(id) = focused_before {
            self.restore_container_focus(&id);
        }

        if let Some(split) = split {
            self.logical_slots.apply_split(&split);
            self.adopt_slot_geometry();
        }

        Ok(created)
    }

    /// Move the boundary on one side of an active container.
    ///
    /// A positive `delta` grows the container on that side. The move is a boundary move, not a
    /// container move: every active container touching the same line changes with it, on both
    /// sides, which is what stops one container growing into a hole or over a neighbour. Only the
    /// axis the boundary belongs to changes, so a left or right resize can never alter a slot's
    /// vertical extent.
    ///
    /// A delta larger than the space available is clamped rather than refused, so holding down a
    /// resize key settles against the minimum instead of stopping working. Refusals are for
    /// boundaries which cannot move at all: the work area's own edge, a target with no slot, and
    /// two sides which do not line up into one clean line.
    pub fn resize_container(
        &mut self,
        id: &ContainerId,
        direction: OperationDirection,
        delta: i32,
    ) -> eyre::Result<SlotResize> {
        // Editing slots which are not the arrangement would be adopting an arrangement this
        // workspace is not in; the pending recalculation would discard the edit anyway.
        if !self.slots_are_authoritative() {
            eyre::bail!("this workspace has to be laid out again before it can be resized");
        }

        let resize = self
            .logical_slots
            .plan_edge_resize(id, direction, delta)
            .ok_or_eyre("that boundary cannot be moved")?;

        let changed: Vec<ContainerId> = resize
            .movers
            .iter()
            .map(|mover| mover.container.clone())
            .collect();

        self.logical_slots.apply_edge_resize(&resize);

        // A hidden container's restore record promises its absorbers still hold exactly what the
        // absorption gave them. Moving a boundary they touch breaks that promise.
        self.invalidate_restores_touching(&changed);
        self.adopt_slot_geometry();

        Ok(resize)
    }

    /// Move the boundary on one side of the container geometry operations start from.
    ///
    /// A hidden container has no boundary to move, so when the focus is on one - a floating window
    /// of an otherwise hidden container, for instance - the workspace's most recent active
    /// container is used instead, exactly as it is for splitting.
    pub fn resize_focused_container(
        &mut self,
        direction: OperationDirection,
        delta: i32,
    ) -> eyre::Result<SlotResize> {
        let idx = self
            .active_container_idx_for_geometry()
            .ok_or_eyre("this workspace has no active container to resize")?;

        let id = self.containers()[idx].id.clone();

        self.resize_container(&id, direction, delta)
    }

    /// Adopt the slots as they are now as the arrangement this workspace is in.
    ///
    /// A local edit changes the slots deliberately. Without this the next reconciliation would see
    /// inputs it has never arranged - a container list with one more entry in it - and recalculate
    /// the whole workspace, discarding exactly the edit that was just made.
    fn adopt_slot_geometry(&mut self) {
        self.relayout_pending = false;
        self.slot_inputs = Some(self.slot_inputs());
    }

    pub fn new_container_for_window(&mut self, window: Window) {
        let next_idx = if let Some(idx) = self.preselected_container_idx {
            let next = idx;
            self.preselected_container_idx = None;
            self.remove_container_by_idx(next);
            next
        } else if self.containers().is_empty() {
            0
        } else {
            self.resolve_placement_index(&window)
        };

        let mut container = Container::default();
        container.add_window(window);

        self.insert_container_at_idx(next_idx, container);
    }

    fn new_container_for_managed_window(&mut self, window: ManagedWindow) {
        let next_idx = if let Some(idx) = self.preselected_container_idx {
            let next = idx;
            self.preselected_container_idx = None;
            self.remove_container_by_idx(next);
            next
        } else if self.containers().is_empty() {
            0
        } else {
            self.resolve_placement_index(&window.window)
        };

        let mut container = Container::default();
        container.add_managed_window(window);
        self.insert_container_at_idx(next_idx, container);
    }

    /// Resolves the container index at which a new window should be placed,
    /// based on the `initial_window_placement_rules` configuration.
    ///
    /// Falls back to the default placement (currently `AfterFocused`,
    /// i.e. `focused_container_idx() + 1`) when:
    /// - No rules are configured
    /// - A `Rules` map has no matching rule for the window
    /// - A resolved index is out of bounds
    fn resolve_placement_index(&self, window: &Window) -> usize {
        let fallback_idx = self.focused_container_idx() + 1;

        let Some(rules) = &self.initial_window_placement_rules else {
            return fallback_idx;
        };

        match rules {
            InitialWindowPlacementRules::Target(target) => {
                self.resolve_placement_target(target, fallback_idx)
            }
            InitialWindowPlacementRules::Rules(rules_map) => {
                let Ok(title) = window.title() else {
                    return fallback_idx;
                };
                let Ok(exe_name) = window.exe() else {
                    return fallback_idx;
                };
                let Ok(class) = window.class() else {
                    return fallback_idx;
                };
                let Ok(path) = window.path() else {
                    return fallback_idx;
                };

                let regex_identifiers = REGEX_IDENTIFIERS.lock().clone();

                // BTreeMap iterates in key order
                for (target, placement_rules) in rules_map {
                    let matched = match placement_rules {
                        PlacementMatchingRules::Single(id) => should_act_individual(
                            &title,
                            &exe_name,
                            &class,
                            &path,
                            id,
                            &regex_identifiers,
                        ),
                        PlacementMatchingRules::Many(rules) => {
                            // OR logic: any matching rule triggers placement at this target.
                            // Each MatchingRule handles its own Simple/Composite (AND) logic
                            // internally via should_act.
                            should_act(&title, &exe_name, &class, &path, rules, &regex_identifiers)
                                .is_some()
                        }
                    };

                    if matched {
                        return self.resolve_placement_target(target, fallback_idx);
                    }
                }

                // No matching rule found
                fallback_idx
            }
        }
    }

    /// Resolves a `WindowPlacement` variant to a concrete container index.
    fn resolve_window_placement(&self, placement: &WindowPlacement, fallback_idx: usize) -> usize {
        match placement {
            WindowPlacement::AfterFocused => self.focused_container_idx() + 1,
            WindowPlacement::BeforeFocused => self.focused_container_idx(),
            WindowPlacement::Primary => {
                let idx = self.layout.primary_index();
                if idx <= self.containers().len() {
                    idx
                } else {
                    fallback_idx
                }
            }
            WindowPlacement::Secondary => {
                if let Some(idx) = self.layout.secondary_index(self.containers().len()) {
                    if idx <= self.containers().len() {
                        idx
                    } else {
                        fallback_idx
                    }
                } else {
                    fallback_idx
                }
            }
            WindowPlacement::Last => self.containers().len(),
        }
    }

    /// Resolves a `PlacementTarget` (either a named placement or a 1-based index) to a concrete container index.
    fn resolve_placement_target(&self, target: &PlacementTarget, fallback_idx: usize) -> usize {
        match target {
            PlacementTarget::Placement(placement) => {
                self.resolve_window_placement(placement, fallback_idx)
            }
            PlacementTarget::Index(idx) => {
                // Config indices are 1-based; convert to 0-based
                let zero_based = idx.saturating_sub(1);
                if zero_based <= self.containers().len() {
                    zero_based
                } else {
                    fallback_idx
                }
            }
        }
    }

    /// Float the focused window, keeping it in the container which owns it.
    ///
    /// A presented window is returned to Normal first, and a window held by the transitional
    /// monocle path is reintegrated first, so floating leaves ownership where the model requires
    /// it. Nothing is removed from a container here, which is why an emptied container can no
    /// longer appear as a side effect of floating a window.
    pub fn new_floating_window(&mut self) -> eyre::Result<()> {
        if let Some(presented) = self.presented_window() {
            self.leave_presentation(presented.hwnd)?;
        } else if self.is_monocle() {
            self.reintegrate_monocle_container()?;
        }

        let hwnd = self
            .focused_container()
            .and_then(Container::focused_window)
            .ok_or_eyre("there is no window")?
            .hwnd;

        let current_rect = WindowsApi::window_rect(hwnd).unwrap_or_default();

        self.float_window(hwnd, current_rect)
    }

    /// Return the focused floating window to its container's control.
    pub fn unfloat_focused_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .focused_floating_window()
            .ok_or_eyre("there is no floating window")?
            .hwnd;

        self.unfloat_window(hwnd)
    }

    fn enforce_resize_constraints(&mut self) {
        match self.layout {
            Layout::Default(DefaultLayout::BSP) => self.enforce_resize_constraints_for_bsp(),
            Layout::Default(DefaultLayout::Columns) => self.enforce_resize_for_columns(),
            Layout::Default(DefaultLayout::Rows) => self.enforce_resize_for_rows(),
            Layout::Default(DefaultLayout::VerticalStack) => {
                self.enforce_resize_for_vertical_stack();
            }
            Layout::Default(DefaultLayout::RightMainVerticalStack) => {
                self.enforce_resize_for_right_vertical_stack();
            }
            Layout::Default(DefaultLayout::HorizontalStack) => {
                self.enforce_resize_for_horizontal_stack();
            }
            Layout::Default(DefaultLayout::UltrawideVerticalStack) => {
                self.enforce_resize_for_ultrawide();
            }
            Layout::Default(DefaultLayout::Scrolling) => {
                self.enforce_resize_for_scrolling();
            }
            _ => self.enforce_no_resize(),
        }
    }

    fn enforce_resize_constraints_for_bsp(&mut self) {
        for (i, rect) in self.resize_dimensions.iter_mut().enumerate() {
            if let Some(rect) = rect {
                // Even containers can't be resized to the bottom
                if i % 2 == 0 {
                    rect.bottom = 0;
                    // Odd containers can't be resized to the right
                } else {
                    rect.right = 0;
                }
            }
        }

        // The first container can never be resized to the left or the top
        if let Some(Some(first)) = self.resize_dimensions.first_mut() {
            first.top = 0;
            first.left = 0;
        }

        // The last container can never be resized to the bottom or the right
        if let Some(Some(last)) = self.resize_dimensions.last_mut() {
            last.bottom = 0;
            last.right = 0;
        }
    }

    fn enforce_resize_for_columns(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();
                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.top = 0;
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        }
                        if i == len - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_rows(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();
                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.left = 0;
                        rect.right = 0;

                        if i == 0 {
                            rect.top = 0;
                        }
                        if i == len - 1 {
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_vertical_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            _ => {
                // Zero is actually on the left
                if let Some(left) = resize_dimensions[0].as_mut() {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the right
                        rect.right = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_right_vertical_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            _ => {
                // Zero is actually on the right
                if let Some(left) = resize_dimensions[1].as_mut() {
                    left.top = 0;
                    left.bottom = 0;
                    left.right = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the left
                        rect.left = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_horizontal_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                if let Some(left) = resize_dimensions[0].as_mut() {
                    left.top = 0;
                    left.left = 0;
                    left.right = 0;
                }

                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        }
                        if i == stack_size - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_ultrawide(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            // Two windows can only be resized in the middle
            2 => {
                // Zero is actually on the right
                if let Some(right) = resize_dimensions[0].as_mut() {
                    right.top = 0;
                    right.bottom = 0;
                    right.right = 0;
                }

                // One is on the left
                if let Some(left) = resize_dimensions[1].as_mut() {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }
            }
            // Three or more windows means 0 is in center, 1 is at the left, 2.. are a vertical
            // stack on the right
            _ => {
                // Central can be resized left or right
                if let Some(right) = resize_dimensions[0].as_mut() {
                    right.top = 0;
                    right.bottom = 0;
                }

                // Left one can only be resized to the right
                if let Some(left) = resize_dimensions[1].as_mut() {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[2..].len();
                for (i, rect) in resize_dimensions[2..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the right
                        rect.right = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_scrolling(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();

                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.top = 0;
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        } else if i == len - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }
    fn enforce_no_resize(&mut self) {
        for rect in self.resize_dimensions.iter_mut().flatten() {
            rect.left = 0;
            rect.right = 0;
            rect.top = 0;
            rect.bottom = 0;
        }
    }

    /// The container this workspace shows alone, if monocle mode is on.
    ///
    /// Monocle is a reference to a container which is still in the ring, so this cannot hand back
    /// a container the workspace does not own; a stale reference resolves to `None` and the
    /// workspace simply is not in monocle mode.
    #[must_use]
    pub fn monocle_container(&self) -> Option<&Container> {
        self.monocle_container_idx()
            .and_then(|idx| self.containers().get(idx))
    }

    pub fn monocle_container_mut(&mut self) -> Option<&mut Container> {
        self.monocle_container_idx()
            .and_then(|idx| self.containers_mut().get_mut(idx))
    }

    /// The position in the ring of the container monocle currently shows.
    #[must_use]
    pub fn monocle_container_idx(&self) -> Option<usize> {
        let id = self.monocle_container_id.as_ref()?;

        self.containers()
            .iter()
            .position(|container| &container.id == id)
    }

    #[must_use]
    pub fn is_monocle(&self) -> bool {
        self.monocle_container_idx().is_some()
    }

    /// Give the monocle container the whole work area and take every other slot away.
    fn record_monocle_slot(&mut self, idx: usize, available_area: Rect) {
        let Some(container) = self.containers().get(idx) else {
            return;
        };

        let id = container.id.clone();
        let area = LogicalRect::from(available_area);

        self.logical_work_area = Some(area);
        self.logical_slots.replace_all([(id, area)]);
    }

    /// Show the focused container alone, without taking it out of the ring.
    ///
    /// The container keeps its stable ID, its place in the ring, its stack and both histories.
    /// Only the slot arrangement changes, which is why a monocle toggle can no longer lose a
    /// container's identity or its resize adjustment.
    pub fn new_monocle_container(&mut self) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();

        let container = self
            .containers()
            .get(focused_idx)
            .ok_or_eyre("there is no container")?;

        if container.is_preselect() {
            bail!("a preselect marker cannot be shown as a monocle container");
        }

        self.monocle_container_id = Some(container.id.clone());
        self.focus_container(focused_idx);

        self.containers_mut()
            .get_mut(focused_idx)
            .ok_or_eyre("there is no container")?
            .load_focused_window();

        Ok(())
    }

    /// Return the workspace to the ordinary arrangement.
    pub fn reintegrate_monocle_container(&mut self) -> eyre::Result<()> {
        let restore_idx = self
            .monocle_container_idx()
            .ok_or_eyre("there is no monocle container")?;

        self.monocle_container_id = None;
        self.focus_container(restore_idx);

        self.containers_mut()
            .get_mut(restore_idx)
            .ok_or_eyre("there is no container")?
            .load_focused_window();

        Ok(())
    }

    /// Take every container except the monocle container off the screen.
    ///
    /// The monocle container is still in the ring, so it must be excluded here; hiding every
    /// container would hide the one the workspace is meant to be showing.
    pub fn hide_containers_around_monocle(&mut self) {
        let Some(monocle_idx) = self.monocle_container_idx() else {
            return;
        };

        for (idx, container) in self.containers().iter().enumerate() {
            if idx != monocle_idx {
                // A container hides every window it owns, floating ones included.
                container.hide(None);
            }
        }
    }

    pub fn cycle_monocle_container(&mut self, direction: CycleDirection) -> eyre::Result<()> {
        if self.containers().is_empty() {
            return Ok(());
        }

        self.reintegrate_monocle_container()?;

        let new_idx = self
            .new_idx_for_cycle_direction(direction)
            .ok_or_eyre("there is no container to cycle monocle to")?;

        self.focus_container(new_idx);
        self.new_monocle_container()?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_container(&mut self, idx: usize) {
        tracing::info!("focusing container");

        // The scrolling layout arranges around the focused container, so for it alone a focus
        // change is an arrangement change. Every other layout must be able to move focus without
        // discarding a hidden container's exact restore.
        if self.layout_follows_focus() && self.containers.focused_idx() != idx {
            self.invalidate_slot_geometry();
        }

        self.containers.focus(idx);

        // A preselect container is a transient insertion marker with a fixed ID, so it must never
        // enter a history whose entries are stable identities.
        if let Some(container) = self.containers.elements().get(idx)
            && !container.is_preselect()
        {
            let id = container.id.clone();
            self.container_focus_history.record(id);
        }
    }

    /// Record a window focus at both history levels and move the ring focus to match.
    ///
    /// This is the single entry point used when a managed window gains focus, so a container
    /// MRU update can never happen without the corresponding workspace MRU update.
    pub fn record_focused_window(&mut self, hwnd: isize) -> bool {
        let Some(container_idx) = self.container_idx_for_window(hwnd) else {
            return false;
        };

        let focused = self
            .containers_mut()
            .get_mut(container_idx)
            .is_some_and(|container| container.focus_window_by_hwnd(hwnd));

        if focused {
            self.focus_container(container_idx);
            self.window_focus_history.record(hwnd);
        }

        focused
    }

    /// The container and window to focus when this workspace is shown again.
    ///
    /// The most recent container with a focusable window wins, and inside it the most recent
    /// focusable window wins. Minimized windows are never selected.
    pub fn focus_target_from_history(&self) -> Option<(usize, isize)> {
        for id in self.container_focus_history.iter() {
            if let Some(idx) = self
                .containers()
                .iter()
                .position(|container| &container.id == id)
                && let Some(window) = self.containers()[idx].first_focusable_window()
            {
                return Some((idx, window.hwnd));
            }
        }

        // Containers which never took focus are still valid targets after a restart or an import.
        self.containers()
            .iter()
            .enumerate()
            .find_map(|(idx, container)| {
                container
                    .first_focusable_window()
                    .map(|window| (idx, window.hwnd))
            })
    }

    /// Record that `hwnd` was minimized, as the most recent entry of this workspace's history.
    pub fn record_minimized_window(&mut self, hwnd: isize) {
        self.minimize_history.record(hwnd);
    }

    /// Drop `hwnd` from the minimize history, whatever caused it to stop being minimized.
    pub fn forget_minimized_window(&mut self, hwnd: isize) -> bool {
        self.minimize_history.remove(&hwnd)
    }

    /// Take the most recently minimized window which this workspace still owns.
    ///
    /// Stale entries examined on the way are discarded, so a history full of closed windows
    /// leaves an empty history and no side effect.
    pub fn take_last_minimized_window(&mut self) -> Option<isize> {
        let minimized = self
            .containers()
            .iter()
            .flat_map(|container| container.windows().iter())
            .filter(|window| window.visibility == Visibility::Minimized)
            .map(|window| window.hwnd)
            .collect::<Vec<_>>();

        self.minimize_history
            .take_first_valid(|hwnd| minimized.contains(hwnd))
    }

    /// Drop every history entry which no longer resolves to a container or window of this
    /// workspace, and repair each container's own window history.
    ///
    /// Runtime removal paths already prune what they remove. This exists for state which was
    /// deserialized or assembled elsewhere.
    pub fn prune_histories(&mut self) {
        let ids = self
            .containers()
            .iter()
            .filter(|container| !container.is_preselect())
            .map(|container| container.id.clone())
            .collect::<Vec<_>>();

        self.container_focus_history.retain(|id| ids.contains(id));

        for container in self.containers_mut() {
            container.repair_focus_history();
        }

        let hwnds = self
            .containers()
            .iter()
            .flat_map(|container| container.windows().iter())
            .map(|window| window.hwnd)
            .collect::<Vec<_>>();

        self.minimize_history.retain(|hwnd| hwnds.contains(hwnd));
        self.window_focus_history
            .retain(|hwnd| hwnds.contains(hwnd));
        self.prune_monocle_reference();
    }

    /// Drop a monocle reference to a container this workspace no longer owns.
    ///
    /// Containers can also leave through a bulk retain rather than through the removal path which
    /// forgets them one by one, so the reference is pruned wherever the ring is rebuilt.
    pub fn prune_monocle_reference(&mut self) {
        if self.monocle_container_id.is_some() && self.monocle_container_idx().is_none() {
            self.monocle_container_id = None;
        }
    }

    pub fn swap_containers(&mut self, i: usize, j: usize) {
        self.containers.elements_mut().swap_respecting_locks(i, j);
        self.focus_container(j);
    }

    /// Detach the foreground window from this workspace if it is one of its floating windows.
    pub fn remove_focused_floating_window(&mut self) -> Option<Window> {
        let hwnd = WindowsApi::foreground_window().ok()?;

        let window = self
            .floating_managed_windows()
            .find(|window| window.hwnd == hwnd)
            .map(|window| window.window)?;

        self.detach_window(hwnd).ok()?;

        Some(window)
    }

    /// The windows this workspace currently has on screen.
    ///
    /// Monocle shows one container and hides the rest, so it is the whole answer while it is on
    /// rather than an extra entry in front of the ordinary arrangement.
    pub fn visible_windows(&self) -> Vec<Option<&Window>> {
        let mut vec = vec![];

        if let Some(monocle) = self.monocle_container() {
            vec.push(monocle.focused_window());
            return vec;
        }

        for container in self.containers() {
            vec.push(container.focused_visible_stored_window());

            for window in container.visible_floating_windows() {
                vec.push(Some(&window.window));
            }
        }

        vec
    }

    pub fn visible_window_details(&self) -> Vec<WindowDetails> {
        let mut vec: Vec<WindowDetails> = vec![];

        if let Some(monocle) = self.monocle_container() {
            if let Some(focused) = monocle.focused_window()
                && let Ok(details) = (*focused).try_into()
            {
                vec.push(details);
            }

            return vec;
        }

        for container in self.containers() {
            if let Some(focused) = container.focused_window()
                && let Ok(details) = (*focused).try_into()
            {
                vec.push(details);
            }
        }

        for window in self.floating_managed_windows() {
            if let Ok(details) = window.window.try_into() {
                vec.push(details);
            }
        }

        vec
    }

    pub fn focus_previous_container(&mut self) {
        let focused_idx = self.focused_container_idx();
        self.focus_container(focused_idx.saturating_sub(1));
    }

    fn focus_last_container(&mut self) {
        self.focus_container(self.containers().len().saturating_sub(1));
    }

    fn focus_first_container(&mut self) {
        self.focus_container(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Window;
    use crate::container::Container;
    use crate::floating_geometry;
    use crate::geometry::SlotOrder;
    use crate::managed_window::Presentation;
    use std::collections::HashMap;

    #[test]
    fn legacy_workspace_json_gets_a_new_stable_id() {
        let workspace = Workspace::default();
        let mut json = serde_json::to_value(&workspace).unwrap();
        json.as_object_mut().unwrap().remove("id");

        let migrated: Workspace = serde_json::from_value(json).unwrap();

        assert!(!migrated.id.is_empty());
        assert_ne!(migrated.id, workspace.id);
    }

    #[test]
    fn workspace_id_survives_serialization_and_order_changes() {
        let first = Workspace::default();
        let second = Workspace::default();
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        let mut workspaces = VecDeque::from([first, second]);

        workspaces.swap(0, 1);
        assert_eq!(workspaces[0].id, second_id);
        assert_eq!(workspaces[1].id, first_id);

        let roundtrip: Workspace =
            serde_json::from_str(&serde_json::to_string(&workspaces[1]).unwrap()).unwrap();
        assert_eq!(roundtrip.id, first_id);
    }

    fn work_area(width: i32, height: i32) -> Rect {
        Rect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    fn legacy_render_layout(
        workspace: &Workspace,
        area: Rect,
        container_padding: i32,
    ) -> Vec<Rect> {
        // How the arrangement was called before logical slots existed: the container gap was an
        // input to the layout calculation itself.
        workspace.layout.as_boxed_arrangement().calculate(
            &area,
            NonZeroUsize::new(workspace.containers().len()).unwrap(),
            Some(container_padding),
            workspace.layout_flip,
            &workspace.resize_dimensions,
            workspace.focused_container_idx(),
            workspace.effective_layout_options(),
            &workspace.latest_layout,
        )
    }

    #[test]
    fn logical_slots_tile_the_available_area_exactly() {
        for count in 1..=5 {
            for area in [work_area(1920, 1080), work_area(1001, 777)] {
                let mut workspace = workspace_with_containers(&vec![1; count]);
                workspace.record_logical_slots(area);

                assert_eq!(workspace.logical_slots.len(), count);
                assert_eq!(
                    workspace
                        .logical_slots
                        .validate_coverage(LogicalRect::from(area)),
                    Ok(()),
                    "{count} containers did not tile {area:?}"
                );
            }
        }
    }

    #[test]
    fn the_container_gap_does_not_change_the_logical_slots() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);

        workspace.record_logical_slots(area);
        let slots = workspace.logical_slots.clone();

        // The gap is not an input to slot calculation at all, so no matter which gap the renderer
        // is about to apply, the slots - and therefore adjacency and coverage - are identical.
        for container_padding in [0, 5, 40] {
            let rendered = workspace
                .logical_slot_at(0)
                .unwrap()
                .to_render_rect(RenderInsets {
                    container_padding,
                    ..RenderInsets::default()
                });

            workspace.record_logical_slots(area);

            // The generation advances on every recalculation, but the geometry does not.
            assert_eq!(
                workspace.logical_slots.ordered(SlotOrder::TopToBottom),
                slots.ordered(SlotOrder::TopToBottom)
            );
            assert_eq!(
                rendered.right,
                slots.get(&workspace.containers()[0].id).unwrap().width - container_padding * 2
            );
        }
    }

    #[test]
    fn rendering_a_logical_slot_reproduces_the_previous_layout_geometry() {
        for count in 1..=4 {
            for container_padding in [0, 10] {
                let workspace = workspace_with_containers(&vec![1; count]);
                let area = work_area(1920, 1080);

                let expected = legacy_render_layout(&workspace, area, container_padding);
                let logical = workspace.calculate_logical_slots(area);

                assert_eq!(logical.len(), expected.len());

                for ((_, slot), expected) in logical.iter().zip(expected.iter()) {
                    let rendered = slot.to_render_rect(RenderInsets {
                        container_padding,
                        ..RenderInsets::default()
                    });

                    assert_eq!(rendered, *expected);
                }
            }
        }
    }

    fn floating_rect(left: i32) -> Rect {
        Rect {
            left,
            top: 10,
            right: 400,
            bottom: 300,
        }
    }

    #[test]
    fn a_floating_window_keeps_the_container_which_owns_it() {
        let mut workspace = workspace_with_containers(&[3]);
        let container_id = workspace.containers()[0].id.clone();

        workspace.float_window(1, floating_rect(0)).unwrap();

        let container = &workspace.containers()[0];
        assert_eq!(container.id, container_id);
        assert_eq!(container.windows().len(), 3);
        // Stack order is untouched: floating is a placement, not a move.
        assert_eq!(
            container
                .windows()
                .iter()
                .map(|window| window.hwnd)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(workspace.container_idx_for_window(1), Some(0));
        assert_eq!(container.windows()[1].container_id, container_id);
        assert_eq!(container.windows()[1].floating_rect, Some(floating_rect(0)));
    }

    #[test]
    fn floating_the_last_visible_stored_window_hides_its_container() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        let floated_id = workspace.containers()[0].id.clone();

        workspace.float_window(0, floating_rect(0)).unwrap();
        workspace.record_logical_slots(area);

        assert!(workspace.containers()[0].is_hidden());
        assert!(!workspace.logical_slots.contains(&floated_id));
        assert_eq!(workspace.active_container_count(), 1);
        // The container is still there with its window; only its slot went away.
        assert_eq!(workspace.containers().len(), 2);
    }

    fn floating_bounds() -> FloatingBounds {
        FloatingBounds::new(work_area(1920, 1080))
    }

    /// A workspace with two single-window containers, the first of which floats and is focused.
    fn workspace_with_a_focused_floating_window() -> Workspace {
        let mut workspace = workspace_with_containers(&[1, 1]);

        workspace.float_window(0, floating_rect(100)).unwrap();
        workspace.focus_container(0);

        workspace
    }

    /// A container of single-window stacks, built the way an arriving container would be.
    fn container_with_hwnds(hwnds: &[isize]) -> Container {
        let mut container = Container::default();

        for hwnd in hwnds {
            container.add_window(Window::from(*hwnd));
        }

        container
    }

    #[test]
    fn a_container_arriving_at_an_empty_workspace_takes_the_work_area() {
        let mut workspace = Workspace::default();
        let area = work_area(1920, 1080);
        let arriving = container_with_hwnds(&[10, 11]);
        let id = arriving.id.clone();

        let arrival = workspace.adopt_container(arriving, None);
        workspace.record_logical_slots(area);

        assert_eq!(arrival, ContainerArrival::Alone(id.clone()));
        assert_eq!(
            workspace.logical_slots.get(&id),
            Some(LogicalRect::from(area))
        );
    }

    #[test]
    fn a_container_arriving_at_an_occupied_workspace_halves_the_focused_slot() {
        let mut workspace = workspace_with_containers(&[1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let donor = workspace.containers()[0].id.clone();
        let arriving = container_with_hwnds(&[10]);
        let id = arriving.id.clone();

        let arrival = workspace.adopt_container(arriving, None);

        assert_eq!(
            arrival,
            ContainerArrival::Split {
                arrived: id.clone(),
                donor: donor.clone(),
                axis: SplitAxis::LeftRight,
            }
        );

        // A left/right division puts the arrival on the left, and the two halves tile the area.
        let arrived_slot = workspace.logical_slots.get(&id).unwrap();
        let donor_slot = workspace.logical_slots.get(&donor).unwrap();

        assert_eq!(arrived_slot.left, area.left);
        assert_eq!(arrived_slot.width, 960);
        assert_eq!(donor_slot.left, 960);
        assert_eq!(donor_slot.width, 960);
        assert!(
            workspace
                .logical_slots
                .validate_coverage(area.into())
                .is_ok()
        );
    }

    #[test]
    fn an_arriving_container_keeps_its_identity_stack_and_window_state() {
        let mut workspace = workspace_with_containers(&[1]);
        workspace.record_logical_slots(work_area(1920, 1080));

        let mut arriving = container_with_hwnds(&[10, 11, 12]);
        arriving.windows_mut()[0].set_floating(floating_rect(50));
        arriving.windows_mut()[1].set_minimized();
        let before = arriving.clone();

        workspace.adopt_container(arriving, None);

        let adopted = workspace
            .containers()
            .iter()
            .find(|container| container.id == before.id)
            .unwrap();

        assert_eq!(*adopted, before);
        // The windows already named this container, so nothing had to be restamped.
        for window in adopted.windows() {
            assert_eq!(window.container_id, before.id);
        }
    }

    #[test]
    fn a_hidden_container_arrives_without_taking_a_slot() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let slots_before = workspace.logical_slots.ordered(SlotOrder::LeftToRight);
        let focused_before = workspace.focused_container().unwrap().id.clone();

        let mut arriving = container_with_hwnds(&[10]);
        arriving.windows_mut()[0].set_minimized();
        let id = arriving.id.clone();

        let arrival = workspace.adopt_container(arriving, None);
        workspace.record_logical_slots(area);

        assert_eq!(arrival, ContainerArrival::Hidden(id.clone()));
        assert!(workspace.containers().iter().any(|c| c.id == id));
        assert!(!workspace.logical_slots.contains(&id));
        // The containers already here kept the area, and the one being shown goes on being shown.
        assert_eq!(
            workspace.logical_slots.ordered(SlotOrder::LeftToRight),
            slots_before
        );
        assert_eq!(workspace.focused_container().unwrap().id, focused_before);
    }

    #[test]
    fn a_container_of_only_floating_windows_arrives_hidden() {
        let mut workspace = workspace_with_containers(&[1]);
        workspace.record_logical_slots(work_area(1920, 1080));

        let mut arriving = container_with_hwnds(&[10]);
        arriving.windows_mut()[0].set_floating(floating_rect(50));
        let id = arriving.id.clone();

        let arrival = workspace.adopt_container(arriving, None);

        assert_eq!(arrival, ContainerArrival::Hidden(id));
    }

    #[test]
    fn a_container_arriving_at_a_workspace_of_hidden_containers_takes_the_work_area() {
        let mut workspace = workspace_with_containers(&[1]);
        let area = work_area(1920, 1080);
        workspace.containers_mut()[0].windows_mut()[0].set_minimized();
        workspace.record_logical_slots(area);

        let arriving = container_with_hwnds(&[10]);
        let id = arriving.id.clone();

        let arrival = workspace.adopt_container(arriving, None);
        workspace.record_logical_slots(area);

        assert_eq!(arrival, ContainerArrival::Alone(id.clone()));
        assert_eq!(workspace.logical_slots.len(), 1);
        assert_eq!(
            workspace.logical_slots.get(&id),
            Some(LogicalRect::from(area))
        );
    }

    #[test]
    fn an_arrival_divides_the_longer_edge_unless_it_is_told_otherwise() {
        for (area, expected) in [
            (work_area(1920, 1080), SplitAxis::LeftRight),
            (work_area(1080, 1920), SplitAxis::TopBottom),
        ] {
            let mut workspace = workspace_with_containers(&[1]);
            workspace.record_logical_slots(area);

            let arrival = workspace.adopt_container(container_with_hwnds(&[10]), None);

            assert!(matches!(
                arrival,
                ContainerArrival::Split { axis, .. } if axis == expected
            ));
        }

        // And a forced axis overrides the shape of the slot.
        let mut workspace = workspace_with_containers(&[1]);
        workspace.record_logical_slots(work_area(1920, 1080));

        let arrival =
            workspace.adopt_container(container_with_hwnds(&[10]), Some(SplitAxis::TopBottom));

        assert!(matches!(
            arrival,
            ContainerArrival::Split {
                axis: SplitAxis::TopBottom,
                ..
            }
        ));
    }

    #[test]
    fn an_arrival_discards_the_exact_hidden_restores_it_invalidates() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        // Hiding a container writes the restore record which says who absorbed its slot.
        workspace.containers_mut()[0].windows_mut()[0].set_minimized();
        workspace.record_logical_slots(area);
        assert!(!workspace.hidden_slot_restores.is_empty());

        workspace.adopt_container(container_with_hwnds(&[10]), None);

        // A container arriving from elsewhere is the topology change that record cannot survive.
        assert!(workspace.hidden_slot_restores.is_empty());
    }

    #[test]
    fn transferring_a_workspace_carries_its_floating_rectangles_into_the_new_area() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let from = work_area(1920, 1080);
        let to = Rect {
            left: 1920,
            top: 0,
            right: 2560,
            bottom: 1440,
        };

        workspace.transfer_floating_rects(from, to);

        let carried = workspace.containers()[0].windows()[0]
            .floating_rect
            .unwrap();

        // The rectangle is expressed in the target's coordinates and stays inside it.
        assert!(carried.left >= to.left);
        assert!(carried.left + carried.right <= to.left + to.right);
        assert_eq!(
            carried,
            floating_geometry::transfer_between_areas(floating_rect(100), from, to)
        );
    }

    #[test]
    fn transferring_a_workspace_leaves_its_stored_windows_alone() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let stored_before = workspace.containers()[1].windows()[0].clone();

        workspace.transfer_floating_rects(
            work_area(1920, 1080),
            Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
        );

        // A stored window is placed by a slot the receiving workspace recalculates, so nothing
        // about it is rewritten here.
        assert_eq!(workspace.containers()[1].windows()[0], stored_before);
    }

    #[test]
    fn transferring_a_workspace_into_the_same_area_changes_nothing() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let area = work_area(1920, 1080);
        let before = workspace.clone();

        workspace.transfer_floating_rects(area, area);

        assert_eq!(workspace, before);
    }

    #[test]
    fn a_window_which_has_never_floated_gets_no_rectangle_from_a_transfer() {
        let mut workspace = workspace_with_containers(&[1]);

        workspace.transfer_floating_rects(
            work_area(1920, 1080),
            Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
        );

        assert!(
            workspace.containers()[0].windows()[0]
                .floating_rect
                .is_none()
        );
    }

    #[test]
    fn moving_a_floating_window_changes_its_rectangle_and_nothing_else() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let slots_before = workspace.logical_slots.ordered(SlotOrder::LeftToRight);
        let ids_before = workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect::<Vec<_>>();

        let change = workspace
            .move_focused_floating_window(OperationDirection::Right, 50, floating_bounds(), None)
            .unwrap();

        assert_eq!(change.hwnd, 0);
        assert!(change.changed);
        assert_eq!(change.rect, floating_rect(150));
        assert_eq!(
            workspace.containers()[0].windows()[0].floating_rect,
            Some(floating_rect(150))
        );

        // The arrangement, the container identities and the other container's window are all
        // untouched: a floating move is not a layout operation.
        assert_eq!(
            workspace.logical_slots.ordered(SlotOrder::LeftToRight),
            slots_before
        );
        assert_eq!(
            workspace
                .containers()
                .iter()
                .map(|container| container.id.clone())
                .collect::<Vec<_>>(),
            ids_before
        );
        assert_eq!(workspace.containers()[1].windows()[0].floating_rect, None);
    }

    #[test]
    fn resizing_a_floating_window_moves_only_the_named_edge() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let slots_before = workspace.logical_slots.ordered(SlotOrder::LeftToRight);

        let change = workspace
            .resize_focused_floating_window(
                OperationDirection::Right,
                Sizing::Increase,
                50,
                FloatingLimits::default(),
                None,
            )
            .unwrap();

        assert_eq!(change.rect.left, floating_rect(100).left);
        assert_eq!(change.rect.top, floating_rect(100).top);
        assert_eq!(change.rect.right, floating_rect(100).right + 50);
        assert_eq!(change.rect.bottom, floating_rect(100).bottom);
        assert_eq!(
            workspace.logical_slots.ordered(SlotOrder::LeftToRight),
            slots_before
        );
    }

    #[test]
    fn a_floating_command_refuses_every_other_kind_of_window() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        workspace.focus_container(0);

        // Stored: the container owns this window's rectangle.
        assert_eq!(
            workspace.move_focused_floating_window(
                OperationDirection::Right,
                50,
                floating_bounds(),
                None
            ),
            Err(FloatingRejection::NotFloating)
        );

        workspace.float_window(0, floating_rect(100)).unwrap();
        workspace.minimize_window(0).unwrap();
        assert_eq!(
            workspace.move_focused_floating_window(
                OperationDirection::Right,
                50,
                floating_bounds(),
                None
            ),
            Err(FloatingRejection::Minimized)
        );

        workspace.unminimize_window(0).unwrap();
        workspace.containers_mut()[0].windows_mut()[0].set_maximized(floating_rect(100));
        assert_eq!(
            workspace.resize_focused_floating_window(
                OperationDirection::Right,
                Sizing::Increase,
                50,
                FloatingLimits::default(),
                None,
            ),
            Err(FloatingRejection::Presented(Presentation::Maximized))
        );

        // None of the refusals wrote anything.
        assert_eq!(
            workspace.containers()[0].windows()[0].floating_rect,
            Some(floating_rect(100))
        );
    }

    #[test]
    fn a_floating_command_without_a_focused_window_is_refused() {
        let mut workspace = Workspace::default();

        assert_eq!(
            workspace.move_focused_floating_window(
                OperationDirection::Left,
                50,
                floating_bounds(),
                None
            ),
            Err(FloatingRejection::NoSubject)
        );
    }

    #[test]
    fn a_floating_command_acts_on_the_focused_window_not_the_first_floating_one() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        workspace.float_window(0, floating_rect(100)).unwrap();
        workspace.float_window(1, floating_rect(700)).unwrap();
        workspace.focus_container(1);

        let change = workspace
            .move_focused_floating_window(OperationDirection::Right, 50, floating_bounds(), None)
            .unwrap();

        assert_eq!(change.hwnd, 1);
        assert_eq!(
            workspace.containers()[0].windows()[0].floating_rect,
            Some(floating_rect(100))
        );
    }

    #[test]
    fn moving_a_floating_window_in_a_hidden_container_leaves_it_hidden() {
        let mut workspace = workspace_with_a_focused_floating_window();
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let hidden_id = workspace.containers()[0].id.clone();
        assert!(workspace.containers()[0].is_hidden());

        workspace
            .move_focused_floating_window(OperationDirection::Down, 50, floating_bounds(), None)
            .unwrap();

        // A hidden container's floating window is visible and movable; moving it is not a reason
        // to give the container a slot back, because nothing about its placement changed.
        assert!(workspace.containers()[0].is_hidden());
        assert!(!workspace.logical_slots.contains(&hidden_id));
        assert_eq!(workspace.active_container_count(), 1);
    }

    #[test]
    fn a_floating_move_settles_against_the_work_area() {
        let mut workspace = workspace_with_a_focused_floating_window();

        let change = workspace
            .move_focused_floating_window(OperationDirection::Left, 5000, floating_bounds(), None)
            .unwrap();

        assert_eq!(change.rect.left, 0);
        assert!(change.changed);

        // Already against the edge: the command is a no-op rather than a refusal.
        let change = workspace
            .move_focused_floating_window(OperationDirection::Left, 5000, floating_bounds(), None)
            .unwrap();

        assert!(!change.changed);
        assert_eq!(change.rect.left, 0);
    }

    #[test]
    fn a_floating_command_starts_from_what_win32_reports() {
        let mut workspace = workspace_with_a_focused_floating_window();

        // The window was dragged with the mouse, so the record is stale until a command reads the
        // live rectangle and plans from that.
        let dragged = floating_rect(900);
        let change = workspace
            .move_focused_floating_window(
                OperationDirection::Right,
                50,
                floating_bounds(),
                Some(dragged),
            )
            .unwrap();

        assert_eq!(change.rect, floating_rect(950));
    }

    #[test]
    fn the_accepted_rectangle_replaces_the_planned_one() {
        let mut workspace = workspace_with_a_focused_floating_window();

        let change = workspace
            .resize_focused_floating_window(
                OperationDirection::Right,
                Sizing::Decrease,
                200,
                FloatingLimits::default(),
                None,
            )
            .unwrap();

        // The application refused to go that narrow and Win32 reported what it settled on.
        let mut accepted = change.rect;
        accepted.right = 320;
        assert!(workspace.confirm_floating_geometry(change.hwnd, accepted));

        assert_eq!(
            workspace.containers()[0].windows()[0].floating_rect,
            Some(accepted)
        );
    }

    #[test]
    fn unfloating_returns_the_container_to_the_arrangement() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        let floated_id = workspace.containers()[0].id.clone();

        workspace.float_window(0, floating_rect(0)).unwrap();
        workspace.record_logical_slots(area);
        workspace.unfloat_window(0).unwrap();
        workspace.record_logical_slots(area);

        assert!(workspace.containers()[0].is_active());
        assert!(workspace.logical_slots.contains(&floated_id));
        assert_eq!(workspace.active_container_count(), 2);
        // Unfloating focuses the window it restored, at both levels.
        assert_eq!(workspace.focused_container_idx(), 0);
        assert_eq!(
            workspace.focused_container().unwrap().focused_window_idx(),
            0
        );
    }

    #[test]
    fn floating_windows_are_listed_in_container_then_stack_order() {
        let mut workspace = workspace_with_containers(&[2, 2]);

        // Float them out of order to prove the listing is derived, not an insertion log.
        workspace.float_window(3, floating_rect(0)).unwrap();
        workspace.float_window(0, floating_rect(1)).unwrap();
        workspace.float_window(2, floating_rect(2)).unwrap();

        assert_eq!(
            workspace.floating_windows(),
            vec![Window::from(0), Window::from(2), Window::from(3)]
        );
        assert_eq!(workspace.floating_window_idx(3), Some(2));
        assert!(workspace.is_floating_window(2));
        assert!(!workspace.is_floating_window(1));
    }

    #[test]
    fn a_floating_window_carries_its_state_to_another_workspace() {
        let mut source = workspace_with_containers(&[2]);
        let mut target = workspace_with_containers(&[1]);
        source.float_window(1, floating_rect(7)).unwrap();

        let window = source.take_window(1).unwrap();
        target.adopt_managed_window(window);

        // The source container kept its other window, so it survives the move.
        assert_eq!(source.containers().len(), 1);
        assert!(source.floating_windows().is_empty());

        assert_eq!(target.containers().len(), 2);
        assert!(target.is_floating_window(1));
        let adopted = &target.containers()[1];
        assert_eq!(adopted.windows()[0].floating_rect, Some(floating_rect(7)));
        // The receiving container stamped its own ownership on the window.
        assert_eq!(adopted.windows()[0].container_id, adopted.id);
    }

    #[test]
    fn floating_a_window_this_workspace_does_not_own_fails_without_changing_anything() {
        let mut workspace = workspace_with_containers(&[1]);
        let before = workspace.clone();

        assert!(workspace.float_window(404, floating_rect(0)).is_err());
        assert_eq!(workspace.containers(), before.containers());
        assert!(workspace.floating_windows().is_empty());
    }

    #[test]
    fn a_hidden_container_of_floating_windows_is_not_positioned_by_the_arrangement() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.float_window(0, floating_rect(0)).unwrap();
        workspace.record_logical_slots(area);

        // The remaining active container covers the whole area, and the floating window's
        // container is absent from the slot map entirely.
        assert_eq!(workspace.logical_slots.len(), 1);
        assert_eq!(
            workspace.logical_slots.get(&workspace.containers()[1].id),
            Some(LogicalRect::from(area))
        );
        assert_eq!(
            workspace.containers()[0].visible_stored_windows().count(),
            0
        );
    }

    #[test]
    fn minimizing_a_window_keeps_it_in_its_container() {
        let mut workspace = workspace_with_containers(&[3]);
        let container_id = workspace.containers()[0].id.clone();

        assert!(workspace.minimize_window(1).unwrap());

        let container = &workspace.containers()[0];
        assert_eq!(container.id, container_id);
        assert_eq!(container.windows().len(), 3);
        assert_eq!(
            container
                .windows()
                .iter()
                .map(|window| window.hwnd)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(container.windows()[1].visibility, Visibility::Minimized);
        assert!(container.is_active());
        assert!(workspace.minimize_history.contains(&1));
    }

    #[test]
    fn minimizing_the_last_visible_stored_window_hides_its_container() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        let minimized_id = workspace.containers()[0].id.clone();

        workspace.minimize_window(0).unwrap();
        workspace.record_logical_slots(area);

        assert!(workspace.containers()[0].is_hidden());
        assert!(!workspace.logical_slots.contains(&minimized_id));
        // The container is not destroyed and keeps the window it owns.
        assert_eq!(workspace.containers().len(), 2);
        assert_eq!(workspace.containers()[0].windows().len(), 1);
    }

    #[test]
    fn minimizing_the_focused_window_moves_container_focus_off_it() {
        let mut workspace = workspace_with_containers(&[3]);
        workspace.containers_mut()[0].focus_window(1);

        workspace.minimize_window(1).unwrap();

        let container = &workspace.containers()[0];
        assert_ne!(container.focused_window_idx(), 1);
        assert!(
            container
                .focused_managed_window()
                .is_some_and(ManagedWindow::is_visible_stored)
        );
    }

    #[test]
    fn a_repeated_minimize_changes_nothing() {
        let mut workspace = workspace_with_containers(&[2]);

        assert!(workspace.minimize_window(0).unwrap());
        let after_first = workspace.clone();

        assert!(!workspace.minimize_window(0).unwrap());
        assert_eq!(workspace.containers(), after_first.containers());
        assert_eq!(workspace.minimize_history, after_first.minimize_history);
    }

    #[test]
    fn restoring_the_last_minimized_window_reactivates_and_focuses_it() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.focus_container(1);
        workspace.minimize_window(0).unwrap();
        workspace.record_logical_slots(area);

        assert_eq!(workspace.restore_last_minimized_window(), Some(0));
        workspace.record_logical_slots(area);

        assert!(workspace.containers()[0].is_active());
        assert!(
            workspace
                .logical_slots
                .contains(&workspace.containers()[0].id)
        );
        assert_eq!(workspace.focused_container_idx(), 0);
        assert!(!workspace.minimize_history.contains(&0));
        // Both levels of history recorded the restored selection.
        assert_eq!(
            workspace.container_focus_history.iter().next(),
            Some(&workspace.containers()[0].id)
        );
    }

    #[test]
    fn a_restored_window_returns_to_the_top_of_its_stack_and_reactivates_its_container() {
        let mut workspace = workspace_with_containers(&[3]);
        let area = work_area(1920, 1080);

        // Every window of the container is minimized, so the container is hidden and holds no slot.
        for hwnd in [2, 1, 0] {
            workspace.minimize_window(hwnd).unwrap();
        }
        workspace.record_logical_slots(area);
        assert!(workspace.containers()[0].is_hidden());
        assert!(workspace.logical_slots.is_empty());

        assert_eq!(workspace.restore_last_minimized_window(), Some(0));
        workspace.record_logical_slots(area);

        let container = &workspace.containers()[0];
        assert_eq!(
            container
                .windows()
                .iter()
                .map(|w| w.hwnd)
                .collect::<Vec<_>>(),
            vec![1, 2, 0],
            "the restored window is on top and the others keep their order"
        );
        assert_eq!(container.focused_window().map(|w| w.hwnd), Some(0));
        assert!(container.is_active());
        assert!(workspace.logical_slots.contains(&container.id));
        assert_eq!(
            workspace
                .minimize_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn a_restored_window_keeps_the_placement_it_was_minimized_with() {
        let mut workspace = workspace_with_containers(&[2]);
        workspace.float_window(0, floating_rect(3)).unwrap();
        workspace.containers_mut()[0].windows_mut()[0].presentation = Presentation::Maximized;
        workspace.minimize_window(0).unwrap();

        assert_eq!(workspace.restore_last_minimized_window(), Some(0));

        // Addressed by identity rather than by depth: a restored window returns to the top of its
        // container's stack, which is the last element rather than the first.
        let container = &workspace.containers()[0];
        let window = container
            .windows()
            .iter()
            .find(|window| window.hwnd == 0)
            .unwrap();
        assert_eq!(container.windows().back().map(|w| w.hwnd), Some(0));
        assert_eq!(window.visibility, Visibility::Visible);
        assert_eq!(window.placement, ManagedPlacement::Floating);
        assert_eq!(window.presentation, Presentation::Maximized);
        assert_eq!(window.floating_rect, Some(floating_rect(3)));
    }

    #[test]
    fn restoring_without_a_minimized_window_changes_nothing() {
        let mut workspace = workspace_with_containers(&[2]);
        let before = workspace.clone();

        assert_eq!(workspace.restore_last_minimized_window(), None);
        assert_eq!(workspace.containers(), before.containers());

        // A history entry for a window which is no longer minimized is discarded, not restored.
        workspace.record_minimized_window(0);
        assert_eq!(workspace.restore_last_minimized_window(), None);
        assert!(workspace.minimize_history.is_empty());
    }

    #[test]
    fn a_repeated_unminimize_changes_nothing() {
        let mut workspace = workspace_with_containers(&[2]);
        workspace.minimize_window(0).unwrap();

        assert!(workspace.unminimize_window(0).unwrap());
        let after_first = workspace.clone();

        assert!(!workspace.unminimize_window(0).unwrap());
        assert_eq!(workspace.containers(), after_first.containers());
    }

    #[test]
    fn minimizing_a_window_this_workspace_does_not_own_fails_without_changing_anything() {
        let mut workspace = workspace_with_containers(&[1]);
        let before = workspace.clone();

        assert!(workspace.minimize_window(404).is_err());
        assert_eq!(workspace.containers(), before.containers());
        assert!(workspace.minimize_history.is_empty());
    }

    fn hide_container(workspace: &mut Workspace, idx: usize) {
        for window in workspace.containers_mut()[idx].windows_mut().iter_mut() {
            window.set_minimized();
        }
    }

    #[test]
    fn a_hidden_container_occupies_no_logical_slot() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        let hidden_id = workspace.containers()[1].id.clone();

        workspace.record_logical_slots(area);
        assert_eq!(workspace.logical_slots.len(), 3);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        assert_eq!(workspace.active_container_count(), 2);
        assert_eq!(workspace.logical_slots.len(), 2);
        assert!(!workspace.logical_slots.contains(&hidden_id));
        // The container itself is untouched: it keeps its window, its ID and its position.
        assert_eq!(workspace.containers().len(), 3);
        assert_eq!(workspace.containers()[1].id, hidden_id);
        assert_eq!(workspace.containers()[1].windows().len(), 1);
    }

    #[test]
    fn a_new_window_does_not_freeze_an_arrangement_which_owes_a_recalculation() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        // Something the slots cannot express locally: the layout changed, so the whole
        // arrangement is owed a recalculation and the slots still hold the old one.
        workspace.layout = Layout::Default(DefaultLayout::Rows);
        workspace.invalidate_slot_geometry();

        // A container leaves while that is outstanding. Its slot is not absorbed, because there
        // is no arrangement to absorb it into, so what is left covers half the work area.
        workspace.destroy_container(1).unwrap();

        // A new window arrives. Halving a stale slot is fine; adopting the result as the
        // arrangement is not, because the recalculation the layout change asked for would be
        // dropped and the workspace would tile half its work area from then on.
        workspace.place_new_window(Window::from(9));

        assert!(
            workspace.relayout_pending,
            "the pending recalculation survived the new window"
        );

        workspace.record_logical_slots(area);

        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    #[test]
    fn the_active_containers_expand_over_a_hidden_container_area() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        // Two containers, three slots' worth of area: the remaining slots still tile it exactly,
        // which is only possible if they absorbed what the hidden container had.
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    fn show_container(workspace: &mut Workspace, idx: usize) {
        for window in workspace.containers_mut()[idx].windows_mut().iter_mut() {
            window.set_visible();
        }
    }

    #[test]
    fn hiding_a_container_gives_its_slot_to_a_complete_edge_group() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let hidden_id = workspace.containers()[1].id.clone();
        let before: HashMap<_, _> = workspace
            .logical_slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        // The hidden container's area went to neighbours; it was not redistributed by relaying the
        // whole workspace out, so the container which is neither hidden nor an absorber is
        // untouched.
        let record = workspace.hidden_slot_restores.get(&hidden_id).unwrap();
        assert_eq!(record.old_rect, before[&hidden_id]);
        assert!(record.exact_restore_valid);
        assert!(!record.absorbers.is_empty());

        for (id, slot) in &before {
            if id != &hidden_id && !record.absorbers.contains(id) {
                assert_eq!(workspace.logical_slots.get(id), Some(*slot));
            }
        }

        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    #[test]
    fn restoring_a_container_shrinks_its_absorbers_back_exactly() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before: HashMap<_, _> = workspace
            .logical_slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);
        show_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        for (id, slot) in &before {
            assert_eq!(workspace.logical_slots.get(id), Some(*slot), "{id}");
        }
        assert!(workspace.hidden_slot_restores.is_empty());
    }

    #[test]
    fn a_floating_window_hides_and_restores_its_container_slot() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let floated_id = workspace.containers()[0].id.clone();
        let hwnd = workspace.containers()[0].windows()[0].hwnd;
        let before = workspace.logical_slots.get(&floated_id).unwrap();

        workspace.float_window(hwnd, Rect::default()).unwrap();
        workspace.record_logical_slots(area);

        assert!(!workspace.logical_slots.contains(&floated_id));
        assert_eq!(
            workspace.logical_slots.get(&workspace.containers()[1].id),
            Some(LogicalRect::from(area))
        );

        workspace.unfloat_window(hwnd).unwrap();
        workspace.record_logical_slots(area);

        assert_eq!(workspace.logical_slots.get(&floated_id), Some(before));
    }

    #[test]
    fn several_containers_hide_and_restore_one_after_another() {
        let mut workspace = workspace_with_containers(&[1, 1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before: HashMap<_, _> = workspace
            .logical_slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        for idx in [1, 2] {
            hide_container(&mut workspace, idx);
            workspace.record_logical_slots(area);
            assert!(
                workspace
                    .logical_slots
                    .validate_coverage(LogicalRect::from(area))
                    .is_ok()
            );
        }

        assert_eq!(workspace.logical_slots.len(), 2);
        assert_eq!(workspace.hidden_slot_restores.len(), 2);

        // Restoring in the reverse order undoes each absorption against the arrangement it was
        // recorded on, which is what makes the round trip exact.
        for idx in [2, 1] {
            show_container(&mut workspace, idx);
            workspace.record_logical_slots(area);
        }

        for (id, slot) in &before {
            assert_eq!(workspace.logical_slots.get(id), Some(*slot), "{id}");
        }
        assert!(workspace.hidden_slot_restores.is_empty());
    }

    #[test]
    fn a_layout_change_drops_the_restore_records_and_relays_out() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);
        assert_eq!(workspace.hidden_slot_restores.len(), 1);

        workspace.layout = Layout::Default(DefaultLayout::Columns);
        workspace.record_logical_slots(area);

        assert!(workspace.hidden_slot_restores.is_empty());
        assert_eq!(workspace.logical_slots.len(), 2);
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );

        // Without a record the restore falls back to a full recalculation rather than refusing.
        show_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        assert_eq!(workspace.logical_slots.len(), 3);
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    #[test]
    fn a_changed_work_area_relays_out_instead_of_editing_slots() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        workspace.record_logical_slots(work_area(1920, 1080));

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(work_area(1920, 1080));
        assert_eq!(workspace.hidden_slot_restores.len(), 1);

        let smaller = work_area(1280, 720);
        workspace.record_logical_slots(smaller);

        assert!(workspace.hidden_slot_restores.is_empty());
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(smaller))
                .is_ok()
        );
    }

    #[test]
    fn a_manual_resize_makes_the_exact_restore_impossible() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        // A resize adjustment is an arrangement input, so it is what the fingerprint notices.
        workspace.resize_dimensions = vec![
            Some(Rect {
                left: 0,
                top: 0,
                right: 100,
                bottom: 0,
            }),
            None,
            None,
        ];
        workspace.record_logical_slots(area);

        assert!(workspace.hidden_slot_restores.is_empty());

        show_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        assert_eq!(workspace.logical_slots.len(), 3);
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    #[test]
    fn hiding_the_only_active_container_leaves_no_active_slot() {
        let mut workspace = workspace_with_containers(&[1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 0);
        workspace.record_logical_slots(area);

        assert!(workspace.logical_slots.is_empty());
        // Nothing absorbed it, so there is nothing to reverse and no record is kept.
        assert!(workspace.hidden_slot_restores.is_empty());

        show_container(&mut workspace, 0);
        workspace.record_logical_slots(area);

        assert_eq!(
            workspace.logical_slots.get(&workspace.containers()[0].id),
            Some(LogicalRect::from(area))
        );
    }

    #[test]
    fn moving_focus_does_not_discard_an_absorption() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);
        let absorbed: HashMap<_, _> = workspace
            .logical_slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        assert_ne!(workspace.focused_container_idx(), 0);
        workspace.focus_container(0);
        workspace.record_logical_slots(area);

        for (id, slot) in &absorbed {
            assert_eq!(workspace.logical_slots.get(id), Some(*slot), "{id}");
        }
        assert_eq!(workspace.hidden_slot_restores.len(), 1);
    }

    #[test]
    fn the_scrolling_layout_relays_out_when_focus_moves() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.layout = Layout::Default(DefaultLayout::Scrolling);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);
        assert_eq!(workspace.hidden_slot_restores.len(), 1);

        // This is the one layout which arranges around the focused container, so a focus change
        // really is an arrangement change for it.
        assert_ne!(workspace.focused_container_idx(), 0);
        workspace.focus_container(0);

        assert!(workspace.hidden_slot_restores.is_empty());
        assert!(workspace.relayout_pending);
    }

    #[test]
    fn the_last_active_container_takes_the_whole_work_area() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);

        hide_container(&mut workspace, 0);
        workspace.record_logical_slots(area);

        let slot = workspace.logical_slots.get(&workspace.containers()[1].id);
        assert_eq!(slot, Some(LogicalRect::from(area)));
    }

    #[test]
    fn hiding_every_container_leaves_no_active_slot() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 0);
        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        assert_eq!(workspace.active_container_count(), 0);
        assert!(workspace.logical_slots.is_empty());
        assert_eq!(workspace.hidden_containers().count(), 2);
        assert_eq!(workspace.active_container_idx_for_geometry(), None);
    }

    #[test]
    fn geometry_starts_from_the_most_recent_active_container_when_the_focus_is_hidden() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);

        workspace.focus_container(2);
        workspace.focus_container(0);
        assert_eq!(workspace.active_container_idx_for_geometry(), Some(0));

        hide_container(&mut workspace, 0);

        assert_eq!(workspace.container_state(0), Some(ContainerState::Hidden));
        assert_eq!(workspace.active_container_idx_for_geometry(), Some(2));
    }

    #[test]
    fn hidden_containers_are_absent_from_the_active_selectors() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        hide_container(&mut workspace, 1);

        assert_eq!(workspace.active_container_indices(), vec![0, 2]);
        assert_eq!(workspace.active_container_count(), 2);
        assert_eq!(workspace.hidden_containers().count(), 1);
        assert_eq!(workspace.container_state(1), Some(ContainerState::Hidden));
        assert_eq!(workspace.container_state(9), None);
    }

    #[test]
    fn logical_slots_are_keyed_by_identity_not_by_index() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let first_id = workspace.containers()[0].id.clone();
        let second_id = workspace.containers()[1].id.clone();
        let first_slot = workspace.logical_slots.get(&first_id).unwrap();
        let second_slot = workspace.logical_slots.get(&second_id).unwrap();
        assert_ne!(first_slot, second_slot);

        workspace.containers_mut().swap(0, 1);
        workspace.record_logical_slots(area);

        // Both containers still have a slot under their own ID, and the geometry followed the
        // new ordering rather than staying attached to a stale index.
        assert_eq!(workspace.logical_slots.get(&second_id), Some(first_slot));
        assert_eq!(workspace.logical_slots.get(&first_id), Some(second_slot));
    }

    #[test]
    fn removing_a_container_drops_its_logical_slot() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        workspace.record_logical_slots(work_area(1920, 1080));

        let removed_id = workspace.containers()[1].id.clone();
        let kept_id = workspace.containers()[0].id.clone();

        workspace.remove_container_by_idx(1);

        assert!(!workspace.logical_slots.contains(&removed_id));
        assert!(workspace.logical_slots.contains(&kept_id));
    }

    #[test]
    fn a_point_in_the_rendered_gutter_still_belongs_to_a_container() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let mut boundary = None;
        for idx in 0..2 {
            let slot = workspace.logical_slot_at(idx).unwrap();
            if slot.left > 0 {
                boundary = Some(slot.left);
            }
        }

        let boundary = boundary.expect("two containers should share a vertical edge");

        // The renderer insets both neighbours by the gap, so this column is inside no rendered
        // rectangle at all; the gap-free slots still attribute it to exactly one container.
        let gutter = (boundary - 1, area.bottom / 2);
        let owner = workspace.container_idx_from_logical_point(gutter);

        let owner = owner.expect("a gap-free slot must own every point of the work area");
        assert!(
            workspace
                .logical_slot_at(owner)
                .unwrap()
                .contains_point(gutter)
        );
    }

    #[test]
    fn recalculating_slots_advances_the_geometry_generation() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);

        workspace.record_logical_slots(area);
        let generation = workspace.logical_slots.generation();

        workspace.record_logical_slots(work_area(1280, 1024));

        assert!(workspace.logical_slots.generation() > generation);
        assert_eq!(
            workspace.logical_work_area,
            Some(LogicalRect::from(work_area(1280, 1024)))
        );
    }

    #[test]
    fn workspace_json_without_logical_slots_still_deserializes() {
        let workspace = Workspace::default();
        let mut json = serde_json::to_value(&workspace).unwrap();
        let object = json.as_object_mut().unwrap();
        object.remove("logical_slots");
        object.remove("logical_work_area");

        let migrated: Workspace = serde_json::from_value(json).unwrap();

        assert_eq!(migrated.logical_slots, LogicalSlots::default());
        assert_eq!(migrated.logical_work_area, None);
    }

    fn slot_map(workspace: &Workspace) -> HashMap<ContainerId, LogicalRect> {
        workspace
            .logical_slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect()
    }

    /// The number of edges which differ between two rectangles.
    ///
    /// An expansion may only ever move one of them: moving two would mean a container had changed
    /// both its width and its height to take a rectangular slot, which it can only do by opening a
    /// hole somewhere else.
    fn edges_moved(before: LogicalRect, after: LogicalRect) -> usize {
        usize::from(before.left != after.left)
            + usize::from(before.top != after.top)
            + usize::from(before.right() != after.right())
            + usize::from(before.bottom() != after.bottom())
    }

    #[test]
    fn deleting_a_container_gives_its_slot_to_its_neighbours() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before = slot_map(&workspace);
        let deleted = workspace.containers()[1].id.clone();

        workspace.remove_container(1);

        assert!(!workspace.logical_slots.contains(&deleted));
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );

        // The freed area went to neighbours rather than being redistributed by a fresh layout, so
        // every container which changed at all changed exactly one of its edges.
        let after = slot_map(&workspace);
        let mut grew = 0;

        for (id, slot) in &after {
            let was = before[id];

            if was != *slot {
                assert_eq!(edges_moved(was, *slot), 1, "{id} moved more than one edge");
                assert!(slot.area() > was.area(), "{id} did not grow");
                grew += 1;
            }
        }

        assert!(grew > 0, "nothing expanded over the deleted slot");
    }

    #[test]
    fn every_neighbour_on_a_complete_edge_expands_together() {
        let mut workspace = workspace_with_containers(&[1, 1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before = slot_map(&workspace);
        let deleted = workspace.containers()[1].id.clone();
        let freed = before[&deleted];

        workspace.remove_container(1);

        let after = slot_map(&workspace);
        let grown: Vec<_> = after
            .iter()
            .filter(|(id, slot)| before[*id] != **slot)
            .collect();

        // A four-container arrangement puts two containers on the complete edge of this one, so
        // this exercises the multi-neighbour case rather than the single-neighbour one.
        assert!(
            grown.len() > 1,
            "expected several neighbours to share the freed slot, got {}",
            grown.len()
        );

        // Together they took exactly the freed slot: no more, and nothing from anyone else.
        let gained: i64 = grown
            .iter()
            .map(|(id, slot)| slot.area() - before[*id].area())
            .sum();

        assert_eq!(gained, freed.area());

        for (id, slot) in &grown {
            assert_eq!(edges_moved(before[*id], **slot), 1, "{id}");
        }

        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
    }

    #[test]
    fn a_deletion_leaves_the_containers_it_did_not_touch_alone() {
        let mut workspace = workspace_with_containers(&[1, 1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before = slot_map(&workspace);
        let deleted = workspace.containers()[1].id.clone();

        workspace.remove_container(1);

        // A relayout would have moved every container. At least one which is not on the freed
        // slot's absorbing edge must still hold precisely the rectangle it had.
        let after = slot_map(&workspace);
        let unchanged = before
            .keys()
            .filter(|id| **id != deleted)
            .filter(|id| after.get(*id) == Some(&before[*id]))
            .count();

        assert!(unchanged > 0, "the whole workspace was relaid out");
        assert!(!workspace.relayout_pending);
    }

    #[test]
    fn deleting_the_last_active_container_leaves_no_active_slot() {
        let mut workspace = workspace_with_containers(&[1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        workspace.remove_container(0);
        workspace.record_logical_slots(area);

        assert!(workspace.logical_slots.is_empty());
        assert!(workspace.hidden_slot_restores.is_empty());
    }

    #[test]
    fn deleting_a_hidden_container_does_not_disturb_the_arrangement() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        let hidden = workspace.containers()[1].id.clone();
        let before = slot_map(&workspace);

        assert!(workspace.hidden_slot_restores.contains_key(&hidden));

        workspace.remove_container(1);

        // A hidden container holds no slot, so there is nothing to expand over and nothing to
        // recalculate; its restore record leaves with it.
        assert_eq!(slot_map(&workspace), before);
        assert!(!workspace.hidden_slot_restores.contains_key(&hidden));
        assert!(!workspace.relayout_pending);
    }

    #[test]
    fn deleting_an_absorber_makes_the_restore_it_holds_inexact() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 2);
        workspace.record_logical_slots(area);

        let hidden = workspace.containers()[2].id.clone();
        let absorber = workspace.hidden_slot_restores[&hidden].absorbers[0].clone();
        let absorber_idx = workspace.container_idx_for_id(&absorber).unwrap();

        assert!(workspace.hidden_slot_restores[&hidden].exact_restore_valid);

        workspace.remove_container(absorber_idx);

        // The record promised that this absorber still held exactly what the absorption gave it.
        // It no longer holds anything at all, so the promise is broken and the hidden container has
        // to come back through a recalculation instead of an exact reverse.
        let record = &workspace.hidden_slot_restores[&hidden];
        assert!(!record.exact_restore_valid);
        assert!(record.old_rect.area() > 0);
    }

    #[test]
    fn focus_moves_to_the_first_container_which_expanded() {
        let mut workspace = workspace_with_containers(&[1, 1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let expected = workspace.expansion_focus_target(1).unwrap();
        let before = slot_map(&workspace);

        workspace.remove_container(1);

        let focused = workspace.containers()[workspace.focused_container_idx()]
            .id
            .clone();

        assert_eq!(focused, expected);
        assert_ne!(
            workspace.logical_slots.get(&expected),
            Some(before[&expected])
        );
        assert_eq!(
            workspace.container_focus_history.most_recent(),
            Some(&expected)
        );
    }

    #[test]
    fn focus_after_a_deletion_never_lands_on_a_minimized_window() {
        let mut workspace = workspace_with_containers(&[1, 2, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let recipient = workspace.expansion_focus_target(0).unwrap();
        let recipient_idx = workspace.container_idx_for_id(&recipient).unwrap();

        // Minimize whichever window the recipient is currently showing.
        let top = workspace.containers()[recipient_idx]
            .focused_window()
            .unwrap()
            .hwnd;
        workspace.minimize_window(top).unwrap();

        workspace.remove_container(0);

        let idx = workspace.focused_container_idx();
        assert_eq!(workspace.containers()[idx].id, recipient);

        if let Some(window) = workspace.containers()[idx].focused_managed_window() {
            assert_eq!(window.visibility, Visibility::Visible);
        }
    }

    /// Every window this workspace holds, by container, bottom of the stack first.
    fn stacks(workspace: &Workspace) -> Vec<Vec<isize>> {
        workspace
            .containers()
            .iter()
            .map(|container| container.windows().iter().map(|w| w.hwnd).collect())
            .collect()
    }

    #[test]
    fn destroying_a_container_deals_its_windows_round_robin() {
        let mut workspace = workspace_with_containers(&[4, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let source: Vec<isize> = workspace.containers()[0]
            .windows()
            .iter()
            .map(|w| w.hwnd)
            .collect();
        let recipients = workspace.distribution_recipients(&workspace.containers()[0].id.clone());

        workspace.destroy_container(0).unwrap();

        assert_eq!(workspace.containers().len(), 2);

        // The source stack is dealt top down, so its top window goes to the first recipient, the
        // one below it to the second, and so on around again.
        let top_down: Vec<isize> = source.iter().rev().copied().collect();

        for (position, hwnd) in top_down.iter().enumerate() {
            let expected = &recipients[position % recipients.len()];
            let idx = workspace.container_idx_for_id(expected).unwrap();

            assert!(
                workspace.containers()[idx].contains_window(*hwnd),
                "window {hwnd} did not go to recipient {position}"
            );
        }

        // No window was lost or duplicated on the way.
        let mut landed: Vec<isize> = stacks(&workspace).concat();
        landed.sort_unstable();
        let mut expected: Vec<isize> = source.clone();
        expected.extend([4, 5]);
        expected.sort_unstable();

        assert_eq!(landed, expected);

        crate::invariants::assert_invariants(&workspace, "after destroying a container");
    }

    #[test]
    fn distributed_windows_arrive_underneath_what_the_recipient_was_showing() {
        let mut workspace = workspace_with_containers(&[2, 2]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let recipient_id = workspace.containers()[1].id.clone();
        let showing = workspace.containers()[1].focused_window().unwrap().hwnd;

        workspace.destroy_container(0).unwrap();

        let idx = workspace.container_idx_for_id(&recipient_id).unwrap();
        let container = &workspace.containers()[idx];

        // Dealing the source top down and inserting each window at the bottom lands them in the
        // recipient in the order they had in the source, underneath everything already there.
        assert_eq!(container.windows()[0].hwnd, 0);
        assert_eq!(container.windows()[1].hwnd, 1);
        assert_eq!(container.windows()[2].hwnd, 2);
        assert_eq!(container.focused_window().unwrap().hwnd, showing);

        // They are underneath in the focus history too, so a later focus selection prefers the
        // window this container already had.
        assert_eq!(container.focus_history().most_recent(), Some(&showing));
    }

    #[test]
    fn a_distributed_window_keeps_every_dimension_of_its_own_state() {
        let mut workspace = workspace_with_containers(&[2, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        workspace.float_window(0, floating_rect(3)).unwrap();
        workspace.containers_mut()[0].windows_mut()[0].presentation = Presentation::Maximized;
        workspace.minimize_window(0).unwrap();

        workspace.destroy_container(0).unwrap();

        let container = workspace.container_for_window(0).unwrap();
        let window = container
            .windows()
            .iter()
            .find(|w| w.hwnd == 0)
            .expect("the window kept its state but lost its container");

        assert_eq!(window.placement, ManagedPlacement::Floating);
        assert_eq!(window.visibility, Visibility::Minimized);
        assert_eq!(window.presentation, Presentation::Maximized);
        assert_eq!(window.floating_rect, Some(floating_rect(3)));

        // Ownership is the one thing which did change.
        assert_eq!(window.container_id, container.id);
    }

    #[test]
    fn destroying_a_container_expands_its_neighbours_over_its_slot() {
        let mut workspace = workspace_with_containers(&[2, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let destroyed = workspace.containers()[1].id.clone();

        workspace.destroy_container(1).unwrap();

        assert!(!workspace.logical_slots.contains(&destroyed));
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );
        assert!(!workspace.relayout_pending);
    }

    #[test]
    fn a_hidden_container_sends_its_windows_to_the_containers_which_took_its_area() {
        let mut workspace = workspace_with_containers(&[1, 1, 2]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 2);
        workspace.record_logical_slots(area);

        let hidden = workspace.containers()[2].id.clone();
        let absorbers = workspace.hidden_slot_restores[&hidden].absorbers.clone();
        let recipients = workspace.distribution_recipients(&hidden);
        let before = slot_map(&workspace);

        // The containers which took this one's area are offered its windows first.
        assert!(!absorbers.is_empty());
        assert_eq!(recipients[..absorbers.len()], absorbers[..]);

        let top = workspace.containers()[2].focused_window().unwrap().hwnd;

        workspace.destroy_container(2).unwrap();

        // A hidden container holds no slot, so nothing expands.
        assert_eq!(slot_map(&workspace), before);

        // Its stack is dealt from the top, so its top window goes to the first absorber. There are
        // more windows here than absorbers, so the rest go on round the recipient order, which is
        // why this asserts the deal rather than that every window landed on an absorber.
        let owner = workspace.container_for_window(top).unwrap().id.clone();
        assert_eq!(owner, recipients[0]);
        assert!(absorbers.contains(&owner));

        crate::invariants::assert_invariants(&workspace, "after destroying a hidden container");
    }

    #[test]
    fn destroying_the_only_container_of_a_workspace_refuses_without_changing_anything() {
        let mut workspace = workspace_with_containers(&[2]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before = workspace.clone();

        assert!(workspace.destroy_container(0).is_err());

        assert_eq!(workspace.containers(), before.containers());
        assert_eq!(slot_map(&workspace), slot_map(&before));
        assert_eq!(
            workspace.container_focus_history.len(),
            before.container_focus_history.len()
        );
    }

    #[test]
    fn an_empty_container_can_always_be_destroyed() {
        let mut workspace = workspace_with_containers(&[1]);
        workspace.containers_mut()[0].windows_mut().clear();

        assert!(workspace.destroy_container(0).is_ok());
        assert!(workspace.containers().is_empty());
    }

    #[test]
    fn destroying_a_container_which_does_not_hold_the_focus_leaves_the_focus_alone() {
        let mut workspace = workspace_with_containers(&[2, 1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        workspace.focus_container(3);
        let focused_container = workspace.containers()[3].id.clone();
        let focused_window = workspace.containers()[3].focused_window().unwrap().hwnd;
        let moved: Vec<isize> = workspace.containers()[1]
            .windows()
            .iter()
            .map(|w| w.hwnd)
            .collect();

        workspace.destroy_container(1).unwrap();

        // Changing how many containers there are is not a focus change.
        let idx = workspace.focused_container_idx();
        assert_eq!(workspace.containers()[idx].id, focused_container);
        assert_eq!(
            workspace.containers()[idx].focused_window().unwrap().hwnd,
            focused_window
        );

        // And every recipient goes on showing what it was already showing.
        for container in workspace.containers() {
            let shown = container.focused_window().unwrap().hwnd;
            assert!(!moved.contains(&shown));
        }
    }

    #[test]
    fn destroying_the_focused_container_carries_its_window_to_the_top_of_its_recipient() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        workspace.focus_container(1);
        let focused_window = workspace.containers()[1].focused_window().unwrap().hwnd;

        workspace.destroy_container(1).unwrap();

        // The window the user was working on is still the window they are working on, and it is on
        // top of the stack it landed in rather than hidden underneath it.
        assert_eq!(workspace.containers().len(), 1);
        let container = &workspace.containers()[0];
        assert_eq!(container.focused_window().unwrap().hwnd, focused_window);
        assert_eq!(
            container.windows().back().map(|window| window.hwnd),
            Some(focused_window)
        );
        assert_eq!(
            workspace.window_focus_history.most_recent(),
            Some(&focused_window)
        );
    }

    #[test]
    fn destroying_the_newest_container_undoes_a_manual_split() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2], area);
        let original = workspace.containers()[0].id.clone();
        let hwnds: Vec<isize> = workspace.containers()[0]
            .windows()
            .iter()
            .map(|window| window.hwnd)
            .collect();

        let created = workspace.create_container_from_donor(None).unwrap();
        workspace.destroy_newest_container().unwrap();

        // The created container is the one which goes, whoever holds the focus, and the workspace
        // is back to one container holding the same windows over the whole work area.
        assert!(workspace.container_idx_for_id(&created).is_none());
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers()[0].id, original);

        let mut left: Vec<isize> = workspace.containers()[0]
            .windows()
            .iter()
            .map(|window| window.hwnd)
            .collect();
        left.sort_unstable();
        let mut expected = hwnds;
        expected.sort_unstable();
        assert_eq!(left, expected);

        workspace.record_logical_slots(area);
        assert_eq!(slot_of(&workspace, &original), LogicalRect::from(area));
    }

    #[test]
    fn destroying_the_newest_container_is_not_destroying_the_focused_one() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2, 1], area);
        let focused = workspace.containers()[0].id.clone();
        workspace.focus_container(0);

        let newest = workspace.containers()[1].id.clone();

        workspace.destroy_newest_container().unwrap();

        assert!(workspace.container_idx_for_id(&newest).is_none());
        assert_eq!(
            workspace
                .focused_container()
                .map(|container| container.id.clone()),
            Some(focused)
        );
    }

    #[test]
    fn the_minimize_history_survives_a_destruction() {
        let mut workspace = workspace_with_containers(&[2, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        workspace.minimize_window(0).unwrap();
        workspace.minimize_window(1).unwrap();

        let before: Vec<isize> = workspace.minimize_history.iter().copied().collect();

        workspace.destroy_container(0).unwrap();

        // The windows are still minimized and still in this workspace, so the order in which they
        // would be restored is unchanged.
        assert_eq!(
            workspace
                .minimize_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            before
        );
        assert_eq!(workspace.restore_last_minimized_window(), Some(1));
    }

    #[test]
    fn resizing_a_container_keeps_the_workspace_tiled() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let id = workspace.containers()[0].id.clone();
        let before = slot_map(&workspace);

        let resize = workspace
            .resize_container(&id, OperationDirection::Right, 120)
            .unwrap();

        assert!(!resize.movers.is_empty());
        assert!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area))
                .is_ok()
        );

        // Only the containers on the boundary moved.
        let after = slot_map(&workspace);
        let moved: Vec<_> = after.keys().filter(|k| after[*k] != before[*k]).collect();

        assert_eq!(moved.len(), resize.movers.len());
    }

    #[test]
    fn a_resize_survives_the_next_reconciliation() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let id = workspace.containers()[0].id.clone();
        workspace
            .resize_container(&id, OperationDirection::Right, 120)
            .unwrap();

        let after = slot_map(&workspace);

        // A deliberate edit is adopted as the arrangement, so reconciling does not undo it.
        workspace.record_logical_slots(area);

        assert_eq!(slot_map(&workspace), after);
    }

    #[test]
    fn a_layout_change_discards_a_manual_resize() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let original = slot_map(&workspace);
        let id = workspace.containers()[0].id.clone();

        workspace
            .resize_container(&id, OperationDirection::Right, 120)
            .unwrap();
        assert_ne!(slot_map(&workspace), original);

        workspace.invalidate_slot_geometry();
        workspace.record_logical_slots(area);

        assert_eq!(slot_map(&workspace), original);
    }

    #[test]
    fn moving_a_boundary_makes_the_restores_it_touches_inexact() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 2);
        workspace.record_logical_slots(area);

        let hidden = workspace.containers()[2].id.clone();
        let absorber = workspace.hidden_slot_restores[&hidden].absorbers[0].clone();

        assert!(workspace.hidden_slot_restores[&hidden].exact_restore_valid);

        // Whichever way this absorber's boundary moves, it no longer holds what the absorption
        // gave it, so the exact reverse is off the table.
        let moved = [OperationDirection::Right, OperationDirection::Left]
            .into_iter()
            .any(|direction| workspace.resize_container(&absorber, direction, 80).is_ok());

        assert!(moved, "the absorber had no boundary to move");
        assert!(!workspace.hidden_slot_restores[&hidden].exact_restore_valid);
    }

    #[test]
    fn a_hidden_container_is_not_a_resize_target() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        let hidden = workspace.containers()[1].id.clone();
        let before = slot_map(&workspace);

        assert!(
            workspace
                .resize_container(&hidden, OperationDirection::Left, 100)
                .is_err()
        );
        assert_eq!(slot_map(&workspace), before);
    }

    #[test]
    fn a_refused_resize_changes_nothing_at_all() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        let before = workspace.clone();
        let id = workspace.containers()[0].id.clone();

        // The left edge of the leftmost container is the work area's own edge.
        assert!(
            workspace
                .resize_container(&id, OperationDirection::Left, 100)
                .is_err()
        );

        assert_eq!(slot_map(&workspace), slot_map(&before));
        assert_eq!(
            workspace.logical_slots.generation(),
            before.logical_slots.generation()
        );
    }

    #[test]
    fn the_resize_target_falls_back_to_an_active_container_when_the_focus_is_hidden() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let area = work_area(1920, 1080);
        workspace.record_logical_slots(area);

        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);
        workspace.focus_container(1);

        // The only active container now covers the whole work area, so there is no boundary and
        // this refuses - but it refuses having looked at the active container, not the hidden one.
        assert!(workspace.active_container_idx_for_geometry() == Some(0));
        assert!(
            workspace
                .resize_focused_container(OperationDirection::Right, 100)
                .is_err()
        );
    }

    /// A workspace whose containers hold exactly the given window handles.
    fn workspace_with_hwnds(containers: &[&[isize]]) -> Workspace {
        let mut workspace = Workspace::default();

        for hwnds in containers {
            let mut container = Container::default();
            for hwnd in *hwnds {
                container.add_window(Window::from(*hwnd));
            }
            workspace.add_container_to_back(container);
        }

        workspace
    }

    fn container_ids(workspace: &Workspace) -> Vec<ContainerId> {
        workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect()
    }

    #[test]
    fn merging_re_parents_every_container_whole() {
        let mut target = workspace_with_hwnds(&[&[1, 2]]);
        let source = workspace_with_hwnds(&[&[3], &[4, 5]]);
        let target_ids = container_ids(&target);
        let source_ids = container_ids(&source);

        target.merge_from(source);

        assert_eq!(
            container_ids(&target),
            [target_ids, source_ids].concat(),
            "containers keep their stable IDs and their relative order"
        );
        assert_eq!(stacks(&target), vec![vec![1, 2], vec![3], vec![4, 5]]);
    }

    #[test]
    fn merging_preserves_the_multi_dimensional_state_of_every_window() {
        let mut target = workspace_with_hwnds(&[&[1]]);
        let mut source = workspace_with_hwnds(&[&[2, 3]]);
        let floating_rect = Rect {
            left: 10,
            top: 20,
            right: 300,
            bottom: 400,
        };

        {
            let container = source.containers_mut().front_mut().unwrap();
            container.windows_mut()[0].set_floating(floating_rect);
            container.windows_mut()[1].set_minimized();
        }
        source.record_minimized_window(3);

        target.merge_from(source);

        let merged = &target.containers()[1];
        assert_eq!(merged.windows()[0].placement, ManagedPlacement::Floating);
        assert_eq!(merged.windows()[0].floating_rect, Some(floating_rect));
        assert_eq!(merged.windows()[1].visibility, Visibility::Minimized);
        assert!(target.minimize_history.contains(&3));
    }

    #[test]
    fn a_container_with_nothing_visible_and_stored_stays_hidden_across_a_merge() {
        let mut target = workspace_with_hwnds(&[&[1]]);
        let mut source = workspace_with_hwnds(&[&[2]]);

        source.containers_mut()[0].windows_mut()[0].set_minimized();
        assert!(source.containers()[0].is_hidden());

        target.merge_from(source);

        assert!(target.containers()[1].is_hidden());
        assert_eq!(target.active_container_count(), 1);
    }

    #[test]
    fn merging_puts_the_source_history_first_and_keeps_each_entry_once() {
        let mut target = workspace_with_hwnds(&[&[1], &[2]]);
        let mut source = workspace_with_hwnds(&[&[3], &[4]]);
        let target_ids = container_ids(&target);
        let source_ids = container_ids(&source);

        target.focus_container(1);
        target.focus_container(0);
        source.focus_container(0);
        source.focus_container(1);
        source.record_minimized_window(3);
        target.record_minimized_window(1);

        target.merge_from(source);

        assert_eq!(
            target
                .container_focus_history
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                source_ids[1].clone(),
                source_ids[0].clone(),
                target_ids[0].clone(),
                target_ids[1].clone()
            ]
        );
        assert_eq!(
            target.minimize_history.iter().copied().collect::<Vec<_>>(),
            vec![3, 1]
        );
    }

    #[test]
    fn merging_inherits_the_focused_window_of_the_workspace_which_was_deleted() {
        let mut target = workspace_with_hwnds(&[&[1]]);
        let mut source = workspace_with_hwnds(&[&[2], &[3, 4]]);

        source.focus_container(1);
        source.containers_mut()[1].focus_window(0);
        let inherited = source.containers()[1].id.clone();

        target.merge_from(source);

        assert_eq!(
            target.focused_container().map(|c| c.id.clone()),
            Some(inherited)
        );
        assert_eq!(
            target
                .focused_container()
                .and_then(|c| c.focused_window())
                .map(|w| w.hwnd),
            Some(3)
        );
    }

    #[test]
    fn an_inherited_container_is_focused_even_when_its_window_cannot_be() {
        let mut target = workspace_with_hwnds(&[&[1]]);
        let mut source = workspace_with_hwnds(&[&[2]]);

        source.containers_mut()[0].windows_mut()[0].set_minimized();
        let inherited = source.containers()[0].id.clone();

        target.merge_from(source);

        assert_eq!(
            target.focused_container().map(|c| c.id.clone()),
            Some(inherited)
        );
    }

    #[test]
    fn merging_into_an_empty_workspace_keeps_the_source_arrangement_order() {
        let mut target = Workspace::default();
        let source = workspace_with_hwnds(&[&[1], &[2]]);
        let source_ids = container_ids(&source);

        target.merge_from(source);

        assert_eq!(container_ids(&target), source_ids);
    }

    #[test]
    fn merging_discards_manual_boundaries_and_every_exact_hidden_restore() {
        let mut target = workspace_with_hwnds(&[&[1], &[2]]);
        let source = workspace_with_hwnds(&[&[3]]);

        target.record_logical_slots(work_area(1920, 1080));
        target.resize_dimensions = vec![
            Some(Rect {
                left: 0,
                top: 0,
                right: 40,
                bottom: 0,
            }),
            None,
        ];
        target.hidden_slot_restores.insert(
            target.containers()[0].id.clone(),
            HiddenSlotRestore {
                old_rect: LogicalRect::from(work_area(100, 100)),
                direction: OperationDirection::Left,
                absorbers: vec![],
                absorber_rects_before: vec![],
                geometry_generation: 0,
                exact_restore_valid: true,
            },
        );

        target.merge_from(source);

        assert_eq!(target.resize_dimensions, vec![None, None, None]);
        assert!(target.hidden_slot_restores.is_empty());
        assert!(target.relayout_pending);
    }

    fn workspace_with_containers(counts: &[usize]) -> Workspace {
        let mut workspace = Workspace::default();
        let mut hwnd = 0;

        for count in counts {
            let mut container = Container::default();
            for _ in 0..*count {
                container.add_window(Window::from(hwnd));
                hwnd += 1;
            }
            workspace.add_container_to_back(container);
        }

        workspace
    }

    #[test]
    fn focusing_a_container_records_it_as_most_recent() {
        let mut workspace = workspace_with_containers(&[1, 1, 1]);
        let ids = workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect::<Vec<_>>();

        workspace.focus_container(0);
        workspace.focus_container(2);

        assert_eq!(
            workspace
                .container_focus_history
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]
        );
    }

    #[test]
    fn a_preselect_container_never_enters_the_focus_history() {
        let mut workspace = workspace_with_containers(&[1]);
        let before = workspace.container_focus_history.len();

        workspace.preselect_container_idx(0);

        assert_eq!(workspace.container_focus_history.len(), before);
        assert!(
            !workspace
                .container_focus_history
                .contains(&"PRESELECT".into())
        );
    }

    #[test]
    fn recording_a_window_focus_updates_both_histories() {
        let mut workspace = workspace_with_containers(&[2, 2]);
        let first_id = workspace.containers()[0].id.clone();

        assert!(workspace.record_focused_window(0));

        assert_eq!(workspace.focused_container_idx(), 0);
        assert_eq!(
            workspace.container_focus_history.most_recent(),
            Some(&first_id)
        );
        assert_eq!(
            workspace.containers()[0].focus_history().most_recent(),
            Some(&0)
        );

        assert!(!workspace.record_focused_window(404));
        assert_eq!(workspace.focused_container_idx(), 0);
    }

    #[test]
    fn focus_selection_uses_the_most_recent_container_with_a_focusable_window() {
        let mut workspace = workspace_with_containers(&[2, 1]);

        workspace.record_focused_window(2);
        workspace.record_focused_window(1);

        assert_eq!(workspace.focus_target_from_history(), Some((0, 1)));

        // The whole most recent container is minimized, so the next one in history wins.
        for window in workspace.containers_mut()[0].windows_mut() {
            window.set_minimized();
        }

        assert_eq!(workspace.focus_target_from_history(), Some((1, 2)));

        for window in workspace.containers_mut()[1].windows_mut() {
            window.set_minimized();
        }

        assert_eq!(workspace.focus_target_from_history(), None);
    }

    #[test]
    fn focus_selection_still_answers_without_any_history() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        workspace.container_focus_history.clear();

        assert_eq!(workspace.focus_target_from_history(), Some((0, 0)));
    }

    #[test]
    fn removing_a_container_drops_its_workspace_history_entries() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let removed_id = workspace.containers()[1].id.clone();

        workspace.record_focused_window(1);
        workspace.record_minimized_window(1);
        workspace.remove_container_by_idx(1);

        assert!(!workspace.container_focus_history.contains(&removed_id));
        assert!(!workspace.minimize_history.contains(&1));
    }

    #[test]
    fn removing_the_last_window_of_a_container_clears_both_levels() {
        let mut workspace = workspace_with_containers(&[1, 1]);
        let removed_id = workspace.containers()[1].id.clone();

        workspace.record_minimized_window(1);
        workspace.remove_window(1).unwrap();

        assert_eq!(workspace.containers().len(), 1);
        assert!(!workspace.container_focus_history.contains(&removed_id));
        assert!(!workspace.minimize_history.contains(&1));
    }

    #[test]
    fn the_minimize_history_returns_owned_minimized_windows_most_recent_first() {
        let mut workspace = workspace_with_containers(&[3]);

        for hwnd in 0..3 {
            workspace.containers_mut()[0].windows_mut()[hwnd].set_minimized();
            workspace.record_minimized_window(hwnd as isize);
        }

        assert_eq!(workspace.take_last_minimized_window(), Some(2));
        assert_eq!(workspace.take_last_minimized_window(), Some(1));
        assert_eq!(workspace.take_last_minimized_window(), Some(0));
        assert_eq!(workspace.take_last_minimized_window(), None);
    }

    #[test]
    fn the_minimize_history_skips_windows_which_are_no_longer_minimized() {
        let mut workspace = workspace_with_containers(&[2]);

        workspace.containers_mut()[0].windows_mut()[0].set_minimized();
        workspace.record_minimized_window(0);
        workspace.record_minimized_window(1);
        workspace.record_minimized_window(404);

        assert_eq!(workspace.take_last_minimized_window(), Some(0));
        assert!(workspace.minimize_history.is_empty());
    }

    #[test]
    fn recording_a_minimized_window_twice_does_not_duplicate_it() {
        let mut workspace = workspace_with_containers(&[2]);

        workspace.record_minimized_window(1);
        workspace.record_minimized_window(0);
        workspace.record_minimized_window(1);

        assert_eq!(
            workspace
                .minimize_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert!(workspace.forget_minimized_window(1));
        assert!(!workspace.forget_minimized_window(1));
    }

    #[test]
    fn pruning_drops_history_entries_for_objects_which_left_the_workspace() {
        let mut workspace = workspace_with_containers(&[2, 1]);

        workspace.container_focus_history.record("gone".into());
        workspace.minimize_history.record(404);

        workspace.prune_histories();

        assert!(!workspace.container_focus_history.contains(&"gone".into()));
        assert_eq!(workspace.container_focus_history.len(), 2);
        assert!(workspace.minimize_history.is_empty());
        assert_eq!(workspace.containers()[0].focus_history().len(), 2);
    }

    #[test]
    fn histories_survive_serialization_and_legacy_json_is_accepted() {
        let mut workspace = workspace_with_containers(&[2]);
        workspace.record_focused_window(0);
        workspace.record_minimized_window(1);

        let mut json = serde_json::to_value(&workspace).unwrap();
        let restored: Workspace = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(
            restored.container_focus_history,
            workspace.container_focus_history
        );
        assert_eq!(restored.minimize_history, workspace.minimize_history);

        let object = json.as_object_mut().unwrap();
        object.remove("container_focus_history");
        object.remove("minimize_history");
        let legacy: Workspace = serde_json::from_value(json).unwrap();

        assert!(legacy.container_focus_history.is_empty());
        assert!(legacy.minimize_history.is_empty());
    }

    #[test]
    fn test_locked_containers_with_new_window() {
        let mut ws = Workspace::default();

        let mut state = HashMap::new();

        // add 4 containers
        for i in 0..4 {
            let mut container = Container::default();
            if i == 3 {
                container.locked = true; // set index 3 locked
            }
            state.insert(i, container.id.to_string());
            ws.add_container_to_back(container);
        }
        assert_eq!(ws.containers().len(), 4);

        // focus container at index 2
        ws.focus_container(2);

        // simulate a new window being launched on this workspace
        ws.new_container_for_window(Window::from(123));

        // new length should be 5, with the focus on the new window at index 4
        assert_eq!(ws.containers().len(), 5);
        assert_eq!(ws.focused_container_idx(), 4);
        assert_eq!(
            ws.focused_container()
                .unwrap()
                .focused_window()
                .unwrap()
                .hwnd,
            123
        );

        // when inserting a new container at index 0, index 3's container should not change
        ws.focus_container(0);
        ws.new_container_for_window(Window::from(234));
        assert_eq!(
            ws.containers()[3].id.to_string(),
            state.get(&3).unwrap().to_string()
        );
    }

    #[test]
    fn test_locked_containers_remove_window() {
        let mut ws = Workspace::default();

        // add 4 containers
        for i in 0..4 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            if i == 1 {
                container.locked = true;
            }
            ws.add_container_to_back(container);
        }
        assert_eq!(ws.containers().len(), 4);

        ws.remove_window(0).unwrap();
        assert_eq!(ws.containers()[0].focused_window().unwrap().hwnd, 2);
        // index 1 should still be the same
        assert_eq!(ws.containers()[1].focused_window().unwrap().hwnd, 1);
        assert_eq!(ws.containers()[2].focused_window().unwrap().hwnd, 3);
    }

    #[test]
    fn test_locked_containers_toggle_float() {
        let mut ws = Workspace::default();

        // add 4 containers
        for i in 0..4 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            if i == 1 {
                container.locked = true;
            }
            ws.add_container_to_back(container);
        }
        assert_eq!(ws.containers().len(), 4);

        // set index 0 focused
        ws.focus_container(0);

        // float index 0
        ws.new_floating_window().unwrap();

        // Floating keeps the window in the container which owns it, so no container is created,
        // destroyed or reordered and no locked index can be disturbed by it.
        assert_eq!(ws.containers().len(), 4);
        assert_eq!(ws.floating_windows(), vec![Window::from(0)]);
        assert!(ws.containers()[0].is_hidden());

        ws.unfloat_window(0).unwrap();

        assert!(ws.floating_windows().is_empty());
        assert!(ws.containers()[0].is_active());

        // all indexes are still at their original position
        for i in 0..4 {
            assert_eq!(
                ws.containers()[i].focused_window().unwrap().hwnd,
                i as isize
            );
        }
    }

    #[test]
    fn test_locked_containers_stack() {
        let mut ws = Workspace::default();

        // add 6 containers
        for i in 0..6 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            if i == 4 {
                container.locked = true;
            }
            ws.add_container_to_back(container);
        }
        assert_eq!(ws.containers().len(), 6);

        // set index 3 focused
        ws.focus_container(3);

        // stack index 3 on top of index 2
        ws.move_window_to_container(2).unwrap();

        assert_eq!(ws.containers()[0].focused_window().unwrap().hwnd, 0);
        assert_eq!(ws.containers()[1].focused_window().unwrap().hwnd, 1);
        assert_eq!(ws.containers()[2].windows().len(), 2);
        assert_eq!(ws.containers()[3].focused_window().unwrap().hwnd, 5);
        // index 4 should still be the same
        assert_eq!(ws.containers()[4].focused_window().unwrap().hwnd, 4);

        // unstack
        ws.new_container_for_focused_window().unwrap();

        // all indexes should be at their original position
        for i in 0..6 {
            assert_eq!(
                ws.containers()[i].focused_window().unwrap().hwnd,
                i as isize
            )
        }
    }

    #[test]
    fn test_contains_window() {
        // Create default workspace
        let mut workspace = Workspace::default();

        // Add a window to the container
        let mut container = Container::default();
        container.windows_mut().push_back(Window::from(0));

        // Add container
        workspace.add_container_to_back(container);

        // Should be true
        assert!(workspace.contains_window(0));

        // Should be false
        assert!(!workspace.is_empty())
    }

    #[test]
    fn test_add_container_to_back() {
        let mut workspace = Workspace::default();

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }
        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        // Get focused container. Should be the index of the last container added
        let container = workspace.focused_container_mut().unwrap();

        // Should be focused on the container with 1 window
        assert_eq!(container.windows().len(), 1);
    }

    #[test]
    fn test_add_container_to_front() {
        let mut workspace = Workspace::default();

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_front(container);
        }

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_front(container);
        }
        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        // Get focused container. Should be the index of the last container added
        let container = workspace.focused_container_mut().unwrap();

        // Should be focused on the container with 3 windows
        assert_eq!(container.windows().len(), 3);
    }

    #[test]
    fn test_remove_non_existent_window() {
        let mut workspace = Workspace::default();

        {
            // Add a container with one window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }

        // Attempt to remove a non-existent window
        let result = workspace.remove_window(2);

        // Should return an error
        assert!(
            result.is_err(),
            "Expected an error when removing a non-existent window"
        );

        // Get focused container. Should be the index of the last container added
        let container = workspace.focused_container_mut().unwrap();

        // Should still have 1 window
        assert_eq!(container.windows().len(), 1);
    }

    #[test]
    fn test_remove_focused_container() {
        let mut workspace = Workspace::default();

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }
        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        // Should be focused on the container at index 1
        assert_eq!(workspace.focused_container_idx(), 1);

        // Store the container at index 1 before removal
        let container_to_remove = workspace.containers().get(1).cloned();
        workspace.remove_focused_container();

        // Should only have 1 container
        assert_eq!(workspace.containers().len(), 1);

        // Should be focused on the container at index 0
        assert_eq!(workspace.focused_container_idx(), 0);

        // Ensure the container at index 1 before removal is no longer present
        assert!(container_to_remove.is_some());
        assert!(
            !workspace
                .containers()
                .contains(&container_to_remove.unwrap())
        );
    }

    #[test]
    fn test_insert_container_at_idx() {
        let mut workspace = Workspace::default();

        for i in 0..4 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 4 containers
        assert_eq!(workspace.containers().len(), 4);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 3);

        // Insert a container at index 4
        workspace.insert_container_at_idx(4, Container::default());

        // Should have 5 containers
        assert_eq!(workspace.containers().len(), 5);

        // Should be focused on the newly inserted container
        assert_eq!(workspace.focused_container_idx(), 4);
    }

    #[test]
    fn test_remove_container_by_idx() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 2);

        // Store the container at index 1 before removal
        let container_to_remove = workspace.containers().get(1).cloned();

        // Remove the container at index 1
        workspace.remove_container_by_idx(1);

        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        // Ensure the container at index 1 before removal is no longer present
        assert!(container_to_remove.is_some());
        assert!(
            !workspace
                .containers()
                .contains(&container_to_remove.unwrap())
        );
    }

    #[test]
    fn test_remove_container() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 2);

        // Store the container at index 2 before removal
        let container_to_remove = workspace.containers().get(2).cloned();

        // Remove the container at index 2
        workspace.remove_container(2);

        // Should be focused on the previous container which is index 1
        assert_eq!(workspace.focused_container_idx(), 1);

        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        // Ensure the container at index 1 before removal is no longer present
        assert!(container_to_remove.is_some());
        assert!(
            !workspace
                .containers()
                .contains(&container_to_remove.unwrap())
        );
    }

    #[test]
    fn test_focus_container() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 2);

        // Focus on container 1
        workspace.focus_container(1);
        assert_eq!(workspace.focused_container_idx(), 1);

        // Focus on container 0
        workspace.focus_container(0);
        assert_eq!(workspace.focused_container_idx(), 0);

        // Focus on container 2
        workspace.focus_container(2);
        assert_eq!(workspace.focused_container_idx(), 2);
    }

    #[test]
    fn test_focus_previous_container() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 2);

        // Focus on the previous container
        workspace.focus_previous_container();

        // Should be focused on container 1
        assert_eq!(workspace.focused_container_idx(), 1);
    }

    #[test]
    fn test_focus_last_container() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Change focus to the first container for the test
        workspace.focus_container(0);
        assert_eq!(workspace.focused_container_idx(), 0);

        // Focus on the last container
        workspace.focus_last_container();

        // Should be focused on container 1
        assert_eq!(workspace.focused_container_idx(), 2);
    }

    #[test]
    fn test_focus_first_container() {
        let mut workspace = Workspace::default();

        for i in 0..3 {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(i));
            workspace.add_container_to_back(container);
        }

        // Should have 3 containers
        assert_eq!(workspace.containers().len(), 3);

        // Should be focused on the last container
        assert_eq!(workspace.focused_container_idx(), 2);

        // Focus on the first container
        workspace.focus_first_container();

        // Should be focused on container 1
        assert_eq!(workspace.focused_container_idx(), 0);
    }

    #[test]
    fn test_swap_containers() {
        let mut workspace = Workspace::default();

        {
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }

        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        {
            // Should be focused on container 1
            assert_eq!(workspace.focused_container_idx(), 1);

            // Should have 1 window
            let container = workspace.focused_container_mut().unwrap();
            assert_eq!(container.windows().len(), 1);
        }

        // Swap containers 0 and 1
        workspace.swap_containers(0, 1);

        {
            // Should be focused on container 0
            assert_eq!(workspace.focused_container_idx(), 1);

            let container = workspace.focused_container_mut().unwrap();
            assert_eq!(container.windows().len(), 3);
        }
    }

    #[test]
    fn test_new_container_for_window() {
        let mut workspace = Workspace::default();

        {
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(1));
            workspace.add_container_to_back(container);
        }

        // Add new window to container
        workspace.new_container_for_window(Window::from(2));

        // Container 0 should have 1 window
        let container = workspace.focused_container_mut().unwrap();
        assert_eq!(container.windows().len(), 1);
        assert_eq!(container.windows()[0].container_id, container.id);

        // Should return true that window 2 exists
        assert!(workspace.contains_window(2));
    }

    #[test]
    fn test_move_window_to_container() {
        let mut workspace = Workspace::default();

        {
            // Container with 0 windows
            let container = Container::default();
            workspace.add_container_to_back(container);
        }

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        // Move A Window from container 1 to container 0
        workspace.move_window_to_container(0).unwrap();

        // Focus on container 0
        workspace.focus_container(0);

        // Container 0 should have 1 window
        let container = workspace.focused_container_mut().unwrap();
        assert_eq!(container.windows().len(), 1);
    }

    #[test]
    fn moving_window_to_container_preserves_state_and_reassigns_owner() {
        let mut workspace = Workspace::default();
        let target = Container::default();
        let target_id = target.id.clone();
        workspace.add_container_to_back(target);

        let mut source = Container::default();
        let mut window =
            ManagedWindow::from_observed(Window::from(42), source.id.clone(), true, true, false);
        window.set_floating(Default::default());
        source.add_managed_window(window);
        workspace.add_container_to_back(source);
        workspace.focus_container(1);

        workspace.move_window_to_container(0).unwrap();

        let moved = &workspace.containers()[0].windows()[0];
        assert_eq!(moved.container_id, target_id);
        assert_eq!(moved.placement, crate::ManagedPlacement::Floating);
        assert_eq!(moved.visibility, crate::Visibility::Minimized);
        assert_eq!(moved.presentation, crate::Presentation::Maximized);
    }

    #[test]
    fn test_move_window_to_non_existent_container() {
        let mut workspace = Workspace::default();

        // Add a container with one window
        let mut container = Container::default();
        container.windows_mut().push_back(Window::from(1));
        workspace.add_container_to_back(container);

        // Try to move window to a non-existent container
        let result = workspace.move_window_to_container(8);

        // Should return an error
        assert!(
            result.is_err(),
            "Expected an error when moving a window to a non-existent container"
        );
    }

    #[test]
    fn test_remove_window() {
        let mut workspace = Workspace::default();

        {
            // Container with 1 window
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        // Remove window 1
        workspace.remove_window(1).ok();

        // Should have 2 windows
        let container = workspace.focused_container_mut().unwrap();
        assert_eq!(container.windows().len(), 2);

        // Check that window 1 is removed
        assert!(!workspace.contains_window(1));
    }

    #[test]
    fn detach_last_window_removes_its_container() {
        let mut workspace = Workspace::default();
        let mut container = Container::default();
        container.windows_mut().push_back(Window::from(42));
        workspace.add_container_to_back(container);

        workspace.detach_window(42).unwrap();

        assert!(workspace.containers().is_empty());
        assert!(!workspace.contains_window(42));
    }

    #[test]
    fn detach_removes_a_floating_window_through_its_container() {
        let mut workspace = workspace_with_containers(&[1]);
        workspace.float_window(0, Rect::default()).unwrap();

        workspace.detach_window(0).unwrap();

        // A floating window is the last window of its container here, so detaching it destroys
        // the container exactly as detaching a stored window would.
        assert!(workspace.floating_windows().is_empty());
        assert!(workspace.containers().is_empty());
    }

    fn workspace_with_stack(hwnds: &[isize]) -> Workspace {
        let mut workspace = Workspace::default();
        let mut container = Container::default();

        for hwnd in hwnds {
            container.windows_mut().push_back(Window::from(*hwnd));
        }

        workspace.add_container_to_back(container);
        workspace
    }

    #[test]
    fn raising_the_next_stack_window_records_it_in_both_histories() {
        let mut workspace = workspace_with_stack(&[1, 2, 3]);
        let container_id = workspace.containers()[0].id.clone();

        assert_eq!(workspace.raise_next_stack_window(), Some(2));

        let container = &workspace.containers()[0];
        let order: Vec<isize> = container.windows().iter().map(|w| w.hwnd).collect();

        assert_eq!(order, vec![1, 3, 2], "only the depth of one window changed");
        assert_eq!(container.id, container_id, "no container was created");
        assert_eq!(container.focused_window().map(|w| w.hwnd), Some(2));
        assert_eq!(container.focus_history().iter().next(), Some(&2));
        assert_eq!(
            workspace.container_focus_history.iter().next(),
            Some(&container_id)
        );
    }

    #[test]
    fn raising_the_next_stack_window_needs_something_under_the_top() {
        let mut workspace = workspace_with_stack(&[1]);

        assert_eq!(workspace.raise_next_stack_window(), None);
        assert_eq!(Workspace::default().raise_next_stack_window(), None);
    }

    #[test]
    fn maximizing_keeps_the_window_in_its_container() {
        let mut workspace = workspace_with_stack(&[1, 2, 3]);
        let container_id = workspace.containers()[0].id.clone();

        workspace.maximize_focused_window().unwrap();

        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers()[0].id, container_id);
        assert_eq!(workspace.containers()[0].windows().len(), 3);
        assert_eq!(workspace.maximized_window(), Some(Window::from(1)));
        assert!(workspace.is_maximized_window(1));

        // A maximized window is still a visible stored window, so its container keeps its slot.
        assert!(workspace.containers()[0].is_active());
        assert_eq!(workspace.active_container_count(), 1);
    }

    #[test]
    fn a_window_restored_outside_komorebi_stops_being_recorded_as_maximized() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let container_id = workspace.containers()[0].id.clone();
        workspace.maximize_window(1).unwrap();

        assert!(workspace.reconcile_window_presentation(1, Presentation::Normal, None));

        let container = &workspace.containers()[0];
        assert_eq!(container.windows()[0].presentation, Presentation::Normal);
        // Only the presentation moved: the window is where it was, in the container it was in.
        assert_eq!(container.id, container_id);
        assert_eq!(
            container
                .windows()
                .iter()
                .map(|w| w.hwnd)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(container.is_active());
    }

    #[test]
    fn reconciling_the_same_observation_twice_only_changes_the_record_once() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.maximize_window(1).unwrap();

        assert!(workspace.reconcile_window_presentation(1, Presentation::Normal, None));
        assert!(!workspace.reconcile_window_presentation(1, Presentation::Normal, None));
    }

    #[test]
    fn an_observation_does_not_maximize_a_window_komorebi_is_tiling() {
        let mut workspace = workspace_with_stack(&[1]);

        assert!(!workspace.reconcile_window_presentation(1, Presentation::Maximized, None));
        assert_eq!(
            workspace.containers()[0].windows()[0].presentation,
            Presentation::Normal
        );
    }

    #[test]
    fn reconciling_a_window_this_workspace_does_not_own_changes_nothing() {
        let mut workspace = workspace_with_stack(&[1]);

        assert!(!workspace.reconcile_window_presentation(99, Presentation::Normal, None));
    }

    #[test]
    fn maximizing_does_not_change_the_stack_or_the_owning_container_id() {
        let mut workspace = workspace_with_stack(&[1, 2, 3]);
        let owner = workspace.containers()[0].id.clone();

        workspace.maximize_window(2).unwrap();

        let container = &workspace.containers()[0];
        let order: Vec<isize> = container.windows().iter().map(|w| w.hwnd).collect();
        assert_eq!(order, vec![1, 2, 3]);

        for window in container.windows() {
            assert_eq!(window.container_id, owner);
        }

        assert_eq!(workspace.maximized_window(), Some(Window::from(2)));
    }

    #[test]
    fn maximizing_twice_is_idempotent_and_keeps_the_first_restore_rectangle() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.containers_mut()[0].windows_mut()[0].restore_rect = None;

        workspace.maximize_window(1).unwrap();
        let first = workspace.containers()[0].windows()[0].restore_rect;

        workspace.maximize_window(1).unwrap();

        let window = &workspace.containers()[0].windows()[0];
        assert_eq!(window.presentation, Presentation::Maximized);
        assert_eq!(window.restore_rect, first);
        assert_eq!(workspace.containers()[0].windows().len(), 1);
    }

    #[test]
    fn unmaximizing_returns_the_window_to_normal_without_moving_it() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let container_id = workspace.containers()[0].id.clone();

        workspace.maximize_window(1).unwrap();
        workspace.unmaximize_window().unwrap();

        assert_eq!(workspace.maximized_window(), None);
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers()[0].id, container_id);
        assert_eq!(workspace.containers()[0].windows().len(), 2);

        let window = &workspace.containers()[0].windows()[0];
        assert_eq!(window.presentation, Presentation::Normal);
        assert_eq!(window.restore_rect, None);
        assert_eq!(window.placement, ManagedPlacement::Stored);
    }

    #[test]
    fn unmaximizing_without_a_maximized_window_changes_nothing() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let before = workspace.clone();

        assert!(workspace.unmaximize_window().is_err());
        assert_eq!(workspace.containers(), before.containers());
    }

    #[test]
    fn maximize_refuses_a_window_this_workspace_does_not_own() {
        let mut workspace = workspace_with_stack(&[1]);

        assert!(workspace.maximize_window(99).is_err());
        assert_eq!(workspace.maximized_window(), None);
        assert_eq!(workspace.containers()[0].windows().len(), 1);
    }

    #[test]
    fn maximizing_a_floating_window_keeps_its_placement_and_rectangle() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let rect = Rect {
            left: 10,
            top: 20,
            right: 300,
            bottom: 400,
        };
        workspace.float_window(1, rect).unwrap();

        workspace.maximize_window(1).unwrap();

        let window = &workspace.containers()[0].windows()[0];
        assert_eq!(window.placement, ManagedPlacement::Floating);
        assert_eq!(window.floating_rect, Some(rect));
        assert_eq!(window.presentation, Presentation::Maximized);

        workspace.unmaximize_window().unwrap();

        let window = &workspace.containers()[0].windows()[0];
        assert_eq!(window.placement, ManagedPlacement::Floating);
        assert_eq!(window.floating_rect, Some(rect));
        assert_eq!(window.presentation, Presentation::Normal);
    }

    #[test]
    fn fullscreen_keeps_the_window_in_its_container_and_is_idempotent() {
        let mut workspace = workspace_with_stack(&[1, 2, 3]);
        let container_id = workspace.containers()[0].id.clone();

        workspace.fullscreen_focused_window().unwrap();
        workspace.fullscreen_focused_window().unwrap();

        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers()[0].id, container_id);
        assert_eq!(workspace.containers()[0].windows().len(), 3);
        assert_eq!(workspace.fullscreened_window(), Some(Window::from(1)));
        assert_eq!(workspace.maximized_window(), None);
        assert_eq!(workspace.presented_window(), Some(Window::from(1)));

        workspace.unfullscreen_window().unwrap();

        assert_eq!(workspace.fullscreened_window(), None);
        assert_eq!(workspace.presented_window(), None);
        assert_eq!(workspace.containers()[0].id, container_id);
        assert_eq!(workspace.containers()[0].windows().len(), 3);
        assert_eq!(
            workspace.containers()[0].windows()[0].presentation,
            Presentation::Normal
        );
    }

    #[test]
    fn the_two_presentations_do_not_answer_each_other() {
        let mut workspace = workspace_with_stack(&[1]);

        workspace.fullscreen_window(1).unwrap();
        assert!(workspace.unmaximize_window().is_err());
        assert_eq!(workspace.fullscreened_window(), Some(Window::from(1)));

        workspace.unfullscreen_window().unwrap();

        workspace.maximize_window(1).unwrap();
        assert!(workspace.unfullscreen_window().is_err());
        assert_eq!(workspace.maximized_window(), Some(Window::from(1)));
    }

    #[test]
    fn switching_presentation_keeps_the_pre_presentation_restore_rect() {
        let mut workspace = workspace_with_stack(&[1]);

        workspace.maximize_window(1).unwrap();
        let restore = workspace.containers()[0].windows()[0].restore_rect;

        workspace.fullscreen_window(1).unwrap();

        let window = &workspace.containers()[0].windows()[0];
        assert_eq!(window.presentation, Presentation::Fullscreen);
        assert_eq!(window.restore_rect, restore);
        assert_eq!(workspace.maximized_window(), None);
    }

    #[test]
    fn fullscreen_uses_the_monitor_bounds_not_the_work_area() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.globals.work_area = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1000,
        };
        workspace.globals.monitor_size = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };

        assert_eq!(workspace.fullscreen_rect(), workspace.globals.monitor_size);
    }

    #[test]
    fn a_fullscreen_window_blocks_a_move_of_its_own_container_only() {
        let mut workspace = workspace_with_stack(&[1]);
        let mut second = Container::default();
        second.windows_mut().push_back(Window::from(2));
        workspace.add_container_to_back(second);

        workspace.fullscreen_window(1).unwrap();

        workspace.focus_container(1);
        assert!(!workspace.focused_container_has_presented_window());

        workspace.focus_container(0);
        assert!(workspace.focused_container_has_presented_window());
    }

    #[test]
    fn floating_a_window_leaves_either_presentation_first() {
        for presentation in [Presentation::Maximized, Presentation::Fullscreen] {
            let mut workspace = workspace_with_stack(&[1, 2]);
            workspace.enter_presentation(1, presentation).unwrap();

            workspace.new_floating_window().unwrap();

            assert_eq!(workspace.presented_window(), None);
            assert_eq!(workspace.containers().len(), 1);
            assert_eq!(
                workspace.containers()[0].windows()[0].placement,
                ManagedPlacement::Floating
            );
        }
    }

    #[test]
    fn a_minimized_window_is_never_a_maximize_subject() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.minimize_window(1).unwrap();

        assert!(workspace.maximize_focused_window().is_err());
        assert_eq!(workspace.maximized_window(), None);
        assert_eq!(
            workspace.containers()[0].windows()[0].visibility,
            Visibility::Minimized
        );
    }

    #[test]
    fn a_maximized_window_only_blocks_a_move_of_its_own_container() {
        let mut workspace = workspace_with_stack(&[1]);
        let mut second = Container::default();
        second.windows_mut().push_back(Window::from(2));
        workspace.add_container_to_back(second);

        workspace.maximize_window(1).unwrap();

        workspace.focus_container(1);
        assert!(!workspace.focused_container_has_presented_window());

        workspace.focus_container(0);
        assert!(workspace.focused_container_has_presented_window());
    }

    #[test]
    fn maximizing_keeps_the_window_in_both_focus_histories() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let container_id = workspace.containers()[0].id.clone();

        workspace.maximize_window(2).unwrap();

        assert_eq!(
            workspace.container_focus_history.iter().next(),
            Some(&container_id)
        );
        assert_eq!(
            workspace.containers()[0].focus_history().most_recent(),
            Some(&2)
        );
    }

    #[test]
    fn monocle_keeps_the_container_in_the_ring_with_its_identity() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        let mut second = Container::default();
        second.windows_mut().push_back(Window::from(3));
        workspace.add_container_to_back(second);

        let first_id = workspace.containers()[0].id.clone();
        let second_id = workspace.containers()[1].id.clone();

        workspace.focus_container(0);
        workspace.new_monocle_container().unwrap();

        assert!(workspace.is_monocle());
        assert_eq!(workspace.containers().len(), 2);
        assert_eq!(workspace.containers()[0].id, first_id);
        assert_eq!(workspace.containers()[1].id, second_id);
        assert_eq!(
            workspace.monocle_container().map(|c| c.id.clone()),
            Some(first_id.clone())
        );
        assert_eq!(workspace.monocle_container_idx(), Some(0));

        workspace.reintegrate_monocle_container().unwrap();

        assert!(!workspace.is_monocle());
        assert_eq!(workspace.containers().len(), 2);
        assert_eq!(workspace.containers()[0].id, first_id);
        assert_eq!(workspace.containers()[0].windows().len(), 2);
    }

    #[test]
    fn monocle_gives_its_container_the_whole_work_area_and_no_other_container_a_slot() {
        let mut workspace = workspace_with_stack(&[1]);
        let mut second = Container::default();
        second.windows_mut().push_back(Window::from(2));
        workspace.add_container_to_back(second);

        let area = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        workspace.record_logical_slots(area);
        assert_eq!(workspace.logical_slots.len(), 2);

        workspace.focus_container(1);
        workspace.new_monocle_container().unwrap();
        let monocle_idx = workspace.monocle_container_idx().unwrap();
        workspace.record_monocle_slot(monocle_idx, area);

        assert_eq!(workspace.logical_slots.len(), 1);
        assert_eq!(workspace.logical_slot_at(1), Some(LogicalRect::from(area)));
        assert_eq!(workspace.logical_slot_at(0), None);
    }

    #[test]
    fn cycling_monocle_moves_the_reference_without_moving_any_container() {
        let mut workspace = workspace_with_stack(&[1]);
        let mut second = Container::default();
        second.windows_mut().push_back(Window::from(2));
        workspace.add_container_to_back(second);

        let ids: Vec<_> = workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect();

        workspace.focus_container(0);
        workspace.new_monocle_container().unwrap();
        assert_eq!(workspace.monocle_container_idx(), Some(0));

        workspace
            .cycle_monocle_container(CycleDirection::Next)
            .unwrap();

        assert_eq!(workspace.monocle_container_idx(), Some(1));
        assert_eq!(workspace.containers().len(), 2);
        let after: Vec<_> = workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect();
        assert_eq!(ids, after);
    }

    #[test]
    fn reintegrating_without_a_monocle_container_changes_nothing() {
        let mut workspace = workspace_with_stack(&[1]);
        let before = workspace.clone();

        assert!(workspace.reintegrate_monocle_container().is_err());
        assert_eq!(workspace.containers(), before.containers());
        assert!(!workspace.is_monocle());
    }

    #[test]
    fn a_monocle_container_still_owns_its_windows_for_every_lookup() {
        let mut workspace = workspace_with_stack(&[1, 2]);
        workspace.new_monocle_container().unwrap();

        assert!(workspace.contains_window(1));
        assert!(workspace.contains_managed_window(2));
        assert_eq!(workspace.container_idx_for_window(2), Some(0));
        assert!(!workspace.is_empty());
    }

    #[test]
    fn losing_the_last_window_of_the_monocle_container_clears_the_reference() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.new_monocle_container().unwrap();
        assert!(workspace.is_monocle());

        workspace.remove_window(1).unwrap();

        assert!(workspace.containers().is_empty());
        assert!(!workspace.is_monocle());
        assert!(workspace.monocle_container_id.is_none());
    }

    #[test]
    fn a_monocle_reference_to_a_dropped_container_is_pruned() {
        let mut workspace = workspace_with_stack(&[1]);
        workspace.monocle_container_id = Some(ContainerId::from("gone"));

        workspace.prune_monocle_reference();

        assert!(workspace.monocle_container_id.is_none());
        assert!(workspace.monocle_container().is_none());
    }

    #[test]
    fn a_workspace_without_a_monocle_id_still_deserializes() {
        let json = r#"{
            "name": null,
            "containers": { "elements": [], "focused": 0 },
            "layout": { "Default": "BSP" },
            "layout_options": null,
            "layout_rules": [],
            "work_area_offset_rules": [],
            "layout_flip": null,
            "workspace_padding": 10,
            "container_padding": 10,
            "latest_layout": [],
            "resize_dimensions": [],
            "tile": true,
            "work_area_offset": null,
            "apply_window_based_work_area_offset": true,
            "window_container_behaviour": null,
            "window_container_behaviour_rules": null,
            "float_override": null,
            "layer": "Tiling",
            "floating_layer_behaviour": null,
            "wallpaper": null,
            "monocle_container": { "id": "legacy", "windows": { "elements": [], "focused": 0 } },
            "monocle_container_restore_idx": 3
        }"#;

        let workspace: Workspace = serde_json::from_str(json).unwrap();

        // The legacy workspace-owned monocle container is ignored rather than migrated, exactly
        // as the legacy workspace-owned floating window list is.
        assert!(workspace.monocle_container_id.is_none());
        assert!(!workspace.is_monocle());
    }

    #[test]
    fn detach_removes_alternate_legacy_ownership_paths() {
        let mut monocle_container = Container::default();
        monocle_container.windows_mut().push_back(Window::from(44));
        let mut monocle_workspace = Workspace::default();
        monocle_workspace.add_container_to_back(monocle_container);
        monocle_workspace.new_monocle_container().unwrap();
        assert!(monocle_workspace.is_monocle());

        // Detaching the last window of the monocle container destroys that container, and the
        // monocle reference goes with it rather than dangling.
        monocle_workspace.detach_window(44).unwrap();
        assert!(monocle_workspace.containers().is_empty());
        assert!(monocle_workspace.monocle_container().is_none());
        assert!(monocle_workspace.monocle_container_id.is_none());
    }

    #[test]
    fn test_new_container_for_focused_window() {
        let mut workspace = Workspace::default();

        {
            // Container with 1 window
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        // Add focused window to new container
        workspace.new_container_for_focused_window().ok();

        // Should have 2 containers
        assert_eq!(workspace.containers().len(), 2);

        {
            // Inspect new container. Should contain 1 window. Window name should be 0
            workspace.focus_container(1);
            let container = workspace.focused_container_mut().unwrap();
            assert_eq!(container.windows().len(), 1);
            assert!(workspace.contains_window(0));
        }
    }

    #[test]
    fn test_focus_container_by_window() {
        let mut workspace = Workspace::default();

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(4));
            workspace.add_container_to_back(container);
        }

        // Focus container by window
        workspace.focus_container_by_window(1).unwrap();

        // Should be focused on workspace 0
        assert_eq!(workspace.focused_container_idx(), 0);

        // Should be focused on window 1 and hwnd should be 1
        let focused_container = workspace.focused_container_mut().unwrap();
        assert_eq!(
            focused_container.focused_window(),
            Some(&Window { hwnd: 1 })
        );
        assert_eq!(focused_container.focused_window_idx(), 1);
    }

    #[test]
    fn test_contains_managed_window() {
        let mut workspace = Workspace::default();

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        {
            // Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(4));
            workspace.add_container_to_back(container);
        }

        // Should return true, window is in container 1
        assert!(workspace.contains_managed_window(4));

        // Should return true, all the windows are in container 0
        for i in 0..3 {
            assert!(workspace.contains_managed_window(i));
        }

        // Should return false since window was never added
        assert!(!workspace.contains_managed_window(5));
    }

    #[test]
    fn test_new_floating_window() {
        let mut workspace = Workspace::default();

        {
            // Container with 3 windows
            let mut container = Container::default();
            for i in 0..3 {
                container.windows_mut().push_back(Window::from(i));
            }
            workspace.add_container_to_back(container);
        }

        // Float the focused window of the focused container
        workspace.new_floating_window().unwrap();

        // Should have 1 floating window
        assert_eq!(workspace.floating_windows().len(), 1);

        // The container still owns all three: floating changes placement, not ownership
        let container = workspace.focused_container().unwrap();
        assert_eq!(container.windows().len(), 3);
        assert!(container.is_active());

        // Should contain hwnd 0 since this is the first window in the container
        assert!(workspace.floating_windows().contains(&Window { hwnd: 0 }));
        assert!(workspace.is_floating_window(0));
    }

    #[test]
    fn test_visible_windows() {
        let mut workspace = Workspace::default();

        {
            // Create and add a default Container with 2 windows
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(100));
            container.windows_mut().push_back(Window::from(200));
            workspace.add_container_to_back(container);
        }

        {
            // There is no monocle container, so only the containers contribute a visible window
            let visible_windows = workspace.visible_windows();
            assert_eq!(visible_windows.len(), 1);
            assert_eq!(visible_windows[0].unwrap().hwnd, 100);
        }

        {
            // Create and add a default Container with 1 window
            let mut container = Container::default();
            container.windows_mut().push_back(Window::from(300));
            workspace.add_container_to_back(container);
        }

        {
            // visible_windows should return 100 and 300
            let visible_windows = workspace.visible_windows();
            assert_eq!(visible_windows.len(), 2);
            assert_eq!(visible_windows[0].unwrap().hwnd, 100);
            assert_eq!(visible_windows[1].unwrap().hwnd, 300);
        }

        // Maximizing window 200 makes it the window its own container shows; it does not add a
        // separate entry, because it never leaves that container.
        workspace.maximize_window(200).unwrap();

        {
            let visible_windows = workspace.visible_windows();
            assert_eq!(visible_windows.len(), 2);
            assert_eq!(visible_windows[0].unwrap().hwnd, 200);
            assert_eq!(visible_windows[1].unwrap().hwnd, 300);
        }
    }
    /// A workspace whose slots are already recorded, so placement can edit them locally.
    fn arranged_workspace(counts: &[usize], area: Rect) -> Workspace {
        let mut workspace = workspace_with_containers(counts);
        workspace.record_logical_slots(area);
        workspace
    }

    fn slot_of(workspace: &Workspace, id: &ContainerId) -> LogicalRect {
        workspace
            .logical_slots
            .get(id)
            .expect("an active container holds a slot")
    }

    #[test]
    fn the_first_window_of_an_empty_workspace_takes_the_whole_area() {
        let mut workspace = Workspace::default();
        let area = work_area(1920, 1080);

        let placement = workspace.place_new_window(Window::from(1));
        workspace.record_logical_slots(area);

        let NewWindowPlacement::NewContainer(id) = placement else {
            panic!("the first window gets a container of its own, got {placement:?}");
        };

        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(slot_of(&workspace, &id), LogicalRect::from(area));
        assert_eq!(workspace.focused_container_idx(), 0);
    }

    #[test]
    fn the_second_window_halves_the_focused_container() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1], area);
        let donor_id = workspace.containers()[0].id.clone();

        let placement = workspace.place_new_window(Window::from(99));

        let NewWindowPlacement::Split {
            created,
            donor,
            axis,
        } = placement
        else {
            panic!("the second window splits the focused container, got {placement:?}");
        };

        assert_eq!(donor, donor_id);
        assert_eq!(axis, SplitAxis::LeftRight);
        // A left/right split puts the new container on the left.
        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(0, 0, 960, 1080)
        );
        assert_eq!(
            slot_of(&workspace, &donor_id),
            LogicalRect::new(960, 0, 960, 1080)
        );
        assert_eq!(workspace.containers().len(), 2);
        assert_eq!(workspace.focused_container().unwrap().id, created);
        assert_eq!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area)),
            Ok(())
        );
    }

    #[test]
    fn a_tall_donor_is_divided_top_to_bottom_and_keeps_the_top() {
        let area = work_area(800, 1200);
        let mut workspace = arranged_workspace(&[1], area);
        let donor_id = workspace.containers()[0].id.clone();

        let placement = workspace.place_new_window(Window::from(99));

        let NewWindowPlacement::Split { created, axis, .. } = placement else {
            panic!("expected a split, got {placement:?}");
        };

        assert_eq!(axis, SplitAxis::TopBottom);
        assert_eq!(
            slot_of(&workspace, &donor_id),
            LogicalRect::new(0, 0, 800, 600)
        );
        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(0, 600, 800, 600)
        );
        // The new container is below the donor, so it comes after it in container order too.
        assert_eq!(workspace.containers()[1].id, created);
    }

    #[test]
    fn an_odd_donor_leaves_the_extra_pixel_with_the_donor_and_no_hole() {
        let area = work_area(1921, 1080);
        let mut workspace = arranged_workspace(&[1], area);
        let donor_id = workspace.containers()[0].id.clone();

        let NewWindowPlacement::Split { created, .. } =
            workspace.place_new_window(Window::from(99))
        else {
            panic!("expected a split");
        };

        assert_eq!(slot_of(&workspace, &created).width, 960);
        assert_eq!(slot_of(&workspace, &donor_id).width, 961);
        assert_eq!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area)),
            Ok(())
        );
    }

    #[test]
    fn a_third_window_still_splits_but_a_fourth_joins_a_neighbour() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1], area);

        assert!(matches!(
            workspace.place_new_window(Window::from(30)),
            NewWindowPlacement::Split { .. }
        ));
        assert_eq!(workspace.active_container_count(), 3);

        let placement = workspace.place_new_window(Window::from(40));

        let NewWindowPlacement::Joined(target) = placement else {
            panic!("a fourth window joins a neighbour, got {placement:?}");
        };

        // No container was created, and the window is the one its container now shows.
        assert_eq!(workspace.containers().len(), 3);
        let target_idx = workspace.container_idx_for_id(&target).unwrap();
        assert_eq!(
            workspace.containers()[target_idx]
                .focused_window()
                .unwrap()
                .hwnd,
            40
        );
        assert_eq!(workspace.focused_container().unwrap().id, target);
    }

    #[test]
    fn a_joined_window_goes_to_the_neighbour_chosen_left_before_up() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1, 1], area);

        // Lay the three containers out explicitly: a left column and two stacked on the right.
        let ids: Vec<ContainerId> = workspace
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect();
        workspace.logical_slots.replace_all(vec![
            (ids[0].clone(), LogicalRect::new(0, 0, 960, 1080)),
            (ids[1].clone(), LogicalRect::new(960, 0, 960, 540)),
            (ids[2].clone(), LogicalRect::new(960, 540, 960, 540)),
        ]);

        // The bottom right container has a left neighbour and an up neighbour; left wins.
        workspace.focus_container(2);
        assert_eq!(
            workspace.place_new_window(Window::from(50)),
            NewWindowPlacement::Joined(ids[0].clone())
        );

        // The left container borders both right slots; the upper one is taken first.
        workspace.focus_container(0);
        assert_eq!(
            workspace.place_new_window(Window::from(51)),
            NewWindowPlacement::Joined(ids[1].clone())
        );
    }

    #[test]
    fn a_hidden_focused_container_is_not_the_donor() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1], area);
        let active_id = workspace.containers()[0].id.clone();

        workspace.focus_container(0);
        workspace.focus_container(1);
        hide_container(&mut workspace, 1);
        workspace.record_logical_slots(area);

        // The focused container holds no slot, so the split comes off the most recent one which
        // does.
        let NewWindowPlacement::Split { donor, created, .. } =
            workspace.place_new_window(Window::from(60))
        else {
            panic!("expected a split off the geometry-focused container");
        };

        assert_eq!(donor, active_id);
        assert_eq!(slot_of(&workspace, &created).width, 960);
        assert_eq!(slot_of(&workspace, &active_id).width, 960);
        // Placing a window does not unhide anything: the hidden container still holds no slot.
        assert_eq!(workspace.logical_slots.len(), 2);
    }

    #[test]
    fn a_placed_window_does_not_discard_the_arrangement_it_was_placed_into() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1], area);

        let NewWindowPlacement::Split { created, donor, .. } =
            workspace.place_new_window(Window::from(70))
        else {
            panic!("expected a split");
        };

        let created_slot = slot_of(&workspace, &created);
        let donor_slot = slot_of(&workspace, &donor);

        // The next reconciliation has nothing to reconcile: the split is the arrangement now.
        workspace.record_logical_slots(area);

        assert_eq!(slot_of(&workspace, &created), created_slot);
        assert_eq!(slot_of(&workspace, &donor), donor_slot);
    }

    #[test]
    fn a_preselection_outranks_the_threshold_rule() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1, 1], area);

        workspace.preselect_container_idx(0);

        // Three active containers would otherwise mean joining a neighbour.
        let placement = workspace.place_new_window(Window::from(80));

        assert!(matches!(placement, NewWindowPlacement::NewContainer(_)));
        assert_eq!(workspace.containers().len(), 4);
        assert_eq!(workspace.containers()[0].focused_window().unwrap().hwnd, 80);
    }

    #[test]
    fn a_donor_too_small_to_halve_gets_a_plain_container_instead() {
        let area = work_area(3, 3);
        let mut workspace = arranged_workspace(&[1], area);

        let placement = workspace.place_new_window(Window::from(90));

        assert!(matches!(placement, NewWindowPlacement::NewContainer(_)));
        assert_eq!(workspace.containers().len(), 2);
    }
    #[test]
    fn a_manual_split_takes_the_second_most_recent_window_which_is_not_being_shown() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 2], area);
        let source_id = workspace.containers()[1].id.clone();

        // 2 is the window its container shows and 1 is the one underneath it, so the history reads
        // [2, 1] and the second most recent entry is the one which can be moved.
        workspace.record_focused_window(1);
        workspace.record_focused_window(2);

        let created = workspace.create_container_from_donor(None).unwrap();
        let created_idx = workspace.container_idx_for_id(&created).unwrap();

        assert_eq!(workspace.containers().len(), 3);
        assert_eq!(
            workspace.containers()[created_idx]
                .windows()
                .iter()
                .map(|window| window.hwnd)
                .collect::<Vec<_>>(),
            vec![1]
        );

        // The container it came from still shows what it was showing.
        let source_idx = workspace.container_idx_for_id(&source_id).unwrap();
        assert_eq!(
            workspace.containers()[source_idx]
                .focused_window()
                .unwrap()
                .hwnd,
            2
        );
        // And its history no longer names a window it does not own.
        assert!(
            !workspace.containers()[source_idx]
                .focus_history()
                .contains(&1)
        );
    }

    #[test]
    fn a_manual_split_divides_the_largest_slot_whoever_the_window_came_from() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2, 1], area);
        workspace.record_logical_slots(area);

        // The two-window container is the only one which can give a window away; the other one is
        // made the largest slot, so the two roles fall to different containers.
        let source_id = workspace.containers()[0].id.clone();
        let largest_id = workspace.containers()[1].id.clone();
        workspace
            .logical_slots
            .set(source_id.clone(), LogicalRect::new(0, 0, 640, 1080));
        workspace
            .logical_slots
            .set(largest_id.clone(), LogicalRect::new(640, 0, 1280, 1080));

        let created = workspace.create_container_from_donor(None).unwrap();

        // The half came off the largest slot, and the container which gave up the window kept its
        // own rectangle untouched.
        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(640, 0, 640, 1080)
        );
        assert_eq!(
            slot_of(&workspace, &largest_id),
            LogicalRect::new(1280, 0, 640, 1080)
        );
        assert_eq!(
            slot_of(&workspace, &source_id),
            LogicalRect::new(0, 0, 640, 1080)
        );
        assert_eq!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area)),
            Ok(())
        );
    }

    #[test]
    fn the_newest_container_wins_a_tie_between_equally_large_slots() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2, 1], area);

        // Two halves of exactly the same size; the container built second is the newer one.
        let older = workspace.containers()[0].id.clone();
        let newer = workspace.containers()[1].id.clone();
        assert!(
            workspace.containers()[1].sequence() > workspace.containers()[0].sequence(),
            "the helper builds containers in order"
        );

        let created = workspace.create_container_from_donor(None).unwrap();

        // Two 960x1080 halves: the newer one is the one which gets divided, along its longer edge,
        // and the older one is left exactly as it was.
        assert_eq!(
            slot_of(&workspace, &newer),
            LogicalRect::new(960, 0, 960, 540)
        );
        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(960, 540, 960, 540)
        );
        assert_eq!(
            slot_of(&workspace, &older),
            LogicalRect::new(0, 0, 960, 1080)
        );
    }

    #[test]
    fn a_manual_split_halves_the_donor_slot_without_moving_focus() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2], area);
        let donor_id = workspace.containers()[0].id.clone();

        let created = workspace.create_container_from_donor(None).unwrap();

        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(0, 0, 960, 1080)
        );
        assert_eq!(
            slot_of(&workspace, &donor_id),
            LogicalRect::new(960, 0, 960, 1080)
        );
        assert_eq!(
            workspace
                .logical_slots
                .validate_coverage(LogicalRect::from(area)),
            Ok(())
        );

        // Adding a container is not a focus change: the workspace is still on the container and
        // the window it was on, and the created container takes the oldest place in the history
        // rather than the newest.
        assert_eq!(workspace.focused_container().unwrap().id, donor_id);
        assert_eq!(
            workspace.container_focus_history.most_recent(),
            Some(&donor_id)
        );
        assert_eq!(workspace.container_focus_history.oldest(), Some(&created));
    }

    #[test]
    fn a_forced_axis_divides_the_donor_the_other_way() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2], area);
        let donor_id = workspace.containers()[0].id.clone();

        let created = workspace
            .create_container_from_donor(Some(SplitAxis::TopBottom))
            .unwrap();

        // A top/bottom split keeps the donor on top and puts the new container after it.
        assert_eq!(
            slot_of(&workspace, &donor_id),
            LogicalRect::new(0, 0, 1920, 540)
        );
        assert_eq!(
            slot_of(&workspace, &created),
            LogicalRect::new(0, 540, 1920, 540)
        );
        assert_eq!(workspace.containers()[1].id, created);
    }

    #[test]
    fn a_manual_split_is_refused_atomically_without_an_eligible_donor() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1], area);
        let before = workspace.clone();

        assert!(workspace.create_container_from_donor(None).is_err());

        assert_eq!(workspace.containers(), before.containers());
        assert_eq!(
            workspace.logical_slots.ordered(SlotOrder::TopToBottom),
            before.logical_slots.ordered(SlotOrder::TopToBottom)
        );
        assert_eq!(
            workspace.container_focus_history,
            before.container_focus_history
        );
    }

    #[test]
    fn a_hidden_container_is_never_the_donor() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2, 2], area);

        hide_container(&mut workspace, 0);
        workspace.record_logical_slots(area);
        let hidden_id = workspace.containers()[0].id.clone();
        let active_id = workspace.containers()[1].id.clone();

        // The hidden container is the most recent one, and it has two windows, but it holds no slot.
        workspace.focus_container(0);

        let created = workspace.create_container_from_donor(None).unwrap();
        let hidden_idx = workspace.container_idx_for_id(&hidden_id).unwrap();

        assert_eq!(workspace.containers()[hidden_idx].windows().len(), 2);
        assert_eq!(
            workspace
                .container_idx_for_id(&active_id)
                .map(|idx| workspace.containers()[idx].windows().len()),
            Some(1)
        );
        // The half came off the active container, and the hidden one still holds no slot.
        assert_eq!(slot_of(&workspace, &created).width, 960);
        assert!(!workspace.logical_slots.contains(&hidden_id));
    }

    #[test]
    fn a_split_off_floating_window_keeps_its_state_and_starts_the_container_hidden() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2], area);
        let donor_id = workspace.containers()[0].id.clone();
        let floating_rect = Rect {
            left: 10,
            top: 20,
            right: 300,
            bottom: 400,
        };

        workspace.float_window(1, floating_rect).unwrap();
        // 0 is the window the container shows, so 1 is the one a split may move.
        workspace.containers_mut()[0].focus_window_by_hwnd(0);

        let created = workspace.create_container_from_donor(None).unwrap();
        let created_idx = workspace.container_idx_for_id(&created).unwrap();
        let moved = workspace.containers()[created_idx].windows()[0].clone();

        // Only the window's container changed.
        assert_eq!(moved.hwnd, 1);
        assert_eq!(moved.placement, ManagedPlacement::Floating);
        assert_eq!(moved.floating_rect, Some(floating_rect));
        assert_eq!(moved.container_id, created);

        // The container it landed in has no visible stored window, so it is hidden, and the next
        // reconciliation hands its half back to the donor through the ordinary absorption.
        assert!(workspace.containers()[created_idx].is_hidden());
        workspace.record_logical_slots(area);

        assert!(!workspace.logical_slots.contains(&created));
        assert_eq!(slot_of(&workspace, &donor_id), LogicalRect::from(area));
        assert!(workspace.hidden_slot_restores.contains_key(&created));
    }

    #[test]
    fn a_manual_split_cannot_hide_the_container_it_took_a_window_from() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[2], area);
        let source_id = workspace.containers()[0].id.clone();

        // The window left behind is the one being shown, and a shown window is a visible stored
        // window, so the container it is in cannot lose its slot by giving another window away.
        // This is what the selection rule buys: the previous rule could take the shown window and
        // leave a minimized one in charge.
        workspace.containers_mut()[0]
            .windows_mut()
            .iter_mut()
            .next()
            .unwrap()
            .set_minimized();
        workspace.containers_mut()[0].focus_window_by_hwnd(1);

        let created = workspace.create_container_from_donor(None).unwrap();
        workspace.record_logical_slots(area);

        let source_idx = workspace.container_idx_for_id(&source_id).unwrap();
        assert!(workspace.containers()[source_idx].is_active());
        assert_eq!(
            workspace.containers()[source_idx]
                .focused_window()
                .unwrap()
                .hwnd,
            1
        );

        // The minimized window is the one which moved, so the created container is the hidden one
        // and hands its half straight back.
        assert!(
            workspace.containers()[workspace.container_idx_for_id(&created).unwrap()].is_hidden()
        );
        assert!(!workspace.logical_slots.contains(&created));
        assert_eq!(slot_of(&workspace, &source_id), LogicalRect::from(area));
    }

    /// Whether komorebi currently believes it has hidden `hwnd`.
    ///
    /// This is the set a stack hides its lower windows into, so it is what says whether a window
    /// which is supposed to be on screen is actually being drawn.
    fn is_programmatically_hidden(hwnd: isize) -> bool {
        crate::HIDDEN_HWNDS.lock().contains(&hwnd)
    }

    #[test]
    fn a_manual_split_shows_the_window_each_container_is_left_showing() {
        // Handles no other test uses: the hidden set is global.
        const TOP: isize = 9_102;
        const BELOW: isize = 9_101;

        let area = work_area(1920, 1080);
        let mut workspace = Workspace::default();
        let mut container = Container::default();
        container.add_window(Window::from(BELOW));
        container.add_window(Window::from(TOP));
        workspace.add_container_to_back(container);
        workspace.record_logical_slots(area);

        // Only the top of the stack is on screen before the split.
        assert!(is_programmatically_hidden(BELOW));
        assert!(!is_programmatically_hidden(TOP));

        let created = workspace.create_container_from_donor(None).unwrap();
        let created_idx = workspace.container_idx_for_id(&created).unwrap();

        // The window which was pulled out is the one which was hidden underneath the top, and it
        // is what its new container now shows.
        assert_eq!(
            workspace.containers()[created_idx]
                .focused_window()
                .unwrap()
                .hwnd,
            BELOW
        );
        assert!(!is_programmatically_hidden(BELOW));

        // And the container it came from goes on showing exactly what it was showing.
        assert!(!is_programmatically_hidden(TOP));
    }

    #[test]
    fn a_manual_split_falls_back_to_the_stack_when_the_history_says_nothing() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 3], area);

        // Nothing has been focused, which is the state adoption and a restart leave behind. The
        // biggest stack answers instead, with the window directly below the one it is showing.
        assert!(workspace.window_focus_history.is_empty());
        let shown = workspace.containers()[1].focused_window().unwrap().hwnd;
        let expected = workspace.containers()[1].windows()[1].hwnd;
        assert_ne!(expected, shown);

        let created = workspace.create_container_from_donor(None).unwrap();
        let created_idx = workspace.container_idx_for_id(&created).unwrap();

        assert_eq!(
            workspace.containers()[created_idx]
                .windows()
                .front()
                .unwrap()
                .hwnd,
            expected
        );
    }

    #[test]
    fn a_manual_split_is_refused_when_every_window_is_the_one_its_container_shows() {
        let area = work_area(1920, 1080);
        let mut workspace = arranged_workspace(&[1, 1, 1], area);
        let before = workspace.clone();

        // Three containers, three windows: there are already as many containers as there are
        // windows, so adding one could only be done by emptying another.
        assert!(workspace.create_container_from_donor(None).is_err());
        assert_eq!(workspace.containers(), before.containers());
        assert_eq!(
            workspace.logical_slots.ordered(SlotOrder::TopToBottom),
            before.logical_slots.ordered(SlotOrder::TopToBottom)
        );
    }

    /// The stack a fold produces, bottom to top.
    fn stack_hwnds(workspace: &Workspace, idx: usize) -> Vec<isize> {
        workspace.containers()[idx]
            .windows()
            .iter()
            .map(|window| window.hwnd)
            .collect()
    }

    #[test]
    fn consolidating_folds_every_container_into_the_first() {
        let mut workspace = workspace_with_hwnds(&[&[1, 2], &[3], &[4, 5]]);
        let expected = container_ids(&workspace)[0].clone();

        let target = workspace.consolidate_containers().unwrap();

        assert_eq!(target, expected);
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(container_ids(&workspace), vec![expected.clone()]);
        // Bottom to top: the last container is underneath, the first one on top, and the order
        // inside each of them is unchanged.
        assert_eq!(stack_hwnds(&workspace, 0), vec![4, 5, 3, 1, 2]);

        for window in workspace.containers()[0].windows() {
            assert_eq!(window.container_id, expected);
        }

        assert_eq!(
            crate::invariants::ValidateInvariants::validate_invariants(&workspace),
            vec![]
        );
    }

    #[test]
    fn consolidating_goes_on_showing_the_first_container_s_window() {
        let mut workspace = workspace_with_hwnds(&[&[1, 2], &[3], &[4, 5]]);

        workspace.consolidate_containers().unwrap();

        assert_eq!(
            workspace.containers()[0]
                .focused_managed_window()
                .unwrap()
                .hwnd,
            2
        );
        assert_eq!(workspace.focused_container_idx(), 0);
        assert_eq!(
            workspace.container_focus_history.iter().next(),
            Some(&workspace.containers()[0].id)
        );
    }

    #[test]
    fn consolidating_keeps_every_window_whole() {
        let mut workspace = workspace_with_hwnds(&[&[1], &[2], &[3]]);
        let rect = floating_rect(120);

        workspace.float_window(2, rect).unwrap();
        workspace.minimize_window(3).unwrap();

        workspace.consolidate_containers().unwrap();

        let floating = workspace.containers()[0]
            .windows()
            .iter()
            .find(|window| window.hwnd == 2)
            .unwrap();
        assert_eq!(floating.placement, ManagedPlacement::Floating);
        assert_eq!(floating.floating_rect, Some(rect));

        let minimized = workspace.containers()[0]
            .windows()
            .iter()
            .find(|window| window.hwnd == 3)
            .unwrap();
        assert_eq!(minimized.visibility, Visibility::Minimized);

        // The windows never left the workspace, so its minimize history did not lose them either.
        assert_eq!(
            workspace
                .minimize_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn consolidating_leaves_a_locked_container_alone() {
        let mut workspace = workspace_with_hwnds(&[&[1], &[2], &[3]]);
        let locked = container_ids(&workspace)[1].clone();
        workspace.containers_mut()[1].locked = true;

        let target = workspace.consolidate_containers().unwrap();

        assert_eq!(workspace.containers().len(), 2);
        assert_eq!(workspace.container_idx_for_id(&locked), Some(1));
        assert_eq!(
            stack_hwnds(&workspace, workspace.container_idx_for_id(&target).unwrap()),
            vec![3, 1]
        );
        assert_eq!(stack_hwnds(&workspace, 1), vec![2]);
    }

    #[test]
    fn consolidating_one_container_changes_nothing() {
        let mut workspace = workspace_with_hwnds(&[&[1, 2]]);
        let before = workspace.clone();

        let target = workspace.consolidate_containers().unwrap();

        assert_eq!(target, before.containers()[0].id);
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(stack_hwnds(&workspace, 0), vec![1, 2]);
    }

    #[test]
    fn consolidating_an_empty_workspace_answers_nothing() {
        let mut workspace = Workspace::default();

        assert_eq!(workspace.consolidate_containers(), None);
        assert!(workspace.containers().is_empty());
    }

    #[test]
    fn consolidating_invalidates_the_arrangement_and_forgets_the_folded_containers() {
        let area = work_area(1920, 1080);
        let mut workspace = workspace_with_hwnds(&[&[1], &[2], &[3]]);
        workspace.record_logical_slots(area);
        let folded = container_ids(&workspace)[1..].to_vec();

        let target = workspace.consolidate_containers().unwrap();

        assert!(workspace.relayout_pending);
        assert!(workspace.hidden_slot_restores.is_empty());
        assert_eq!(workspace.resize_dimensions, vec![None]);

        for id in folded {
            assert!(workspace.logical_slots.get(&id).is_none());
            assert!(
                !workspace
                    .container_focus_history
                    .iter()
                    .any(|entry| *entry == id)
            );
        }

        // One container, one slot, the whole work area.
        workspace.record_logical_slots(area);
        assert_eq!(
            workspace.logical_slots.get(&target),
            Some(LogicalRect::from(area))
        );
    }

    #[test]
    fn recording_a_window_focus_records_it_in_the_workspace_history_too() {
        let mut workspace = workspace_with_hwnds(&[&[1, 2], &[3]]);

        workspace.record_focused_window(2);
        workspace.record_focused_window(3);
        workspace.record_focused_window(1);

        // Most recent first, one entry per window, across containers.
        assert_eq!(
            workspace
                .window_focus_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 3, 2]
        );
    }

    #[test]
    fn a_window_leaving_the_workspace_leaves_its_focus_history_entry_behind() {
        let mut workspace = workspace_with_hwnds(&[&[1, 2], &[3]]);
        workspace.record_focused_window(2);
        workspace.record_focused_window(3);

        workspace.take_window(3).unwrap();

        assert!(!workspace.window_focus_history.contains(&3));
        assert!(workspace.window_focus_history.contains(&2));
    }

    #[test]
    fn destroying_a_container_keeps_the_window_focus_history_of_the_windows_it_gives_away() {
        let mut workspace = workspace_with_hwnds(&[&[1], &[2, 3]]);
        workspace.record_focused_window(3);
        workspace.record_focused_window(2);
        workspace.record_focused_window(1);

        workspace.destroy_container(1).unwrap();

        // The windows stayed in the workspace, so their history did too, in the same order.
        assert_eq!(
            workspace
                .window_focus_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn consolidating_keeps_the_window_focus_history() {
        let mut workspace = workspace_with_hwnds(&[&[1], &[2], &[3]]);
        workspace.record_focused_window(3);
        workspace.record_focused_window(2);

        workspace.consolidate_containers().unwrap();

        assert!(workspace.window_focus_history.contains(&2));
        assert!(workspace.window_focus_history.contains(&3));
    }

    #[test]
    fn merging_a_workspace_merges_the_window_focus_histories_most_recent_first() {
        let mut target = workspace_with_hwnds(&[&[1]]);
        target.record_focused_window(1);

        let mut source = workspace_with_hwnds(&[&[2, 3]]);
        source.record_focused_window(3);
        source.record_focused_window(2);

        target.merge_from(source);

        // The source workspace is the one being closed, so its order leads.
        assert_eq!(
            target
                .window_focus_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn pruning_drops_window_focus_history_entries_for_windows_which_are_gone() {
        let mut workspace = workspace_with_hwnds(&[&[1]]);
        workspace.record_focused_window(1);
        workspace.window_focus_history.record(99);

        workspace.prune_histories();

        assert_eq!(
            workspace
                .window_focus_history
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn containers_are_stamped_in_the_order_they_are_created() {
        let first = Container::default();
        let second = Container::default();
        let third = Container::default();

        assert!(first.sequence() < second.sequence());
        assert!(second.sequence() < third.sequence());
    }

    #[test]
    fn a_container_keeps_its_creation_stamp_across_a_state_document() {
        let container = container_with_hwnds(&[1, 2]);
        let json = serde_json::to_string(&container).unwrap();
        let restored: Container = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.sequence(), container.sequence());

        // A container created after the restore is younger than the one which was restored, which
        // is what stops a restart reordering a workspace.
        assert!(Container::default().sequence() > restored.sequence());
    }

    #[test]
    fn a_container_read_from_a_document_written_before_stamps_existed_is_stamped_on_arrival() {
        let legacy = r#"{"id":"legacy","windows":{"elements":[],"focused":0}}"#;
        let restored: Container = serde_json::from_str(legacy).unwrap();

        assert!(restored.sequence() > 0);
    }
}
