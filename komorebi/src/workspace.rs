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
use crate::core::WindowPlacement;
use crate::focus_history::Mru;
use crate::geometry::LogicalRect;
use crate::geometry::LogicalSlots;
use crate::geometry::RenderInsets;
use crate::lockable_sequence::LockableSequence;
use crate::managed_window::ManagedPlacement;
use crate::managed_window::ManagedWindow;
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
    /// Most-recently-minimized window handles owned by this workspace.
    ///
    /// Minimizing keeps the window in its container, so this history is the only record of the
    /// order in which windows were minimized.
    #[serde(default)]
    pub minimize_history: Mru<isize>,
    pub monocle_container: Option<Container>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monocle_container_restore_idx: Option<usize>,
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

impl Default for Workspace {
    fn default() -> Self {
        Self {
            id: WorkspaceId::new(),
            name: None,
            containers: Ring::default(),
            container_focus_history: Mru::default(),
            minimize_history: Mru::default(),
            monocle_container: None,
            monocle_container_restore_idx: None,
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

        if let Some(container) = &self.monocle_container {
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
        if let Some(container) = &self.monocle_container
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
        // Maximised windows and floating windows should always be drawn at the top of the Z order
        // when switching to a workspace. A maximized window has already been shown maximized by
        // the container which owns it, so only the Z order is still left to settle here.
        let maximized_window = self.maximized_window();

        if let Some(window) = to_focus {
            if maximized_window.is_none() && matches!(self.layer, WorkspaceLayer::Tiling) {
                window.focus(mouse_follows_focus)?;
            } else if let Some(maximized_window) = maximized_window {
                maximized_window.focus(mouse_follows_focus)?;
            } else if let Some(floating_window) = self.focused_floating_window() {
                floating_window.focus(mouse_follows_focus)?;
            }
        } else if let Some(maximized_window) = maximized_window {
            maximized_window.focus(mouse_follows_focus)?;
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
        let window_based_work_area_offset = self.globals.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.globals.window_based_work_area_offset_limit;
        let mut rules_work_area_offset = None;

        if !self.work_area_offset_rules.is_empty() {
            let count = if self.monocle_container.is_some() {
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
            || self.monocle_container.is_some() && window_based_work_area_offset_limit > 0)
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
            if let Some(container) = &mut self.monocle_container {
                if let Some(window) = container.focused_window_mut() {
                    adjusted_work_area.add_padding(container_padding);
                    adjusted_work_area.add_padding(border_offset);
                    adjusted_work_area.add_padding(border_width);
                    window.set_position(&adjusted_work_area, true)?;
                };
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

                            // A maximized window keeps its container's slot but is not drawn in
                            // it; drawing it into the slot would silently unmaximize it.
                            if window.is_maximized() {
                                window.window.maximize();
                            } else {
                                window.set_position(layout, false)?;
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

        // since monocle is a toggle, we never want to truncate the resize dimensions since it will
        // almost always be toggled off and the container will be reintegrated into layout
        //
        // without this check, if there are exactly two containers, when one is toggled to monocle
        // the resize dimensions will be truncated to len == 1, and when it is reintegrated, if it
        // had a resize adjustment before, that will have been lost
        if self.monocle_container.is_none() {
            self.resize_dimensions.resize(container_count, None);
        }

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

    /// Whether the container a move would take away currently presents a maximized window.
    ///
    /// A maximized window used to be held outside every container, so any maximized window in the
    /// workspace blocked a container move. Now that it stays where it belongs, only the container
    /// actually being moved can block one.
    #[must_use]
    pub fn focused_container_has_maximized_window(&self) -> bool {
        self.focused_container()
            .and_then(Container::maximized_managed_window)
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

    /// The window a maximize request acts on.
    ///
    /// The floating layer acts on the floating window it is cycling; otherwise the focused
    /// container's focused window is the subject. A minimized window is never a subject, because
    /// maximizing it would make the model claim a presentation nothing is drawing.
    fn maximize_subject(&self) -> Option<isize> {
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
    ///
    /// The window keeps its container, its position in that container's stack and both of its
    /// history entries. Only its presentation changes, which is why a maximize toggle no longer
    /// destroys a container and rebuilds a different one in its place.
    pub fn maximize_focused_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .maximize_subject()
            .ok_or_eyre("there is no window to maximize")?;

        self.maximize_window(hwnd)
    }

    pub fn maximize_window(&mut self, hwnd: isize) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("this workspace does not own that window")?;

        // Read before the mutable borrow: the rectangle the window has now is the one it should
        // come back to when it stops being maximized.
        let current_rect = WindowsApi::window_rect(hwnd).unwrap_or_default();

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_eyre("there is no container")?;

        let window_idx = container
            .idx_for_window(hwnd)
            .ok_or_eyre("that container does not own that window")?;

        let changed = container.windows_mut()[window_idx].set_maximized(current_rect);
        let window = container.windows()[window_idx].window;
        container.focus_window(window_idx);
        self.focus_container(container_idx);

        // Reapplying the Win32 state of a window which is already maximized is what makes a
        // duplicated command or event converge instead of toggling.
        window.maximize();

        if !changed {
            tracing::debug!("window {hwnd} was already maximized");
        }

        Ok(())
    }

    /// Return the maximized window to the presentation its placement implies.
    pub fn unmaximize_window(&mut self) -> eyre::Result<()> {
        let hwnd = self
            .maximized_window()
            .ok_or_eyre("there is no maximized window")?
            .hwnd;

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
        let target = managed.set_normal(fallback);
        let window = container.windows()[window_idx].window;

        window.unmaximize();

        // A stored window is put back in its slot by the retile which follows; a floating window
        // has no slot, so the rectangle it kept is applied directly. The model transition has
        // already committed, so a failed Win32 call is reported rather than rolled back: the
        // next retile reapplies the same rectangle from the same state.
        if placement == ManagedPlacement::Floating
            && let Some(target) = target
            && let Err(error) = window.set_position(&target, false)
        {
            tracing::warn!("could not restore the floating rectangle of window {hwnd}: {error}");
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

    /// Recalculate the logical slots, store them by container ID, and report any tiling violation.
    ///
    /// Returns the active slots in container order so the caller can render them.
    pub fn record_logical_slots(
        &mut self,
        available_area: Rect,
    ) -> Vec<(ContainerId, LogicalRect)> {
        let slots = self.calculate_logical_slots(available_area);
        let area = LogicalRect::from(available_area);

        self.logical_work_area = Some(area);
        self.logical_slots.replace_all(slots.clone());

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

        if let Some(container) = &self.monocle_container
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

        if let Some(container) = &self.monocle_container
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

        if let Some(container) = &self.monocle_container
            && container.contains_window(hwnd)
        {
            return true;
        }

        false
    }

    pub fn is_focused_window_monocle_or_maximized(&self) -> eyre::Result<bool> {
        let hwnd = WindowsApi::foreground_window()?;
        if self.is_maximized_window(hwnd) {
            return Ok(true);
        }

        if let Some(container) = &self.monocle_container
            && container.contains_window(hwnd)
        {
            return Ok(true);
        }

        Ok(false)
    }

    pub fn is_empty(&self) -> bool {
        self.containers().is_empty() && self.monocle_container.is_none()
    }

    pub fn contains_window(&self, hwnd: isize) -> bool {
        for container in self.containers() {
            if container.contains_window(hwnd) {
                return true;
            }
        }

        if let Some(container) = &self.monocle_container
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
        let container = self.containers_mut().remove_respecting_locks(idx);

        if idx < self.resize_dimensions.len() {
            self.resize_dimensions.remove(idx);
        }

        if let Some(container) = &container {
            self.forget_container(container);
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

        for window in container.windows() {
            self.minimize_history.remove(&window.hwnd);
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

        if let Some(container) = &mut self.monocle_container
            && let Some(window_idx) = container
                .windows()
                .iter()
                .position(|window| window.hwnd == hwnd)
        {
            container
                .remove_window_by_idx(window_idx)
                .ok_or_eyre("there is no window")?;

            if container.windows().is_empty() {
                self.monocle_container = None;
                self.monocle_container_restore_idx = None;
            }

            for c in self.containers() {
                c.restore();
            }

            return Ok(());
        }

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;

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
            self.focus_previous_container();
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

        if let Some(container) = &mut self.monocle_container
            && let Some(window_idx) = container.idx_for_window(hwnd)
        {
            let window = container
                .remove_window_by_idx(window_idx)
                .ok_or_eyre("there is no window")?;

            if container.windows().is_empty() {
                self.monocle_container = None;
                self.monocle_container_restore_idx = None;
            } else {
                container.load_focused_window();
            }

            return Ok(window);
        }

        let container_idx = self
            .container_idx_for_window(hwnd)
            .ok_or_eyre("there is no window")?;
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
            self.focus_previous_container();
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

    pub fn remove_focused_container(&mut self) -> Option<Container> {
        let focused_idx = self.focused_container_idx();
        let container = self.remove_container_by_idx(focused_idx);
        self.focus_previous_container();

        container
    }

    pub fn remove_container(&mut self, idx: usize) -> Option<Container> {
        let container = self.remove_container_by_idx(idx);
        self.focus_previous_container();

        container
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

    /// Restore the most recently minimized window this workspace still owns, and focus it.
    ///
    /// The window returns with the placement and presentation it had, so a floating window comes
    /// back floating and a maximized window comes back maximized. Both histories are updated
    /// through the ordinary focus path.
    pub fn restore_last_minimized_window(&mut self) -> Option<isize> {
        let hwnd = self.take_last_minimized_window()?;

        self.unminimize_window(hwnd).ok()?;
        self.focus_container_by_window(hwnd).ok()?;

        Some(hwnd)
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
    /// A maximized window is returned to Normal first, and a window held by the transitional
    /// monocle path is reintegrated first, so floating leaves ownership where the model requires
    /// it. Nothing is removed from a container here, which is why an emptied container can no
    /// longer appear as a side effect of floating a window.
    pub fn new_floating_window(&mut self) -> eyre::Result<()> {
        if self.maximized_window().is_some() {
            self.unmaximize_window()?;
        } else if self.monocle_container.is_some() {
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

    pub fn new_monocle_container(&mut self) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();

        // we shouldn't use remove_container_by_idx here because it doesn't make sense for
        // monocle and maximized toggles which take over the whole screen before being reinserted
        // at the same index to respect locked container indexes
        let container = self
            .containers_mut()
            .remove(focused_idx)
            .ok_or_eyre("there is no container")?;

        // The container is no longer in the ring, so it must not keep an active slot; the next
        // update recalculates the remaining slots and reintegration recalculates them again.
        self.logical_slots.remove(&container.id);

        // We don't remove any resize adjustments for a monocle, because when this container is
        // inevitably reintegrated, it would be weird if it doesn't go back to the dimensions
        // it had before

        self.monocle_container = Option::from(container);
        self.monocle_container_restore_idx = Option::from(focused_idx);
        self.focus_previous_container();

        self.monocle_container
            .as_mut()
            .ok_or_eyre("there is no monocle container")?
            .load_focused_window();

        Ok(())
    }

    pub fn reintegrate_monocle_container(&mut self) -> eyre::Result<()> {
        let restore_idx = self
            .monocle_container_restore_idx
            .ok_or_eyre("there is no monocle restore index")?;

        let container = self
            .monocle_container
            .as_ref()
            .ok_or_eyre("there is no monocle container")?;

        let container = container.clone();
        if restore_idx >= self.containers().len() {
            self.containers_mut()
                .resize(restore_idx, Container::default());
        }

        // we shouldn't use insert_container_at_index here because it doesn't make sense for
        // monocle and maximized toggles which take over the whole screen before being reinserted
        // at the same index to respect locked container indexes
        self.containers_mut().insert(restore_idx, container);
        self.focus_container(restore_idx);
        self.focused_container_mut()
            .ok_or_eyre("there is no container")?
            .load_focused_window();

        self.monocle_container = None;
        self.monocle_container_restore_idx = None;

        Ok(())
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

    pub fn visible_windows(&self) -> Vec<Option<&Window>> {
        let mut vec = vec![];

        if let Some(monocle) = &self.monocle_container {
            vec.push(monocle.focused_window());
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

        if let Some(monocle) = &self.monocle_container
            && let Some(focused) = monocle.focused_window()
            && let Ok(details) = (*focused).try_into()
        {
            vec.push(details);
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
    fn a_restored_window_keeps_the_placement_it_was_minimized_with() {
        let mut workspace = workspace_with_containers(&[2]);
        workspace.float_window(0, floating_rect(3)).unwrap();
        workspace.containers_mut()[0].windows_mut()[0].presentation = Presentation::Maximized;
        workspace.minimize_window(0).unwrap();

        assert_eq!(workspace.restore_last_minimized_window(), Some(0));

        let window = &workspace.containers()[0].windows()[0];
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
        assert!(!workspace.focused_container_has_maximized_window());

        workspace.focus_container(0);
        assert!(workspace.focused_container_has_maximized_window());
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
    fn detach_removes_alternate_legacy_ownership_paths() {
        let mut monocle_container = Container::default();
        monocle_container.windows_mut().push_back(Window::from(44));
        let mut monocle_workspace = Workspace {
            monocle_container: Some(monocle_container),
            monocle_container_restore_idx: Some(0),
            ..Workspace::default()
        };
        monocle_workspace.detach_window(44).unwrap();
        assert!(monocle_workspace.monocle_container.is_none());
        assert!(monocle_workspace.monocle_container_restore_idx.is_none());
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
}
