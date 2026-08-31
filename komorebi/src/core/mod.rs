#![warn(clippy::all)]
#![allow(clippy::missing_errors_doc, clippy::use_self, clippy::doc_markdown)]
#![allow(deprecated)] // allow deprecated variants like HidingBehaviour::Hide to be used in derive macros

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;

use clap::ValueEnum;
use color_eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;
use strum::EnumString;

use crate::KomorebiTheme;
use crate::animation::prefix::AnimationPrefix;
use crate::geometry::SplitAxis;
use crate::state::State;

// Re-export everything from komorebi-layouts
pub use komorebi_layouts::Arrangement;
pub use komorebi_layouts::Axis;
pub use komorebi_layouts::Column;
pub use komorebi_layouts::ColumnSplit;
pub use komorebi_layouts::ColumnSplitWithCapacity;
pub use komorebi_layouts::ColumnWidth;
pub use komorebi_layouts::CustomLayout;
pub use komorebi_layouts::CycleDirection;
pub use komorebi_layouts::DEFAULT_RATIO;
pub use komorebi_layouts::DEFAULT_SECONDARY_RATIO;
pub use komorebi_layouts::DefaultLayout;
pub use komorebi_layouts::Direction;
pub use komorebi_layouts::GridLayoutOptions;
pub use komorebi_layouts::Layout;
pub use komorebi_layouts::LayoutDefaultEntry;
pub use komorebi_layouts::LayoutOptions;
pub use komorebi_layouts::MAX_RATIO;
pub use komorebi_layouts::MAX_RATIOS;
pub use komorebi_layouts::MIN_RATIO;
pub use komorebi_layouts::OperationDirection;
pub use komorebi_layouts::Rect;
pub use komorebi_layouts::ScrollingLayoutOptions;
pub use komorebi_layouts::Sizing;
pub use komorebi_layouts::validate_ratios;

// Local modules and exports
pub use animation::AnimationStyle;
pub use pathext::PathExt;
pub use pathext::ResolvedPathBuf;
pub use pathext::replace_env_in_path;
pub use pathext::resolve_option_hashmap_usize_path;

pub mod animation;
pub mod asc;
pub mod config_generation;
pub mod pathext;

// serde_as must be before derive
#[serde_with::serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Display)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "content")]
pub enum SocketMessage {
    // Window / Container Commands
    FocusWindow(OperationDirection),
    MoveWindow(OperationDirection),
    PreselectDirection(OperationDirection),
    CancelPreselect,
    CycleFocusWindow(CycleDirection),
    CycleMoveWindow(CycleDirection),
    StackWindow(OperationDirection),
    /// Merge the container in this direction into the focused container.
    ///
    /// Deliberately distinct from `StackWindow`, which moves one window into a neighbouring
    /// container. Every window of the neighbour moves and the neighbour is destroyed.
    MergeContainer(OperationDirection),
    UnstackWindow,
    CycleStack(CycleDirection),
    CycleStackIndex(CycleDirection),
    /// Walk the focused container's window focus history, raising the window walked to.
    ///
    /// Deliberately distinct from `CycleStack`, which walks the stack order. Walking a history
    /// does not rewrite it, so successive steps keep going back rather than returning to where
    /// the first step started.
    CycleWindowHistory(CycleDirection),
    /// Walk the focused workspace's container focus history, focusing the container walked to.
    CycleContainerHistory(CycleDirection),
    FocusStackWindow(usize),
    /// Raise the window under the top of the focused container's stack and focus it.
    ///
    /// Deliberately distinct from `CycleStack`, which moves focus around a stack without changing
    /// the order of the windows in it.
    RaiseNextStackWindow,
    StackAll,
    UnstackAll,
    ResizeWindowEdge(OperationDirection, Sizing),
    ResizeWindowAxis(Axis, Sizing),
    /// Move the focused floating window, leaving every container and slot untouched.
    ///
    /// Deliberately distinct from `MoveWindow`, which moves a container within the arrangement.
    /// An omitted delta uses the configured `floating_move_delta`.
    MoveFloatingWindow(OperationDirection, Option<i32>),
    /// Move one edge of the focused floating window, leaving the opposite edge where it is.
    ///
    /// Deliberately distinct from `ResizeWindowEdge`, which moves a boundary shared by tiled
    /// containers. An omitted delta uses the configured `floating_resize_delta`.
    ResizeFloatingWindow(OperationDirection, Sizing, Option<i32>),
    MoveContainerToLastWorkspace,
    SendContainerToLastWorkspace,
    MoveContainerToMonitorNumber(usize),
    CycleMoveContainerToMonitor(CycleDirection),
    MoveContainerToWorkspaceNumber(usize),
    MoveContainerToNamedWorkspace(String),
    CycleMoveContainerToWorkspace(CycleDirection),
    SendContainerToMonitorNumber(usize),
    CycleSendContainerToMonitor(CycleDirection),
    SendContainerToWorkspaceNumber(usize),
    CycleSendContainerToWorkspace(CycleDirection),
    SendContainerToMonitorWorkspaceNumber(usize, usize),
    MoveContainerToMonitorWorkspaceNumber(usize, usize),
    SendContainerToNamedWorkspace(String),
    /// Send the focused window to the workspace with this stable ID, and follow it if asked.
    ///
    /// The window goes to the top of the stack of the target's most recently focused active
    /// container, or into a container of its own when the target has no active container.
    MoveWindowToWorkspaceId(String, bool),
    /// Send the focused window to the top of the stack of the container with this stable ID, and
    /// follow it if asked.
    MoveWindowToContainerId(String, bool),
    /// Send the focused window to the focused workspace of the monitor at this index.
    ///
    /// Deliberately distinct from `MoveContainerToMonitorNumber`, which takes a whole container
    /// across. The boolean is whether focus follows the window.
    MoveWindowToMonitorNumber(usize, bool),
    CycleMoveWorkspaceToMonitor(CycleDirection),
    MoveWorkspaceToMonitorNumber(usize),
    SwapWorkspacesToMonitorNumber(usize),
    ForceFocus,
    Close,
    Minimize,
    /// Restore the window most recently minimized on the focused workspace.
    ///
    /// The window returns to the container it was minimized in, with the placement and
    /// presentation it was minimized with.
    RestoreLastMinimizedWindow,
    Promote,
    PromoteSwap,
    PromoteFocus,
    PromoteWindow(OperationDirection),
    EagerFocus(String),
    LockMonitorWorkspaceContainer(usize, usize, usize),
    UnlockMonitorWorkspaceContainer(usize, usize, usize),
    ToggleLock,
    ToggleFloat,
    ToggleMonocle,
    ToggleMaximize,
    ToggleFullscreen,
    ToggleWindowContainerBehaviour,
    ToggleFloatOverride,
    WindowHidingBehaviour(HidingBehaviour),
    ToggleCrossMonitorMoveBehaviour,
    CrossMonitorMoveBehaviour(MoveBehaviour),
    ToggleMonocleFocusBehaviour,
    MonocleFocusBehaviour(MonocleFocusBehaviour),
    UnmanagedWindowOperationBehaviour(OperationBehaviour),
    /// Add one container to the focused workspace.
    ///
    /// The largest active slot is divided, the most recently created container winning a tie, and
    /// an omitted axis divides that slot's longer edge. The window put into the created container
    /// is the second most recent window in the workspace's focus history which is not the window
    /// its own container is showing. Focus does not move.
    CreateContainer(Option<SplitAxis>),
    /// Remove one container from the focused workspace: the one created most recently, whichever
    /// container holds the focus.
    ///
    /// Its windows are dealt out to the containers which remain. Focus does not move, except that
    /// a focused window which was in the destroyed container is raised and focused wherever it
    /// lands.
    DestroyContainer,
    /// Destroy the focused container, dealing every window it holds out to the containers which
    /// remain.
    ///
    /// The focused window travels with the focus into whichever container receives it.
    DestroyFocusedContainer,
    // Current Workspace Commands
    ManageFocusedWindow,
    UnmanageFocusedWindow,
    /// Temporarily remove a window from management, or the foreground window when no handle is
    /// given.
    ///
    /// The window keeps no container, workspace, stack position or history: komorebi stops
    /// positioning it and ordinary Win32 events will not take it back.
    SuspendWindow(Option<isize>),
    /// Hand a temporarily unmanaged window back to management, or the foreground window when no
    /// handle is given.
    ///
    /// The window is processed as a newly opened one rather than returned to where it used to be.
    ResumeWindow(Option<isize>),
    AdjustContainerPadding(Sizing, i32),
    AdjustWorkspacePadding(Sizing, i32),
    ChangeLayout(DefaultLayout),
    CycleLayout(CycleDirection),
    LayoutRatios(Option<Vec<f32>>, Option<Vec<f32>>),
    ScrollingLayoutColumns(NonZeroUsize),
    ChangeLayoutCustom(#[serde_as(as = "ResolvedPathBuf")] PathBuf),
    FlipLayout(Axis),
    ToggleWorkspaceWindowContainerBehaviour,
    ToggleWorkspaceFloatOverride,
    // Monitor and Workspace Commands
    MonitorIndexPreference(usize, i32, i32, i32, i32),
    DisplayIndexPreference(usize, String),
    EnsureWorkspaces(usize, usize),
    EnsureNamedWorkspaces(usize, Vec<String>),
    NewWorkspace,
    /// Move the focused workspace to a position on its monitor's list.
    ///
    /// Only the order changes: every workspace keeps its ID, its name, its windows and its layout.
    MoveWorkspaceToIndex(usize),
    /// Move the focused workspace one position along its monitor's list, wrapping at the ends.
    CycleMoveWorkspace(CycleDirection),
    /// Exchange the focused workspace's position with the workspace at an index.
    SwapWorkspaceWithIndex(usize),
    /// Delete the focused workspace, merging its containers, windows and histories into a
    /// neighbour. A monitor's last workspace is refused.
    MergeFocusedWorkspace,
    ToggleTiling,
    Stop,
    StopIgnoreRestore,
    TogglePause,
    /// Pause all tiling without stopping komorebi. Pausing an already paused komorebi is a no-op.
    Pause,
    /// Resume tiling after a pause. Resuming a komorebi which is not paused is a no-op.
    Unpause,
    Retile,
    RetileWithResizeDimensions,
    QuickSave,
    QuickLoad,
    Save(#[serde_as(as = "ResolvedPathBuf")] PathBuf),
    Load(#[serde_as(as = "ResolvedPathBuf")] PathBuf),
    CycleFocusMonitor(CycleDirection),
    CycleFocusWorkspace(CycleDirection),
    CycleFocusEmptyWorkspace(CycleDirection),
    FocusMonitorNumber(usize),
    FocusMonitorAtCursor,
    FocusLastWorkspace,
    CloseWorkspace,
    FocusWorkspaceNumber(usize),
    FocusWorkspaceNumbers(usize),
    FocusMonitorWorkspaceNumber(usize, usize),
    FocusNamedWorkspace(String),
    ContainerPadding(usize, usize, i32),
    NamedWorkspaceContainerPadding(String, i32),
    FocusedWorkspaceContainerPadding(i32),
    WorkspacePadding(usize, usize, i32),
    NamedWorkspacePadding(String, i32),
    FocusedWorkspacePadding(i32),
    WorkspaceTiling(usize, usize, bool),
    NamedWorkspaceTiling(String, bool),
    WorkspaceName(usize, usize, String),
    WorkspaceLayout(usize, usize, DefaultLayout),
    NamedWorkspaceLayout(String, DefaultLayout),
    WorkspaceLayoutCustom(usize, usize, #[serde_as(as = "ResolvedPathBuf")] PathBuf),
    NamedWorkspaceLayoutCustom(String, #[serde_as(as = "ResolvedPathBuf")] PathBuf),
    WorkspaceLayoutRule(usize, usize, usize, DefaultLayout),
    NamedWorkspaceLayoutRule(String, usize, DefaultLayout),
    WorkspaceLayoutCustomRule(
        usize,
        usize,
        usize,
        #[serde_as(as = "ResolvedPathBuf")] PathBuf,
    ),
    NamedWorkspaceLayoutCustomRule(String, usize, #[serde_as(as = "ResolvedPathBuf")] PathBuf),
    ClearWorkspaceLayoutRules(usize, usize),
    ClearNamedWorkspaceLayoutRules(String),
    ToggleWorkspaceLayer,
    // Configuration
    ReloadConfiguration,
    ReplaceConfiguration(#[serde_as(as = "ResolvedPathBuf")] PathBuf),
    ReloadStaticConfiguration(#[serde_as(as = "ResolvedPathBuf")] PathBuf),
    WatchConfiguration(bool),
    CompleteConfiguration,
    AltFocusHack(bool),
    Theme(Box<KomorebiTheme>),
    Animation(bool, Option<AnimationPrefix>),
    AnimationDuration(u64, Option<AnimationPrefix>),
    AnimationFps(u64),
    AnimationStyle(AnimationStyle, Option<AnimationPrefix>),
    #[serde(alias = "ActiveWindowBorder")]
    Border(bool),
    #[serde(alias = "ActiveWindowBorderColour")]
    BorderColour(WindowKind, u32, u32, u32),
    #[serde(alias = "ActiveWindowBorderStyle")]
    BorderStyle(BorderStyle),
    BorderWidth(i32),
    BorderOffset(i32),
    BorderImplementation(BorderImplementation),
    Transparency(bool),
    ToggleTransparency,
    TransparencyAlpha(u8),
    InvisibleBorders(Rect),
    StackbarMode(StackbarMode),
    StackbarLabel(StackbarLabel),
    StackbarFocusedTextColour(u32, u32, u32),
    StackbarUnfocusedTextColour(u32, u32, u32),
    StackbarBackgroundColour(u32, u32, u32),
    StackbarHeight(i32),
    StackbarTabWidth(i32),
    StackbarFontSize(i32),
    StackbarFontFamily(Option<String>),
    WorkAreaOffset(Rect),
    MonitorWorkAreaOffset(usize, Rect),
    WorkspaceWorkAreaOffset(usize, usize, Rect),
    ToggleWindowBasedWorkAreaOffset,
    ResizeDelta(i32),
    InitialWorkspaceRule(ApplicationIdentifier, String, usize, usize),
    InitialNamedWorkspaceRule(ApplicationIdentifier, String, String),
    WorkspaceRule(ApplicationIdentifier, String, usize, usize),
    NamedWorkspaceRule(ApplicationIdentifier, String, String),
    ClearWorkspaceRules(usize, usize),
    ClearNamedWorkspaceRules(String),
    ClearAllWorkspaceRules,
    EnforceWorkspaceRules,
    SessionFloatRule,
    SessionFloatRules,
    ClearSessionFloatRules,
    #[serde(alias = "FloatRule")]
    IgnoreRule(ApplicationIdentifier, String),
    ManageRule(ApplicationIdentifier, String),
    IdentifyObjectNameChangeApplication(ApplicationIdentifier, String),
    IdentifyTrayApplication(ApplicationIdentifier, String),
    IdentifyLayeredApplication(ApplicationIdentifier, String),
    IdentifyBorderOverflowApplication(ApplicationIdentifier, String),
    State,
    GlobalState,
    VisibleWindows,
    MonitorInformation,
    Query(StateQuery),
    FocusFollowsMouse(FocusFollowsMouseImplementation, bool),
    ToggleFocusFollowsMouse(FocusFollowsMouseImplementation),
    MouseFollowsFocus(bool),
    ToggleMouseFollowsFocus,
    RemoveTitleBar(ApplicationIdentifier, String),
    ToggleTitleBars,
    AddSubscriberSocket(String),
    AddSubscriberSocketWithOptions(String, SubscribeOptions),
    RemoveSubscriberSocket(String),
    AddSubscriberPipe(String),
    RemoveSubscriberPipe(String),
    ApplicationSpecificConfigurationSchema,
    NotificationSchema,
    SocketSchema,
    StaticConfigSchema,
    GenerateStaticConfig,
    DebugWindow(isize),
    // low level commands
    ApplyState(State),
}

impl SocketMessage {
    pub fn as_bytes(&self) -> eyre::Result<Vec<u8>> {
        Ok(serde_json::to_string(self)?.as_bytes().to_vec())
    }
}

impl FromStr for SocketMessage {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> eyre::Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SubscribeOptions {
    /// Only emit notifications when the window manager state has changed
    pub filter_state_changes: bool,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Display, Serialize, Deserialize, ValueEnum)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Stackbar mode
pub enum StackbarMode {
    /// Always show
    Always,
    /// Never show
    Never,
    /// Show on stack
    OnStack,
}

#[derive(Debug, Copy, Default, Clone, Eq, PartialEq, Display, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Starbar label
pub enum StackbarLabel {
    #[default]
    /// Process name
    Process,
    /// Window title
    Title,
}

#[derive(
    Default, Copy, Clone, Debug, Eq, PartialEq, Display, Serialize, Deserialize, ValueEnum,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Border style
pub enum BorderStyle {
    #[default]
    /// Use the system border style
    System,
    /// Use the Windows 11-style rounded borders
    Rounded,
    /// Use the Windows 10-style square borders
    Square,
}

#[derive(
    Default, Copy, Clone, Debug, Eq, PartialEq, Display, Serialize, Deserialize, ValueEnum,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Border style
pub enum BorderImplementation {
    #[default]
    /// Use the adjustable komorebi border implementation
    Komorebi,
    /// Use the thin Windows accent border implementation
    Windows,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    ValueEnum,
    PartialEq,
    Eq,
    Hash,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window kind
pub enum WindowKind {
    /// Single window
    Single,
    /// Stack container
    Stack,
    /// Monocle container
    Monocle,
    #[default]
    /// Unfocused window
    Unfocused,
    /// Unfocused locked container
    UnfocusedLocked,
    /// Floating window
    Floating,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, Display, EnumString, ValueEnum)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum StateQuery {
    FocusedMonitorIndex,
    FocusedWorkspaceIndex,
    FocusedContainerIndex,
    FocusedWindowIndex,
    FocusedWorkspaceName,
    FocusedWorkspaceLayout,
    FocusedContainerKind,
    Version,
}

#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Display, EnumString, ValueEnum,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Application identifier
pub enum ApplicationIdentifier {
    /// Executable name
    #[serde(alias = "exe")]
    Exe,
    /// Class
    #[serde(alias = "class")]
    Class,
    #[serde(alias = "title")]
    /// Window title
    Title,
    /// Executable path
    #[serde(alias = "path")]
    Path,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, Display, EnumString, ValueEnum)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Focus follows mouse implementation
pub enum FocusFollowsMouseImplementation {
    /// Custom FFM implementation (slightly more CPU-intensive)
    Komorebi,
    /// Native (legacy) Windows FFM implementation
    Windows,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window management behaviour
pub struct WindowManagementBehaviour {
    /// The current [`WindowContainerBehaviour`] to be used
    pub current_behaviour: WindowContainerBehaviour,
    /// Override of `current_behaviour` to open new windows as floating windows
    /// that can be later toggled to tiled, when false it will default to
    /// `current_behaviour` again.
    pub float_override: bool,
    /// Determines if a new window should be spawned floating when on the floating layer and the
    /// floating layer behaviour is set to float. This value is always calculated when checking for
    /// the management behaviour on a specific workspace.
    pub floating_layer_override: bool,
    /// The floating layer behaviour to be used if the float override is being used
    pub floating_layer_behaviour: FloatingLayerBehaviour,
    /// The `Placement` to be used when toggling a window to float
    pub toggle_float_placement: Placement,
    /// The `Placement` to be used when spawning a window on the floating layer with the
    /// `FloatingLayerBehaviour` set to `FloatingLayerBehaviour::Float`
    pub floating_layer_placement: Placement,
    /// The `Placement` to be used when spawning a window with float override active
    pub float_override_placement: Placement,
    /// The `Placement` to be used when spawning a window that matches a `floating_applications` rule
    pub float_rule_placement: Placement,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window container behaviour when a new window is opened
pub enum WindowContainerBehaviour {
    /// Create a new container for each new window
    #[default]
    Create,
    /// Append new windows to the focused window container
    Append,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Adoption behaviour for the windows which are already open when komorebi starts
pub enum WindowAdoptionBehaviour {
    /// Adopt every window already open into the first workspace's first container
    #[default]
    SingleContainer,
    /// Give every window already open a container of its own
    SeparateContainers,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Floating layer behaviour when a new window is opened
pub enum FloatingLayerBehaviour {
    /// Tile new windows (unless they match a float rule or float override is active)
    #[default]
    Tile,
    /// Float new windows
    Float,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Placement behaviour for floating windows
pub enum Placement {
    /// Does not change the size or position of the window
    #[default]
    None,
    /// Center the window without changing the size
    Center,
    /// Center the window and resize it according to the `AspectRatio`
    CenterAndResize,
}

impl FloatingLayerBehaviour {
    pub fn should_float(&self) -> bool {
        match self {
            FloatingLayerBehaviour::Tile => false,
            FloatingLayerBehaviour::Float => true,
        }
    }
}

impl Placement {
    pub fn should_center(&self) -> bool {
        match self {
            Placement::None => false,
            Placement::Center | Placement::CenterAndResize => true,
        }
    }

    pub fn should_resize(&self) -> bool {
        match self {
            Placement::None | Placement::Center => false,
            Placement::CenterAndResize => true,
        }
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    ValueEnum,
    PartialEq,
    Eq,
    Hash,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Placement strategy for new windows in a workspace
pub enum WindowPlacement {
    /// Place the new window at the primary (largest) container position
    Primary,
    /// Place the new window at the secondary container position
    Secondary,
    /// Place the new window before the currently focused container
    BeforeFocused,
    /// Place the new window after the currently focused container (default behaviour)
    #[default]
    AfterFocused,
    /// Place the new window at the end of the container list
    Last,
}

/// A target position for window placement, used as a key in `InitialWindowPlacementRules::Rules`.
///
/// Can be either a `WindowPlacement` variant name (e.g. `"Primary"`, `"Last"`)
/// or a 1-based container index (e.g. `"1"`, `"3"`).
///
/// NOTE: Integer indices are 1-based in the config for user-friendliness.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum PlacementTarget {
    /// A named placement strategy
    Placement(WindowPlacement),
    /// A 1-based container index
    Index(usize),
}

impl std::fmt::Display for PlacementTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementTarget::Placement(p) => write!(f, "{p}"),
            PlacementTarget::Index(i) => write!(f, "{i}"),
        }
    }
}

impl std::str::FromStr for PlacementTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Try parsing as a WindowPlacement variant first
        if let Ok(placement) = <WindowPlacement as std::str::FromStr>::from_str(s) {
            return Ok(PlacementTarget::Placement(placement));
        }
        // Then try parsing as a 1-based index
        if let Ok(idx) = s.parse::<usize>() {
            return Ok(PlacementTarget::Index(idx));
        }
        Err(format!(
            "'{s}' is not a valid WindowPlacement variant or integer"
        ))
    }
}

impl PartialOrd for PlacementTarget {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PlacementTarget {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl Serialize for PlacementTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlacementTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Configuration for initial window placement rules on a workspace.
///
/// This can be specified in two forms in the JSON config:
/// - A placement target (string or integer) — applies the same placement to all windows.
///   Strings are `WindowPlacement` variant names (e.g. `"Primary"`, `"AfterFocused"`),
///   integers are 1-based container indices.
/// - A map of placement targets to matching rules — keys can be `WindowPlacement` variant names
///   (e.g. `"Primary"`, `"Secondary"`) or 1-based container indices (e.g. `"1"`, `"3"`).
///   Rules are evaluated in key order; the first matching rule determines placement.
///
/// NOTE: Container indices in the config are 1-based for user-friendliness.
/// They are converted to 0-based internally during resolution.
///
/// NOTE: This feature currently only applies when `WindowContainerBehaviour::Create` is active.
/// Future versions may support toggling this for `Append` mode as well.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum InitialWindowPlacementRules {
    /// A single placement target applied to all new windows (string or integer in config)
    Target(PlacementTarget),
    /// A map of placement targets to matching rules.
    /// Keys can be `WindowPlacement` variant names (e.g. `"Primary"`) or 1-based container indices (e.g. `"1"`).
    /// Values can be:
    /// - A single `IdWithIdentifier` object (simple rule)
    /// - An array containing objects and/or arrays:
    ///   - Each object in the array is an independent simple rule (OR logic between entries)
    ///   - Each inner array is a composite rule where all conditions must match (AND logic)
    ///   - The outer array entries are evaluated with OR logic
    ///
    /// Rules are evaluated in key order; the first matching rule determines placement.
    Rules(std::collections::BTreeMap<PlacementTarget, PlacementMatchingRules>),
}

/// Matching rules for a placement target.
///
/// Can be specified in JSON as:
/// - A single `IdWithIdentifier` object — one simple rule
/// - An array of `MatchingRule`s — multiple rules with OR logic between them
///   (each element can be a simple rule object or a composite rule array with AND logic)
///
/// Examples:
/// ```json
/// // Single rule
/// { "kind": "Exe", "id": "chrome.exe", "matching_strategy": "Equals" }
///
/// // Multiple rules (OR): chrome OR teams
/// [
///   { "kind": "Exe", "id": "chrome.exe", "matching_strategy": "Equals" },
///   { "kind": "Title", "id": "Microsoft Teams", "matching_strategy": "Equals" }
/// ]
///
/// // Mixed: chrome OR (code.exe AND title contains "workspace")
/// [
///   { "kind": "Exe", "id": "chrome.exe", "matching_strategy": "Equals" },
///   [
///     { "kind": "Exe", "id": "code.exe", "matching_strategy": "Equals" },
///     { "kind": "Title", "id": "workspace", "matching_strategy": "Contains" }
///   ]
/// ]
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum PlacementMatchingRules {
    /// A single simple matching rule
    Single(config_generation::IdWithIdentifier),
    /// Multiple matching rules evaluated with OR logic.
    /// Each entry is a `MatchingRule`: either a simple rule (object) or composite rule (array, AND logic).
    Many(Vec<config_generation::MatchingRule>),
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Display, EnumString, ValueEnum,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Move behaviour when the operation works across a monitor boundary
pub enum MoveBehaviour {
    /// Swap the window container with the window container at the edge of the adjacent monitor
    #[default]
    Swap,
    /// Insert the window container into the focused workspace on the adjacent monitor
    Insert,
    /// Do nothing if trying to move a window container in the direction of an adjacent monitor
    NoOp,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Behaviour when an action would cross a monitor boundary
pub enum CrossBoundaryBehaviour {
    /// Attempt to perform actions across a workspace boundary
    Workspace,
    /// Attempt to perform actions across a monitor boundary
    #[default]
    Monitor,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Behaviour when focusing in a direction while a monocle container is active
pub enum MonocleFocusBehaviour {
    /// Cycle the monocle container to the next/previous container in the workspace
    Cycle,
    /// Do nothing, allowing focus to fall through to cross-monitor logic
    #[default]
    NoOp,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window hiding behaviour
pub enum HidingBehaviour {
    /// END OF LIFE FEATURE: Use the `SW_HIDE` flag to hide windows when switching workspaces (has issues with Electron apps)
    #[deprecated(note = "End of life feature")]
    Hide,
    /// Use the `SW_MINIMIZE` flag to hide windows when switching workspaces (has issues with frequent workspace switching)
    Minimize,
    /// Use the undocumented SetCloak Win32 function to hide windows when switching workspaces
    Cloak,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Display, EnumString, ValueEnum,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Operation behaviour for temporarily unmanaged and floating windows
pub enum OperationBehaviour {
    /// Process commands on temporarily unmanaged/floated windows
    #[default]
    Op,
    /// Ignore commands on temporarily unmanaged/floated windows
    NoOp,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, Display, EnumString, ValueEnum, PartialEq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window handling behaviour
pub enum WindowHandlingBehaviour {
    #[default]
    /// Synchronous
    Sync,
    /// Asynchronous
    Async,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes() {
        // Set a variable for testing
        unsafe {
            std::env::set_var("VAR", "VALUE");
        }

        let json = r#"{"type":"WorkspaceLayoutCustomRule","content":[0,0,0,"/path/%VAR%/d"]}"#;
        let message: SocketMessage = serde_json::from_str(json).unwrap();

        let SocketMessage::WorkspaceLayoutCustomRule(
            _workspace_index,
            _workspace_number,
            _monitor_index,
            path,
        ) = message
        else {
            panic!("Expected WorkspaceLayoutCustomRule");
        };

        assert_eq!(path, PathBuf::from("/path/VALUE/d"));
    }

    #[test]
    fn floating_commands_are_distinct_from_the_container_commands() {
        let moved: SocketMessage =
            serde_json::from_str(r#"{"type":"MoveFloatingWindow","content":["Left",120]}"#)
                .unwrap();
        assert!(matches!(
            moved,
            SocketMessage::MoveFloatingWindow(OperationDirection::Left, Some(120))
        ));

        let resized: SocketMessage = serde_json::from_str(
            r#"{"type":"ResizeFloatingWindow","content":["Right","Increase",null]}"#,
        )
        .unwrap();
        assert!(matches!(
            resized,
            SocketMessage::ResizeFloatingWindow(OperationDirection::Right, Sizing::Increase, None)
        ));
    }

    #[test]
    fn an_omitted_floating_delta_round_trips_as_null() {
        let message = SocketMessage::MoveFloatingWindow(OperationDirection::Up, None);
        let json = serde_json::to_string(&message).unwrap();

        assert_eq!(
            json,
            r#"{"type":"MoveFloatingWindow","content":["Up",null]}"#
        );

        let parsed: SocketMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            SocketMessage::MoveFloatingWindow(OperationDirection::Up, None)
        ));
    }

    /// The wire form of a command is what an AutoHotkey script, a subscriber and an older
    /// komorebic all agree on, so each new command's JSON is pinned rather than left to the
    /// derive to decide again after the next refactor.
    #[test]
    fn the_new_commands_keep_their_wire_form() {
        let cases = [
            (SocketMessage::Pause, r#"{"type":"Pause"}"#),
            (SocketMessage::Unpause, r#"{"type":"Unpause"}"#),
            (
                SocketMessage::SuspendWindow(None),
                r#"{"type":"SuspendWindow","content":null}"#,
            ),
            (
                SocketMessage::ResumeWindow(Some(4242)),
                r#"{"type":"ResumeWindow","content":4242}"#,
            ),
            (
                SocketMessage::RestoreLastMinimizedWindow,
                r#"{"type":"RestoreLastMinimizedWindow"}"#,
            ),
            (
                SocketMessage::CreateContainer(None),
                r#"{"type":"CreateContainer","content":null}"#,
            ),
            (
                SocketMessage::CreateContainer(Some(SplitAxis::TopBottom)),
                r#"{"type":"CreateContainer","content":"TopBottom"}"#,
            ),
            (
                SocketMessage::DestroyContainer,
                r#"{"type":"DestroyContainer"}"#,
            ),
            (
                SocketMessage::DestroyFocusedContainer,
                r#"{"type":"DestroyFocusedContainer"}"#,
            ),
            (
                SocketMessage::MoveWorkspaceToIndex(2),
                r#"{"type":"MoveWorkspaceToIndex","content":2}"#,
            ),
            (
                SocketMessage::CycleMoveWorkspace(CycleDirection::Previous),
                r#"{"type":"CycleMoveWorkspace","content":"Previous"}"#,
            ),
            (
                SocketMessage::SwapWorkspaceWithIndex(1),
                r#"{"type":"SwapWorkspaceWithIndex","content":1}"#,
            ),
            (
                SocketMessage::MergeFocusedWorkspace,
                r#"{"type":"MergeFocusedWorkspace"}"#,
            ),
            (
                SocketMessage::MoveWindowToWorkspaceId(String::from("abc"), true),
                r#"{"type":"MoveWindowToWorkspaceId","content":["abc",true]}"#,
            ),
            (
                SocketMessage::MoveWindowToContainerId(String::from("xyz"), false),
                r#"{"type":"MoveWindowToContainerId","content":["xyz",false]}"#,
            ),
            (
                SocketMessage::RaiseNextStackWindow,
                r#"{"type":"RaiseNextStackWindow"}"#,
            ),
            (
                SocketMessage::MergeContainer(OperationDirection::Left),
                r#"{"type":"MergeContainer","content":"Left"}"#,
            ),
            (
                SocketMessage::CycleWindowHistory(CycleDirection::Next),
                r#"{"type":"CycleWindowHistory","content":"Next"}"#,
            ),
            (
                SocketMessage::CycleContainerHistory(CycleDirection::Previous),
                r#"{"type":"CycleContainerHistory","content":"Previous"}"#,
            ),
            (
                SocketMessage::MoveWindowToMonitorNumber(1, true),
                r#"{"type":"MoveWindowToMonitorNumber","content":[1,true]}"#,
            ),
        ];

        for (message, expected) in cases {
            let json = serde_json::to_string(&message).unwrap();
            assert_eq!(json, expected);

            // Parsing what was written is what komorebi actually does with it.
            let parsed: SocketMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.to_string(), message.to_string());
        }
    }
}
