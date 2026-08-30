//! A seeded random-operation harness for the managed-window model.
//!
//! Every other test in this crate names a situation and asserts what the model does with it. This
//! one does the opposite: it drives long sequences of arbitrary operations at a workspace and
//! asserts, after every single one, that the model's invariants still hold. It is the test which
//! can find the transition nobody thought to write down.
//!
//! It is deterministic on purpose. No property-testing dependency is added for it, and the
//! generator is a plain xorshift, so a failure names a seed and an operation index which reproduce
//! it exactly.
//!
//! Only workspace-level operations are driven, because those are the ones which change ownership,
//! lifetime and geometry. Nothing here asks Win32 for anything it needs to be told: the window
//! handles name no real window, so every observation of one is the same on every run.

use crate::Window;
use crate::container::Container;
use crate::container::ContainerState;
use crate::core::DefaultLayout;
use crate::core::Layout;
use crate::core::OperationDirection;
use crate::core::Rect;
use crate::geometry::LogicalRect;
use crate::invariants::ValidateInvariants;
use crate::managed_window::ManagedPlacement;
use crate::managed_window::Visibility;
use crate::workspace::Workspace;

/// The work area every harness workspace is arranged in.
const AREA: Rect = Rect {
    left: 0,
    top: 0,
    right: 1920,
    bottom: 1080,
};

/// A deterministic generator. Not cryptographic and not meant to be.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        // A zero state would stay zero forever.
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number below `n`, or `0` when there is nothing to choose from.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
        }
    }

    fn pick<T: Copy>(&mut self, options: &[T]) -> T {
        options[self.below(options.len())]
    }
}

/// One thing which can happen to a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    /// A new window is opened on this workspace.
    OpenWindow(isize),
    /// A window is closed.
    CloseWindow(isize),
    /// A window starts or stops floating.
    Float(isize),
    Unfloat(isize),
    /// A window is minimized or restored.
    Minimize(isize),
    Unminimize(isize),
    /// A window is presented, or returns from being presented.
    Maximize(isize),
    Unmaximize,
    /// A window is focused, which moves both histories.
    Focus(isize),
    /// A container is split off an eligible donor.
    SplitContainer,
    /// A container is destroyed and its windows are distributed.
    DestroyContainer(usize),
    /// A boundary of the geometry-focused container is moved.
    Resize(OperationDirection, i32),
    /// The layout is changed, which discards every local slot edit.
    SetLayout(DefaultLayout),
    /// The arrangement is brought up to date, which is where slots are written.
    Update,
}

/// The operations which make sense for this workspace right now.
///
/// Generating only applicable operations is what keeps a long run interesting: a sequence of
/// refusals asserts far less than a sequence of transitions. Operations which can still be refused
/// are deliberately left in, because "a refusal changes nothing" is itself a property under test.
fn generate(rng: &mut Rng, workspace: &Workspace, next_hwnd: isize) -> Operation {
    let hwnds: Vec<isize> = workspace
        .containers()
        .iter()
        .flat_map(|container| container.windows().iter().map(|window| window.hwnd))
        .collect();

    let mut choices = vec![
        Operation::OpenWindow(next_hwnd),
        Operation::SplitContainer,
        Operation::Update,
        Operation::SetLayout(rng.pick(&[
            DefaultLayout::BSP,
            DefaultLayout::Columns,
            DefaultLayout::Rows,
            DefaultLayout::VerticalStack,
            DefaultLayout::UltrawideVerticalStack,
        ])),
        Operation::Resize(
            rng.pick(&[
                OperationDirection::Left,
                OperationDirection::Right,
                OperationDirection::Up,
                OperationDirection::Down,
            ]),
            rng.pick(&[-200, -50, 50, 200]),
        ),
        Operation::Unmaximize,
    ];

    if !hwnds.is_empty() {
        let hwnd = hwnds[rng.below(hwnds.len())];

        choices.extend([
            Operation::CloseWindow(hwnd),
            Operation::Float(hwnd),
            Operation::Unfloat(hwnd),
            Operation::Minimize(hwnd),
            Operation::Unminimize(hwnd),
            Operation::Maximize(hwnd),
            Operation::Focus(hwnd),
        ]);
    }

    if !workspace.containers().is_empty() {
        choices.push(Operation::DestroyContainer(
            rng.below(workspace.containers().len()),
        ));
    }

    choices[rng.below(choices.len())]
}

/// Apply an operation, reporting whether the workspace refused it.
fn apply(workspace: &mut Workspace, operation: Operation) -> bool {
    let floating_rect = Rect {
        left: 40,
        top: 40,
        right: 600,
        bottom: 400,
    };

    match operation {
        Operation::OpenWindow(hwnd) => {
            workspace.place_new_window(Window::from(hwnd));
            true
        }
        Operation::CloseWindow(hwnd) => workspace.remove_window(hwnd).is_ok(),
        Operation::Float(hwnd) => workspace.float_window(hwnd, floating_rect).is_ok(),
        Operation::Unfloat(hwnd) => workspace.unfloat_window(hwnd).is_ok(),
        Operation::Minimize(hwnd) => workspace.minimize_window(hwnd).is_ok(),
        Operation::Unminimize(hwnd) => workspace.unminimize_window(hwnd).is_ok(),
        Operation::Maximize(hwnd) => workspace.maximize_window(hwnd).is_ok(),
        Operation::Unmaximize => workspace.unmaximize_window().is_ok(),
        Operation::Focus(hwnd) => workspace.focus_container_by_window(hwnd).is_ok(),
        Operation::SplitContainer => workspace.create_container_from_donor(None).is_ok(),
        Operation::DestroyContainer(idx) => workspace.destroy_container(idx).is_ok(),
        Operation::Resize(direction, delta) => {
            workspace.resize_focused_container(direction, delta).is_ok()
        }
        Operation::SetLayout(layout) => {
            workspace.layout = Layout::Default(layout);
            workspace.invalidate_slot_geometry();
            true
        }
        Operation::Update => {
            workspace.record_logical_slots(AREA);
            true
        }
    }
}

/// Whether an error from this operation is necessarily a refusal by the model.
///
/// These operations decide entirely within the model and write nothing until they have decided, so
/// an error from one of them means nothing happened and invariant 15 - a compound operation either
/// succeeds completely or changes nothing - is directly testable.
///
/// The window lifecycle operations are excluded because they end by asking Windows to focus or
/// position a window, and report a failure of *that* after the model has already committed. In
/// this harness every such call fails, because these handles name no real window, so their errors
/// say nothing about whether the model changed.
const fn refuses_before_writing(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::SplitContainer
            | Operation::DestroyContainer(_)
            | Operation::Resize(_, _)
            | Operation::Unmaximize
    )
}

/// The model rules which have to hold after every operation.
///
/// The ownership, lifetime, focus and history rules come from `validate_invariants`, which is the
/// same check the window manager runs at every point of rest. The slot rules which only hold for a
/// recorded arrangement are checked here, where the harness knows an arrangement was just recorded.
fn check(workspace: &Workspace, context: &str, arrangement_is_current: bool) {
    let violations = workspace.validate_invariants();

    assert!(
        violations.is_empty(),
        "{context}: {}",
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    );

    for container in workspace.containers() {
        let holds_slot = workspace.logical_slots.contains(&container.id);

        match container.state() {
            ContainerState::Hidden => {
                // A container becomes Hidden the moment its last visible stored window does, and
                // gives its slot up when the arrangement is next recorded. Between those two
                // points it still holds the slot it had, which is what makes an exact restore
                // possible at all, so this is only a rule about a recorded arrangement.
                if arrangement_is_current {
                    assert!(
                        !holds_slot,
                        "{context}: hidden container {} holds an active slot",
                        container.id
                    );
                }
            }
            ContainerState::Active => {
                assert!(
                    container.windows().iter().any(|window| {
                        window.placement == ManagedPlacement::Stored
                            && window.visibility == Visibility::Visible
                    }),
                    "{context}: container {} is active with no visible stored window",
                    container.id
                );

                if arrangement_is_current {
                    assert!(
                        holds_slot,
                        "{context}: active container {} holds no slot",
                        container.id
                    );
                }
            }
        }
    }

    if arrangement_is_current
        && let Err(violations) = workspace
            .logical_slots
            .validate_coverage(LogicalRect::from(AREA))
    {
        panic!(
            "{context}: the active slots do not tile the work area: {}",
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

/// Drive one seed and return the workspace it ended with.
fn run(seed: u64, operations: usize) -> Workspace {
    let mut rng = Rng::new(seed);
    let mut workspace = Workspace::default();
    let mut next_hwnd = 1;

    workspace.record_logical_slots(AREA);

    for step in 0..operations {
        let operation = generate(&mut rng, &workspace, next_hwnd);

        if let Operation::OpenWindow(_) = operation {
            next_hwnd += 1;
        }

        let before = workspace.clone();
        let accepted = apply(&mut workspace, operation);
        let context = format!("seed {seed}, step {step}, {operation:?}");

        // A refused operation is not allowed to have changed anything on its way to refusing.
        if !accepted && refuses_before_writing(operation) {
            assert_eq!(workspace, before, "{context}: a refusal changed the model");
        }

        check(&workspace, &context, matches!(operation, Operation::Update));

        // Whatever the operation was, the arrangement can always be brought up to date, and
        // everything must still hold once it has been.
        let mut settled = workspace.clone();
        settled.record_logical_slots(AREA);
        check(&settled, &format!("{context} (settled)"), true);
    }

    workspace
}

#[test]
fn random_operation_sequences_keep_the_model_consistent() {
    for seed in 1..=24u64 {
        run(seed, 120);
    }
}

#[test]
fn a_long_run_from_one_seed_keeps_the_model_consistent() {
    run(0xC0FFEE, 1500);
}

/// One container as a run's shape sees it: which windows it holds, in which state, and the slot it
/// holds if it holds one.
type ContainerShape = (
    Vec<(isize, ManagedPlacement, Visibility)>,
    Option<LogicalRect>,
);

/// What a workspace looks like, with the stable identities left out.
///
/// Container and workspace IDs are generated randomly, so two runs of the same seed are equal in
/// every way except the names of the objects they made. This is what "the same run" means here.
fn shape(workspace: &Workspace) -> Vec<ContainerShape> {
    workspace
        .containers()
        .iter()
        .map(|container| {
            (
                container
                    .windows()
                    .iter()
                    .map(|window| (window.hwnd, window.placement, window.visibility))
                    .collect(),
                workspace.logical_slots.get(&container.id),
            )
        })
        .collect()
}

#[test]
fn the_generator_is_deterministic() {
    let first = run(7, 60);
    let second = run(7, 60);

    assert_eq!(shape(&first), shape(&second));
}

#[test]
fn the_harness_reaches_the_states_it_is_meant_to_exercise() {
    // A run which never hides a container, never stacks and never empties the workspace would
    // assert far less than it appears to, so this is a check on the harness rather than on the
    // model.
    let mut hidden = false;
    let mut stacked = false;
    let mut emptied = false;
    let mut floating = false;
    let mut minimized = false;

    for seed in 1..=24u64 {
        let mut rng = Rng::new(seed);
        let mut workspace = Workspace::default();
        let mut next_hwnd = 1;

        for _ in 0..120 {
            let operation = generate(&mut rng, &workspace, next_hwnd);

            if let Operation::OpenWindow(_) = operation {
                next_hwnd += 1;
            }

            apply(&mut workspace, operation);

            hidden |= workspace.containers().iter().any(Container::is_hidden);
            stacked |= workspace
                .containers()
                .iter()
                .any(|container| container.windows().len() > 1);
            emptied |= workspace.containers().is_empty();
            floating |= workspace.floating_managed_windows().count() > 0;
            minimized |= workspace
                .containers()
                .iter()
                .flat_map(|container| container.windows().iter())
                .any(|window| window.visibility == Visibility::Minimized);
        }
    }

    assert!(hidden, "no run ever hid a container");
    assert!(stacked, "no run ever stacked two windows in a container");
    assert!(emptied, "no run ever emptied the workspace");
    assert!(floating, "no run ever floated a window");
    assert!(minimized, "no run ever minimized a window");
}
