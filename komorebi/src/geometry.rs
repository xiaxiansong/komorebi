//! Gap-free logical geometry for tiled container slots.
//!
//! Every layout decision - splitting, adjacency, absorption, resizing and coverage validation -
//! operates on [`LogicalRect`]. Workspace padding, container gaps and border insets are applied
//! only when a logical slot is converted into the [`Rect`] that is handed to Win32, so gaps can
//! never change which slots are considered adjacent, and rounding can never open a hole between
//! two slots that logically share an edge.

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde::Serialize;

use crate::core::OperationDirection;
use crate::core::Rect;
use crate::model::ContainerId;

/// The smallest edge length a logical slot may have.
///
/// Splitting and resizing refuse to produce anything smaller so that a slot always has a
/// positive area and can still be rendered after gaps are applied.
pub const MIN_SLOT_EDGE: i32 = 2;

/// A gap-free slot rectangle.
///
/// Deliberately not `Rect`: the field names differ so that a logical slot cannot be handed to a
/// Win32 positioning call, nor a raw window rectangle used as a slot, without an explicit
/// conversion.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LogicalRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl LogicalRect {
    #[must_use]
    pub const fn new(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    /// The exclusive right edge.
    #[must_use]
    pub const fn right(self) -> i32 {
        self.left + self.width
    }

    /// The exclusive bottom edge.
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.top + self.height
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    #[must_use]
    pub const fn area(self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.width as i64 * self.height as i64
        }
    }

    /// The centre point, used as a placement anchor when an exact slot cannot be restored.
    #[must_use]
    pub const fn center(self) -> (i32, i32) {
        (self.left + self.width / 2, self.top + self.height / 2)
    }

    #[must_use]
    pub const fn contains_point(self, point: (i32, i32)) -> bool {
        point.0 >= self.left
            && point.0 < self.right()
            && point.1 >= self.top
            && point.1 < self.bottom()
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        other.left >= self.left
            && other.top >= self.top
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The shared area of two slots, or `None` when they only touch or are disjoint.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        (right > left && bottom > top).then(|| Self::new(left, top, right - left, bottom - top))
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    /// The coordinate of the named edge.
    #[must_use]
    pub const fn edge(self, direction: OperationDirection) -> i32 {
        match direction {
            OperationDirection::Left => self.left,
            OperationDirection::Right => self.right(),
            OperationDirection::Up => self.top,
            OperationDirection::Down => self.bottom(),
        }
    }

    /// The interval this slot occupies along the named edge.
    ///
    /// Left and right edges are vertical, so they project onto the y axis; up and down edges
    /// project onto the x axis. Absorption and deletion use these projections to decide whether
    /// a group of neighbours covers a whole edge.
    #[must_use]
    pub const fn projection(self, direction: OperationDirection) -> (i32, i32) {
        match direction {
            OperationDirection::Left | OperationDirection::Right => (self.top, self.bottom()),
            OperationDirection::Up | OperationDirection::Down => (self.left, self.right()),
        }
    }

    /// True when `other` sits immediately on the `direction` side of `self` and the two share a
    /// strictly positive length of that edge.
    ///
    /// A shared corner is not adjacency: the overlapping projection must have positive length.
    #[must_use]
    pub fn is_neighbour(self, other: Self, direction: OperationDirection) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }

        if self.edge(direction) != other.edge(direction.opposite()) {
            return false;
        }

        let (self_start, self_end) = self.projection(direction);
        let (other_start, other_end) = other.projection(direction);

        self_start.max(other_start) < self_end.min(other_end)
    }

    /// Move the named edge to `coordinate`, keeping the opposite edge fixed.
    ///
    /// This is the only growth primitive absorption needs: a container expanding into a freed
    /// slot changes exactly one edge, so it can only change width or height, never both.
    #[must_use]
    pub fn with_edge_at(mut self, direction: OperationDirection, coordinate: i32) -> Self {
        match direction {
            OperationDirection::Left => {
                self.width = self.right() - coordinate;
                self.left = coordinate;
            }
            OperationDirection::Right => self.width = coordinate - self.left,
            OperationDirection::Up => {
                self.height = self.bottom() - coordinate;
                self.top = coordinate;
            }
            OperationDirection::Down => self.height = coordinate - self.top,
        }

        self
    }

    /// The axis a fresh container should be split off along: always the longer edge, and
    /// left/right when the slot is square.
    #[must_use]
    pub const fn longer_edge_axis(self) -> SplitAxis {
        if self.height > self.width {
            SplitAxis::TopBottom
        } else {
            SplitAxis::LeftRight
        }
    }

    /// Split this slot 50:50 for a newly created container.
    ///
    /// Returns `None` when either half would fall below [`MIN_SLOT_EDGE`]. The two halves exactly
    /// tile the original slot, and an odd remainder pixel always goes to the existing container.
    #[must_use]
    pub fn split(self, axis: SplitAxis) -> Option<SplitResult> {
        if self.is_empty() {
            return None;
        }

        match axis {
            SplitAxis::LeftRight => {
                let new_width = self.width / 2;
                let existing_width = self.width - new_width;

                if new_width < MIN_SLOT_EDGE || existing_width < MIN_SLOT_EDGE {
                    return None;
                }

                Some(SplitResult {
                    axis,
                    // A left/right split puts the new container on the left.
                    new_slot: Self::new(self.left, self.top, new_width, self.height),
                    existing_slot: Self::new(
                        self.left + new_width,
                        self.top,
                        existing_width,
                        self.height,
                    ),
                })
            }
            SplitAxis::TopBottom => {
                let new_height = self.height / 2;
                let existing_height = self.height - new_height;

                if new_height < MIN_SLOT_EDGE || existing_height < MIN_SLOT_EDGE {
                    return None;
                }

                Some(SplitResult {
                    axis,
                    // A top/bottom split keeps the existing container on top.
                    new_slot: Self::new(
                        self.left,
                        self.top + existing_height,
                        self.width,
                        new_height,
                    ),
                    existing_slot: Self::new(self.left, self.top, self.width, existing_height),
                })
            }
        }
    }

    /// Split along the longer edge.
    #[must_use]
    pub fn split_longer_edge(self) -> Option<SplitResult> {
        self.split(self.longer_edge_axis())
    }
}

impl From<Rect> for LogicalRect {
    fn from(rect: Rect) -> Self {
        // `Rect::right` and `Rect::bottom` are a width and a height, not edge coordinates.
        Self::new(rect.left, rect.top, rect.right, rect.bottom)
    }
}

impl From<LogicalRect> for Rect {
    fn from(slot: LogicalRect) -> Self {
        Self {
            left: slot.left,
            top: slot.top,
            right: slot.width,
            bottom: slot.height,
        }
    }
}

impl fmt::Display for LogicalRect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}, {}) {}x{}",
            self.left, self.top, self.width, self.height
        )
    }
}

/// The dividing line used when a slot is split in two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum SplitAxis {
    /// A vertical dividing line producing a left and a right slot.
    LeftRight,
    /// A horizontal dividing line producing a top and a bottom slot.
    TopBottom,
}

/// The two halves produced by [`LogicalRect::split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitResult {
    pub axis: SplitAxis,
    /// The slot for the container being created.
    pub new_slot: LogicalRect,
    /// The slot the donor container keeps; it absorbs any odd remainder pixel.
    pub existing_slot: LogicalRect,
}

/// The gaps and insets applied when a logical slot becomes a window rectangle.
///
/// This is the only place gaps exist. Splitting, adjacency, absorption and coverage validation
/// never see them, so changing a gap size can never change which containers are neighbours.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderInsets {
    /// The gap between neighbouring containers, applied as a uniform inset per slot.
    pub container_padding: i32,
    /// How far outside the window its border is drawn.
    pub border_offset: i32,
    /// The width of the border drawn around the window.
    pub border_width: i32,
    /// The strip reserved at the top of the slot for a stackbar, when the container has one.
    pub stackbar_height: i32,
}

impl LogicalRect {
    /// Convert a gap-free slot into the rectangle a stored window is actually positioned to.
    ///
    /// Insets are applied in the order the renderer has always applied them: the container gap
    /// first, then the border offset and width, then the stackbar strip off the top edge.
    #[must_use]
    pub fn to_render_rect(self, insets: RenderInsets) -> Rect {
        let mut rect: Rect = self.into();

        rect.add_padding(insets.container_padding);
        rect.add_padding(insets.border_offset);
        rect.add_padding(insets.border_width);

        if insets.stackbar_height > 0 {
            rect.top += insets.stackbar_height;
            rect.bottom -= insets.stackbar_height;
        }

        rect
    }
}

// Deterministic orderings used when several slots are equally valid candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOrder {
    /// Top to bottom, then left to right: used along left and right edges.
    TopToBottom,
    /// Left to right, then top to bottom: used along up and down edges.
    LeftToRight,
}

impl SlotOrder {
    /// The ordering to use when scanning the neighbours found on `direction`.
    #[must_use]
    pub const fn for_direction(direction: OperationDirection) -> Self {
        match direction {
            OperationDirection::Left | OperationDirection::Right => Self::TopToBottom,
            OperationDirection::Up | OperationDirection::Down => Self::LeftToRight,
        }
    }

    const fn key(self, slot: LogicalRect) -> (i32, i32) {
        match self {
            Self::TopToBottom => (slot.top, slot.left),
            Self::LeftToRight => (slot.left, slot.top),
        }
    }
}

/// Sort `slots` into the deterministic order used to pick neighbours and post-operation focus.
pub fn sort_slots(slots: &mut [(ContainerId, LogicalRect)], order: SlotOrder) {
    slots.sort_by_key(|(_, slot)| order.key(*slot));
}

/// The directions a freed slot offers itself to its neighbours, in priority order.
///
/// The order is fixed rather than chosen per situation so that hiding, restoring and deleting a
/// container all redistribute its area the same way, and so that the same topology always produces
/// the same result.
pub const ABSORPTION_DIRECTIONS: [OperationDirection; 4] = [
    OperationDirection::Left,
    OperationDirection::Right,
    OperationDirection::Up,
    OperationDirection::Down,
];

/// One container's part in a [`SlotShift`]: the slot it holds now and the slot it will hold.
///
/// Exactly one edge differs between the two, so a mover can only change its width or its height,
/// never both, and never its position along the other axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotMove {
    pub container: ContainerId,
    pub before: LogicalRect,
    pub after: LogicalRect,
}

/// A validated, not yet applied, transfer of one whole slot between a container and its neighbours.
///
/// The same value describes both directions. In an absorption `slot` is given up by `container` and
/// shared out among `movers`; in a release it is taken back by `container` and the movers shrink to
/// exactly the rectangles they had before. Nothing is written until the plan is applied, which is
/// what lets a caller refuse a whole operation without having half-changed the geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotShift {
    /// The container giving up or taking back `slot`.
    pub container: ContainerId,
    /// The slot being redistributed.
    pub slot: LogicalRect,
    /// The side of `slot` the movers are on.
    pub direction: OperationDirection,
    /// The neighbours changing size, in the deterministic order for `direction`.
    pub movers: Vec<SlotMove>,
}

impl SlotShift {
    /// The movers' current rectangles, in plan order, as a restoration record stores them.
    #[must_use]
    pub fn rects_before(&self) -> Vec<(ContainerId, LogicalRect)> {
        self.movers
            .iter()
            .map(|mover| (mover.container.clone(), mover.before))
            .collect()
    }

    /// The first mover in the deterministic order for this direction.
    ///
    /// This is the container focus moves to after a deletion: top to bottom along a vertical edge,
    /// left to right along a horizontal one.
    #[must_use]
    pub fn first_mover(&self) -> Option<&ContainerId> {
        self.movers.first().map(|mover| &mover.container)
    }
}

/// A validated, not yet applied, division of one container's slot into two.
///
/// Like [`SlotShift`], nothing is written until the plan is applied, so a caller which cannot
/// complete the rest of its operation can drop the plan and leave the arrangement untouched. The
/// two halves exactly tile the donor's original slot, so applying one can neither open a hole nor
/// create an overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSplit {
    /// The container giving up half of its slot, and the half it keeps.
    pub donor: SlotMove,
    /// The container being created.
    pub created: ContainerId,
    /// The half the created container receives.
    pub created_slot: LogicalRect,
    /// The axis the donor's slot was divided along.
    pub axis: SplitAxis,
}

/// A validated, not yet applied, move of one shared boundary between slots.
///
/// A boundary is a whole line, not one container's edge: every active container which touches it
/// moves, on both sides, or the tiling would open a hole. Like [`SlotShift`] and [`SlotSplit`]
/// nothing is written until the plan is applied, and the plan is only ever produced for a delta
/// which has already been clamped into the legal range, so applying one cannot fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotResize {
    /// The side of the target the boundary is on.
    pub direction: OperationDirection,
    /// Where the boundary is now.
    pub before: i32,
    /// Where it is going. Never equal to `before`.
    pub after: i32,
    /// Every container whose edge moves, both sides of the boundary, in the deterministic order
    /// along it.
    pub movers: Vec<SlotMove>,
}

impl SlotResize {
    /// How far the boundary actually moves, which is the requested delta only when the request
    /// was already legal.
    #[must_use]
    pub const fn shift(&self) -> i32 {
        self.after - self.before
    }
}

/// A way in which a set of slots fails to be a valid tiling of a work area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotViolation {
    Empty {
        container: ContainerId,
        slot: LogicalRect,
    },
    OutsideArea {
        container: ContainerId,
        slot: LogicalRect,
        area: LogicalRect,
    },
    Overlap {
        first: ContainerId,
        second: ContainerId,
        overlap: LogicalRect,
    },
    IncompleteCoverage {
        covered: i64,
        expected: i64,
    },
}

impl fmt::Display for SlotViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { container, slot } => {
                write!(f, "container {container} has an empty logical slot {slot}")
            }
            Self::OutsideArea {
                container,
                slot,
                area,
            } => write!(
                f,
                "container {container} slot {slot} is not contained by the work area {area}"
            ),
            Self::Overlap {
                first,
                second,
                overlap,
            } => write!(f, "containers {first} and {second} overlap over {overlap}"),
            Self::IncompleteCoverage { covered, expected } => write!(
                f,
                "logical slots cover {covered} of {expected} work area pixels"
            ),
        }
    }
}

/// The identity-keyed logical geometry of one workspace.
///
/// Slots are keyed by [`ContainerId`] rather than by container index so that reordering,
/// insertion and removal cannot silently reassign geometry to a different container.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LogicalSlots {
    #[serde(default)]
    slots: HashMap<ContainerId, LogicalRect>,
    /// Incremented by every geometry change, so a stored restoration record can tell whether the
    /// topology it was captured against still holds.
    #[serde(default)]
    generation: u64,
}

impl LogicalSlots {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Record an unrelated geometry change so stale restoration records are invalidated.
    pub const fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    #[must_use]
    pub fn get(&self, id: &ContainerId) -> Option<LogicalRect> {
        self.slots.get(id).copied()
    }

    #[must_use]
    pub fn contains(&self, id: &ContainerId) -> bool {
        self.slots.contains_key(id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ContainerId, &LogicalRect)> {
        self.slots.iter()
    }

    /// Every slot in a deterministic order.
    #[must_use]
    pub fn ordered(&self, order: SlotOrder) -> Vec<(ContainerId, LogicalRect)> {
        let mut entries: Vec<_> = self
            .slots
            .iter()
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        sort_slots(&mut entries, order);
        entries
    }

    pub fn set(&mut self, id: ContainerId, slot: LogicalRect) {
        if self.slots.insert(id, slot) != Some(slot) {
            self.bump_generation();
        }
    }

    /// Replace every slot at once, as a layout recalculation does.
    pub fn replace_all(&mut self, slots: impl IntoIterator<Item = (ContainerId, LogicalRect)>) {
        self.slots = slots.into_iter().collect();
        self.bump_generation();
    }

    pub fn remove(&mut self, id: &ContainerId) -> Option<LogicalRect> {
        let removed = self.slots.remove(id);

        if removed.is_some() {
            self.bump_generation();
        }

        removed
    }

    /// Drop the slots of containers that no longer exist.
    pub fn retain(&mut self, keep: impl Fn(&ContainerId) -> bool) {
        let before = self.slots.len();
        self.slots.retain(|id, _| keep(id));

        if self.slots.len() != before {
            self.bump_generation();
        }
    }

    pub fn clear(&mut self) {
        if !self.slots.is_empty() {
            self.slots.clear();
            self.bump_generation();
        }
    }

    /// The neighbours on the `direction` side of `slot` which together cover that whole edge.
    ///
    /// A group is only usable when it covers the edge completely, with no overlap and no gap: any
    /// other group would leave a hole or produce an overlap once the slot is given away. The
    /// containers are returned in the deterministic order for `direction`.
    #[must_use]
    pub fn complete_edge_group(
        &self,
        slot: LogicalRect,
        direction: OperationDirection,
        exclude: &ContainerId,
    ) -> Option<Vec<(ContainerId, LogicalRect)>> {
        if slot.is_empty() {
            return None;
        }

        let group = self.neighbours_on_edge(slot, direction, exclude);

        if group.is_empty() {
            return None;
        }

        // The edge is one interval. Walking the candidates in order, each must begin exactly where
        // the previous one ended, the first must begin where the edge begins, and the last must end
        // where it ends. That single sweep rejects overlaps, gaps and partial cover at once.
        let (edge_start, edge_end) = slot.projection(direction);
        let mut covered = edge_start;

        for (_, candidate) in &group {
            let (start, end) = candidate.projection(direction);

            if start != covered {
                return None;
            }

            covered = end;
        }

        (covered == edge_end).then_some(group)
    }

    /// Plan giving `slot` away to whichever neighbour group can take all of it.
    ///
    /// Directions are tried in [`ABSORPTION_DIRECTIONS`] order and the first complete group wins.
    /// `None` means no single edge can absorb the slot, which is the caller's signal to fall back
    /// to a full relayout rather than to leave a hole.
    #[must_use]
    pub fn plan_absorption(&self, container: &ContainerId) -> Option<SlotShift> {
        let slot = self.get(container)?;

        ABSORPTION_DIRECTIONS.into_iter().find_map(|direction| {
            let group = self.complete_edge_group(slot, direction, container)?;
            let opposite = direction.opposite();
            let coordinate = slot.edge(opposite);

            let movers = group
                .into_iter()
                .map(|(id, before)| SlotMove {
                    container: id,
                    before,
                    // The mover reaches across the freed slot by moving the edge which faced it,
                    // keeping its far edge where it is.
                    after: before.with_edge_at(opposite, coordinate),
                })
                .collect();

            Some(SlotShift {
                container: container.clone(),
                slot,
                direction,
                movers,
            })
        })
    }

    /// Plan giving `slot` back to `container`, shrinking the recorded movers to exactly the
    /// rectangles they had before they absorbed it.
    ///
    /// Every recorded mover must still exist and must still hold exactly the rectangle the
    /// absorption gave it. That single equality is the whole validity test: if it holds for all of
    /// them, moving each edge back releases precisely `slot` and nothing else, and if it fails for
    /// any of them the topology has changed underneath the record and an exact restore would open a
    /// hole or an overlap. `None` is the caller's signal to fall back to a full relayout.
    #[must_use]
    pub fn plan_release(
        &self,
        container: &ContainerId,
        slot: LogicalRect,
        direction: OperationDirection,
        recorded: &[(ContainerId, LogicalRect)],
    ) -> Option<SlotShift> {
        if slot.is_empty() || recorded.is_empty() || self.contains(container) {
            return None;
        }

        let opposite = direction.opposite();
        let coordinate = slot.edge(opposite);
        let mut movers = Vec::with_capacity(recorded.len());

        for (id, before) in recorded {
            let current = self.get(id)?;

            if current != before.with_edge_at(opposite, coordinate) {
                return None;
            }

            // The rectangle being restored was a valid slot when it was recorded, but it arrives
            // here from stored state, so it is checked rather than trusted.
            if before.width < MIN_SLOT_EDGE || before.height < MIN_SLOT_EDGE {
                return None;
            }

            movers.push(SlotMove {
                container: id.clone(),
                before: current,
                after: *before,
            });
        }

        Some(SlotShift {
            container: container.clone(),
            slot,
            direction,
            movers,
        })
    }

    /// Apply an absorption: the movers grow and the container gives up its slot.
    pub fn apply_absorption(&mut self, shift: &SlotShift) {
        for mover in &shift.movers {
            self.slots.insert(mover.container.clone(), mover.after);
        }

        self.slots.remove(&shift.container);
        self.bump_generation();
    }

    /// Apply a release: the movers shrink and the container takes its slot back.
    pub fn apply_release(&mut self, shift: &SlotShift) {
        for mover in &shift.movers {
            self.slots.insert(mover.container.clone(), mover.after);
        }

        self.slots.insert(shift.container.clone(), shift.slot);
        self.bump_generation();
    }

    /// The neighbours on the `direction` side of `slot`, in the deterministic order for that
    /// direction and excluding `exclude`.
    ///
    /// Adjacency here is the shared-edge relation of [`LogicalRect::is_neighbour`]: a shared corner
    /// is not adjacency, and a gap is not adjacency, so a caller can rely on every returned slot
    /// having a positive length of edge in common with `slot`.
    #[must_use]
    pub fn neighbours_on_edge(
        &self,
        slot: LogicalRect,
        direction: OperationDirection,
        exclude: &ContainerId,
    ) -> Vec<(ContainerId, LogicalRect)> {
        let mut neighbours: Vec<(ContainerId, LogicalRect)> = self
            .slots
            .iter()
            .filter(|(id, candidate)| *id != exclude && slot.is_neighbour(**candidate, direction))
            .map(|(id, candidate)| (id.clone(), *candidate))
            .collect();

        sort_slots(&mut neighbours, SlotOrder::for_direction(direction));

        neighbours
    }

    /// The neighbour a container hands work to when it is not going to be split.
    ///
    /// Directions are tried in [`ABSORPTION_DIRECTIONS`] order and the first neighbour found in the
    /// deterministic order for that direction wins, so the same topology always chooses the same
    /// container. Unlike an absorption this does not require the neighbours to cover a whole edge:
    /// no area changes hands, so a partial neighbour is a perfectly good recipient.
    #[must_use]
    pub fn adjacent_neighbour(&self, container: &ContainerId) -> Option<ContainerId> {
        let slot = self.get(container)?;

        ABSORPTION_DIRECTIONS.into_iter().find_map(|direction| {
            self.neighbours_on_edge(slot, direction, container)
                .into_iter()
                .next()
                .map(|(id, _)| id)
        })
    }

    /// Plan dividing `donor`'s slot 50:50 to make room for `created`.
    ///
    /// `axis` forces the dividing line; `None` divides the longer edge, and a square slot divides
    /// left to right. The odd remainder pixel of an oddly sized slot goes to the donor, and the
    /// created container takes the left half of a left/right split and the bottom half of a
    /// top/bottom split. `None` means the slot cannot be halved without falling below
    /// [`MIN_SLOT_EDGE`], or that the request itself is inconsistent, and it is the caller's signal
    /// to refuse the whole operation rather than to create a container with no slot.
    #[must_use]
    pub fn plan_split(
        &self,
        donor: &ContainerId,
        created: &ContainerId,
        axis: Option<SplitAxis>,
    ) -> Option<SlotSplit> {
        if donor == created || self.contains(created) {
            return None;
        }

        let slot = self.get(donor)?;
        let result = slot.split(axis.unwrap_or_else(|| slot.longer_edge_axis()))?;

        Some(SlotSplit {
            donor: SlotMove {
                container: donor.clone(),
                before: slot,
                after: result.existing_slot,
            },
            created: created.clone(),
            created_slot: result.new_slot,
            axis: result.axis,
        })
    }

    /// The slots whose `edge` lies on `coordinate` and which overlap `interval` along it.
    #[must_use]
    fn slots_on_boundary(
        &self,
        coordinate: i32,
        edge: OperationDirection,
        interval: (i32, i32),
    ) -> Vec<(ContainerId, LogicalRect)> {
        let mut found: Vec<(ContainerId, LogicalRect)> = self
            .slots
            .iter()
            .filter(|(_, slot)| !slot.is_empty() && slot.edge(edge) == coordinate)
            .filter(|(_, slot)| {
                let (start, end) = slot.projection(edge);

                start.max(interval.0) < end.min(interval.1)
            })
            .map(|(id, slot)| (id.clone(), *slot))
            .collect();

        sort_slots(&mut found, SlotOrder::for_direction(edge));

        found
    }

    /// Whether these slots tile `interval` along `edge` exactly: no gap, no overlap, no overhang.
    fn tiles_interval(
        group: &[(ContainerId, LogicalRect)],
        edge: OperationDirection,
        interval: (i32, i32),
    ) -> bool {
        let mut covered = interval.0;

        for (_, slot) in group {
            let (start, end) = slot.projection(edge);

            if start != covered {
                return false;
            }

            covered = end;
        }

        covered == interval.1
    }

    /// Plan moving the boundary on the `direction` side of `container` by `delta`.
    ///
    /// A positive `delta` grows the container, whichever side the boundary is on, so the caller
    /// never has to think about which way the coordinate axis runs. The delta is clamped to the
    /// range which keeps every affected slot at or above [`MIN_SLOT_EDGE`], so a request which is
    /// too large moves the boundary as far as it legally can rather than refusing.
    ///
    /// `None` means the boundary is not one this container can move: it has no slot, it is at the
    /// edge of the work area, the containers on the two sides do not line up into a single clean
    /// line, or there is no room to move it at all. Refusing is the whole point - the alternative
    /// would be a hole or an overlap.
    #[must_use]
    pub fn plan_edge_resize(
        &self,
        container: &ContainerId,
        direction: OperationDirection,
        delta: i32,
    ) -> Option<SlotResize> {
        let slot = self.get(container)?;
        let opposite = direction.opposite();
        let boundary = slot.edge(direction);

        // The boundary starts as this container's edge and grows to take in every slot which
        // touches what it has grown to so far. It only ever grows and it is bounded by the work
        // area, so the loop terminates; the fixpoint is the whole line the two sides share.
        let mut interval = slot.projection(direction);
        let (near, far) = loop {
            let near = self.slots_on_boundary(boundary, direction, interval);
            let far = self.slots_on_boundary(boundary, opposite, interval);

            if far.is_empty() {
                // Nothing on the other side: this is the edge of the work area.
                return None;
            }

            let span = near
                .iter()
                .chain(far.iter())
                .map(|(_, slot)| slot.projection(direction))
                .fold(
                    interval,
                    |(start, end), (candidate_start, candidate_end)| {
                        (start.min(candidate_start), end.max(candidate_end))
                    },
                );

            if span == interval {
                break (near, far);
            }

            interval = span;
        };

        if !Self::tiles_interval(&near, direction, interval)
            || !Self::tiles_interval(&far, opposite, interval)
        {
            return None;
        }

        // Which way the coordinate has to move for the container to grow.
        let outward = match direction {
            OperationDirection::Right | OperationDirection::Down => 1,
            OperationDirection::Left | OperationDirection::Up => -1,
        };

        // A near slot grows as the boundary moves outward; a far slot shrinks by the same amount.
        // Each mover's size along the moving axis is `size + coefficient * shift`, so the legal
        // range of `shift` is one bound per mover.
        let extent = |slot: LogicalRect| match direction {
            OperationDirection::Left | OperationDirection::Right => slot.width,
            OperationDirection::Up | OperationDirection::Down => slot.height,
        };

        let mut lower = i32::MIN;
        let mut upper = i32::MAX;

        for (_, slot) in &near {
            // size + outward * shift >= MIN
            let bound = (MIN_SLOT_EDGE - extent(*slot)) * outward;

            if outward > 0 {
                lower = lower.max(bound);
            } else {
                upper = upper.min(bound);
            }
        }

        for (_, slot) in &far {
            // size - outward * shift >= MIN
            let bound = (extent(*slot) - MIN_SLOT_EDGE) * outward;

            if outward > 0 {
                upper = upper.min(bound);
            } else {
                lower = lower.max(bound);
            }
        }

        if lower > upper {
            return None;
        }

        let shift = delta.saturating_mul(outward).clamp(lower, upper);

        if shift == 0 {
            return None;
        }

        let after = boundary + shift;
        let movers = near
            .into_iter()
            .map(|(id, before)| SlotMove {
                container: id,
                before,
                after: before.with_edge_at(direction, after),
            })
            .chain(far.into_iter().map(|(id, before)| SlotMove {
                container: id,
                before,
                after: before.with_edge_at(opposite, after),
            }))
            .collect();

        Some(SlotResize {
            direction,
            before: boundary,
            after,
            movers,
        })
    }

    /// Apply a boundary move: every container which touches the boundary takes its new rectangle.
    pub fn apply_edge_resize(&mut self, resize: &SlotResize) {
        for mover in &resize.movers {
            self.slots.insert(mover.container.clone(), mover.after);
        }

        self.bump_generation();
    }

    /// Apply a split: the donor shrinks to its half and the created container takes the other.
    pub fn apply_split(&mut self, split: &SlotSplit) {
        self.slots
            .insert(split.donor.container.clone(), split.donor.after);
        self.slots.insert(split.created.clone(), split.created_slot);
        self.bump_generation();
    }

    /// Check that these slots tile `area` exactly.
    ///
    /// Containment plus pairwise non-overlap plus an exact total area is equivalent to gap-free
    /// full coverage, and it avoids materialising a per-pixel map. An empty slot set is vacuously
    /// valid: a workspace is allowed to have no container occupying a slot.
    pub fn validate_coverage(&self, area: LogicalRect) -> Result<(), Vec<SlotViolation>> {
        if self.slots.is_empty() {
            return Ok(());
        }

        let mut violations = vec![];
        let entries = self.ordered(SlotOrder::TopToBottom);
        let mut covered = 0i64;

        for (id, slot) in &entries {
            if slot.is_empty() {
                violations.push(SlotViolation::Empty {
                    container: id.clone(),
                    slot: *slot,
                });

                continue;
            }

            if !area.contains(*slot) {
                violations.push(SlotViolation::OutsideArea {
                    container: id.clone(),
                    slot: *slot,
                    area,
                });
            }

            covered += slot.area();
        }

        for (first_idx, (first_id, first)) in entries.iter().enumerate() {
            for (second_id, second) in entries.iter().skip(first_idx + 1) {
                if let Some(overlap) = first.intersection(*second) {
                    violations.push(SlotViolation::Overlap {
                        first: first_id.clone(),
                        second: second_id.clone(),
                        overlap,
                    });
                }
            }
        }

        if covered != area.area() {
            violations.push(SlotViolation::IncompleteCoverage {
                covered,
                expected: area.area(),
            });
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> LogicalRect {
        LogicalRect::new(0, 0, 1920, 1080)
    }

    fn id(value: &str) -> ContainerId {
        ContainerId::from(value)
    }

    /// Three containers: `left` over the left half, `top_right` and `bottom_right` stacked over
    /// the right half. Deleting `left` needs both right slots together, and neither right slot can
    /// be absorbed by `left` alone.
    fn three_slot_workspace() -> LogicalSlots {
        let mut slots = LogicalSlots::default();
        slots.set(id("left"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("top_right"), LogicalRect::new(960, 0, 960, 540));
        slots.set(id("bottom_right"), LogicalRect::new(960, 540, 960, 540));
        slots
    }

    fn mover_rects(shift: &SlotShift) -> Vec<(String, LogicalRect)> {
        shift
            .movers
            .iter()
            .map(|mover| (mover.container.to_string(), mover.after))
            .collect()
    }

    fn resized(slots: &LogicalSlots, name: &str) -> LogicalRect {
        slots.get(&id(name)).unwrap()
    }

    #[test]
    fn moving_a_boundary_changes_only_the_axis_it_belongs_to() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let resize = slots
            .plan_edge_resize(&id("a"), OperationDirection::Right, 100)
            .unwrap();

        assert_eq!(resize.before, 960);
        assert_eq!(resize.after, 1060);
        assert_eq!(resize.shift(), 100);

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "a"), LogicalRect::new(0, 0, 1060, 1080));
        assert_eq!(resized(&slots, "b"), LogicalRect::new(1060, 0, 860, 1080));
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_positive_delta_grows_the_container_whichever_side_the_boundary_is_on() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        // `b` grows leftwards, so the boundary moves down the axis rather than up it.
        let resize = slots
            .plan_edge_resize(&id("b"), OperationDirection::Left, 100)
            .unwrap();

        assert_eq!(resize.after, 860);

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "b"), LogicalRect::new(860, 0, 1060, 1080));
        assert_eq!(resized(&slots, "a"), LogicalRect::new(0, 0, 860, 1080));
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn every_container_touching_the_boundary_moves_with_it() {
        let mut slots = three_slot_workspace();

        let resize = slots
            .plan_edge_resize(&id("left"), OperationDirection::Right, 200)
            .unwrap();

        // One container on the near side, two on the far side, and the boundary is the whole
        // shared line rather than one container's edge.
        assert_eq!(resize.movers.len(), 3);

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "left"), LogicalRect::new(0, 0, 1160, 1080));
        assert_eq!(
            resized(&slots, "top_right"),
            LogicalRect::new(1160, 0, 760, 540)
        );
        assert_eq!(
            resized(&slots, "bottom_right"),
            LogicalRect::new(1160, 540, 760, 540)
        );
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_boundary_takes_in_the_containers_which_share_it_on_the_near_side_too() {
        // `top_left` and `bottom_left` stack against a single full-height `right`. Resizing from
        // `top_left` alone would tear the column, so the boundary has to take in `bottom_left`.
        let mut slots = LogicalSlots::default();
        slots.set(id("top_left"), LogicalRect::new(0, 0, 960, 540));
        slots.set(id("bottom_left"), LogicalRect::new(0, 540, 960, 540));
        slots.set(id("right"), LogicalRect::new(960, 0, 960, 1080));

        let resize = slots
            .plan_edge_resize(&id("top_left"), OperationDirection::Right, 160)
            .unwrap();

        assert_eq!(resize.movers.len(), 3);

        slots.apply_edge_resize(&resize);

        assert_eq!(
            resized(&slots, "top_left"),
            LogicalRect::new(0, 0, 1120, 540)
        );
        assert_eq!(
            resized(&slots, "bottom_left"),
            LogicalRect::new(0, 540, 1120, 540)
        );
        assert_eq!(
            resized(&slots, "right"),
            LogicalRect::new(1120, 0, 800, 1080)
        );
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_horizontal_boundary_changes_only_heights() {
        let mut slots = LogicalSlots::default();
        slots.set(id("top"), LogicalRect::new(0, 0, 1920, 540));
        slots.set(id("bottom"), LogicalRect::new(0, 540, 1920, 540));

        let resize = slots
            .plan_edge_resize(&id("top"), OperationDirection::Down, 90)
            .unwrap();

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "top"), LogicalRect::new(0, 0, 1920, 630));
        assert_eq!(
            resized(&slots, "bottom"),
            LogicalRect::new(0, 630, 1920, 450)
        );
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn an_oversized_delta_is_clamped_to_the_minimum_slot_edge() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let resize = slots
            .plan_edge_resize(&id("a"), OperationDirection::Right, 100_000)
            .unwrap();

        // Clamped rather than refused: holding a resize key down settles against the minimum.
        assert_eq!(resize.after, 1920 - MIN_SLOT_EDGE);

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "b").width, MIN_SLOT_EDGE);
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn the_clamp_respects_the_narrowest_container_on_the_boundary() {
        // Two containers of different widths share the far side of the boundary. The narrower one
        // has to keep MIN_SLOT_EDGE, so it - not the wider one, and not the work area - is what
        // stops the move.
        let mut slots = LogicalSlots::default();
        slots.set(id("left"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("top_right"), LogicalRect::new(960, 0, 960, 540));
        slots.set(id("bottom_near"), LogicalRect::new(960, 540, 400, 540));
        slots.set(id("bottom_far"), LogicalRect::new(1360, 540, 560, 540));

        assert!(slots.validate_coverage(area()).is_ok());

        let resize = slots
            .plan_edge_resize(&id("left"), OperationDirection::Right, 100_000)
            .unwrap();

        assert_eq!(resize.shift(), 400 - MIN_SLOT_EDGE);

        slots.apply_edge_resize(&resize);

        assert_eq!(resized(&slots, "bottom_near").width, MIN_SLOT_EDGE);
        assert_eq!(
            resized(&slots, "top_right").width,
            960 - (400 - MIN_SLOT_EDGE)
        );

        // The container which is not on this boundary at all never moved.
        assert_eq!(
            resized(&slots, "bottom_far"),
            LogicalRect::new(1360, 540, 560, 540)
        );
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn the_edge_of_the_work_area_is_not_a_boundary() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        assert!(
            slots
                .plan_edge_resize(&id("a"), OperationDirection::Left, 100)
                .is_none()
        );
        assert!(
            slots
                .plan_edge_resize(&id("a"), OperationDirection::Up, 100)
                .is_none()
        );
    }

    #[test]
    fn a_boundary_with_no_room_left_refuses_instead_of_overlapping() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 1920 - MIN_SLOT_EDGE, 1080));
        slots.set(
            id("b"),
            LogicalRect::new(1920 - MIN_SLOT_EDGE, 0, MIN_SLOT_EDGE, 1080),
        );

        let before = slots.clone();

        assert!(
            slots
                .plan_edge_resize(&id("a"), OperationDirection::Right, 10)
                .is_none()
        );
        assert_eq!(slots.get(&id("a")), before.get(&id("a")));
        assert_eq!(slots.get(&id("b")), before.get(&id("b")));
    }

    #[test]
    fn a_zero_delta_is_not_a_boundary_move() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        assert!(
            slots
                .plan_edge_resize(&id("a"), OperationDirection::Right, 0)
                .is_none()
        );
    }

    #[test]
    fn a_container_with_no_slot_has_no_boundary_to_move() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 1920, 1080));

        assert!(
            slots
                .plan_edge_resize(&id("hidden"), OperationDirection::Right, 100)
                .is_none()
        );
    }

    #[test]
    fn a_boundary_which_does_not_line_up_is_refused() {
        // A slot map with a hole in it. `b` only reaches halfway down the boundary, so moving it
        // would leave the bottom half of `a`'s edge facing nothing. In a valid tiling the boundary
        // sweep always closes; this is the safety net for a map which is not one.
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 540));

        assert!(
            slots
                .plan_edge_resize(&id("a"), OperationDirection::Right, 100)
                .is_none()
        );

        // Filling the hole makes the same boundary movable, which is what pins the refusal on the
        // missing coverage rather than on anything else about the fixture.
        slots.set(id("c"), LogicalRect::new(960, 540, 960, 540));

        let resize = slots
            .plan_edge_resize(&id("a"), OperationDirection::Right, 100)
            .unwrap();

        assert_eq!(resize.movers.len(), 3);

        slots.apply_edge_resize(&resize);
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_boundary_move_advances_the_geometry_generation() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let generation = slots.generation();
        let resize = slots
            .plan_edge_resize(&id("a"), OperationDirection::Right, 40)
            .unwrap();

        slots.apply_edge_resize(&resize);

        assert!(slots.generation() > generation);
    }

    #[test]
    fn a_single_neighbour_covering_the_whole_edge_absorbs_the_slot() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let shift = slots.plan_absorption(&id("b")).unwrap();

        assert!(matches!(shift.direction, OperationDirection::Left));
        assert_eq!(
            mover_rects(&shift),
            vec![("a".to_string(), LogicalRect::new(0, 0, 1920, 1080))]
        );

        slots.apply_absorption(&shift);

        assert!(!slots.contains(&id("b")));
        assert_eq!(slots.get(&id("a")), Some(area()));
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn several_neighbours_on_one_edge_expand_together() {
        let mut slots = three_slot_workspace();

        let shift = slots.plan_absorption(&id("left")).unwrap();

        // Nothing is on the left edge of `left`, so the two right slots are the first complete
        // group, ordered top to bottom because the shared edge is vertical.
        assert!(matches!(shift.direction, OperationDirection::Right));
        assert_eq!(
            mover_rects(&shift),
            vec![
                ("top_right".to_string(), LogicalRect::new(0, 0, 1920, 540)),
                (
                    "bottom_right".to_string(),
                    LogicalRect::new(0, 540, 1920, 540)
                ),
            ]
        );
        assert_eq!(shift.first_mover(), Some(&id("top_right")));

        slots.apply_absorption(&shift);
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_group_which_only_partly_covers_the_edge_is_refused() {
        let slots = three_slot_workspace();

        assert!(
            slots
                .complete_edge_group(
                    LogicalRect::new(0, 0, 960, 1080),
                    OperationDirection::Right,
                    &id("left"),
                )
                .is_some()
        );

        // A shorter target has the same two neighbours, but they now overshoot its edge.
        assert!(
            slots
                .complete_edge_group(
                    LogicalRect::new(0, 0, 960, 700),
                    OperationDirection::Right,
                    &id("left"),
                )
                .is_none()
        );
    }

    #[test]
    fn absorption_tries_left_before_right_before_up_before_down() {
        // A slot with a complete neighbour on every one of its four edges.
        let mut slots = LogicalSlots::default();
        slots.set(id("middle"), LogicalRect::new(100, 100, 100, 100));
        slots.set(id("left"), LogicalRect::new(0, 100, 100, 100));
        slots.set(id("right"), LogicalRect::new(200, 100, 100, 100));
        slots.set(id("up"), LogicalRect::new(100, 0, 100, 100));
        slots.set(id("down"), LogicalRect::new(100, 200, 100, 100));

        let shift = slots.plan_absorption(&id("middle")).unwrap();

        assert!(matches!(shift.direction, OperationDirection::Left));
        assert_eq!(
            mover_rects(&shift),
            vec![("left".to_string(), LogicalRect::new(0, 100, 200, 100))]
        );
    }

    #[test]
    fn an_isolated_slot_cannot_be_absorbed() {
        let mut slots = LogicalSlots::default();
        slots.set(id("only"), area());

        assert!(slots.plan_absorption(&id("only")).is_none());
        assert!(slots.plan_absorption(&id("missing")).is_none());
    }

    #[test]
    fn a_diagonal_neighbour_is_not_an_absorber() {
        let mut slots = LogicalSlots::default();
        slots.set(id("target"), LogicalRect::new(100, 100, 100, 100));
        // Touches only the corner of `target`.
        slots.set(id("corner"), LogicalRect::new(0, 0, 100, 100));

        assert!(slots.plan_absorption(&id("target")).is_none());
    }

    #[test]
    fn an_odd_split_absorbs_without_leaving_a_hole() {
        let odd = LogicalRect::new(0, 0, 1921, 1080);
        let split = odd.split(SplitAxis::LeftRight).unwrap();

        let mut slots = LogicalSlots::default();
        slots.set(id("new"), split.new_slot);
        slots.set(id("existing"), split.existing_slot);

        let shift = slots.plan_absorption(&id("new")).unwrap();
        slots.apply_absorption(&shift);

        assert_eq!(slots.get(&id("existing")), Some(odd));
        assert!(slots.validate_coverage(odd).is_ok());
    }

    #[test]
    fn a_release_puts_every_absorber_back_exactly_where_it_was() {
        let mut slots = three_slot_workspace();
        let before = slots.clone();

        let absorption = slots.plan_absorption(&id("left")).unwrap();
        let recorded = absorption.rects_before();
        slots.apply_absorption(&absorption);

        let release = slots
            .plan_release(
                &id("left"),
                absorption.slot,
                absorption.direction,
                &recorded,
            )
            .unwrap();
        slots.apply_release(&release);

        for container in ["left", "top_right", "bottom_right"] {
            assert_eq!(slots.get(&id(container)), before.get(&id(container)));
        }
        assert!(slots.validate_coverage(area()).is_ok());
    }

    #[test]
    fn a_release_is_refused_once_an_absorber_no_longer_holds_what_it_absorbed() {
        let mut slots = three_slot_workspace();
        let absorption = slots.plan_absorption(&id("left")).unwrap();
        let recorded = absorption.rects_before();
        slots.apply_absorption(&absorption);

        let mut gone = slots.clone();
        gone.remove(&id("top_right"));
        assert!(
            gone.plan_release(
                &id("left"),
                absorption.slot,
                absorption.direction,
                &recorded
            )
            .is_none()
        );

        // A manual resize of the shared boundary makes the exact reverse impossible.
        let mut moved = slots.clone();
        moved.set(id("top_right"), LogicalRect::new(0, 0, 1920, 600));
        moved.set(id("bottom_right"), LogicalRect::new(0, 600, 1920, 480));
        assert!(
            moved
                .plan_release(
                    &id("left"),
                    absorption.slot,
                    absorption.direction,
                    &recorded
                )
                .is_none()
        );

        // The untouched map still releases, so both refusals are about the change and not about
        // the record.
        assert!(
            slots
                .plan_release(
                    &id("left"),
                    absorption.slot,
                    absorption.direction,
                    &recorded
                )
                .is_some()
        );
    }

    #[test]
    fn a_release_is_refused_when_the_container_already_holds_a_slot() {
        let slots = three_slot_workspace();
        let absorption = slots.plan_absorption(&id("left")).unwrap();
        let recorded = absorption.rects_before();

        // The absorption was planned but never applied, so releasing onto `left` would overlap.
        assert!(
            slots
                .plan_release(
                    &id("left"),
                    absorption.slot,
                    absorption.direction,
                    &recorded
                )
                .is_none()
        );
    }

    #[test]
    fn absorbing_and_releasing_both_advance_the_geometry_generation() {
        let mut slots = three_slot_workspace();
        let start = slots.generation();

        let absorption = slots.plan_absorption(&id("left")).unwrap();
        let recorded = absorption.rects_before();
        slots.apply_absorption(&absorption);
        let absorbed = slots.generation();
        assert!(absorbed > start);

        let release = slots
            .plan_release(
                &id("left"),
                absorption.slot,
                absorption.direction,
                &recorded,
            )
            .unwrap();
        slots.apply_release(&release);

        assert!(slots.generation() > absorbed);
    }

    #[test]
    fn a_split_gives_the_new_container_the_left_half_of_a_wide_slot() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), area());

        let split = slots
            .plan_split(&id("donor"), &id("new"), None)
            .expect("a full work area can be halved");

        assert_eq!(split.axis, SplitAxis::LeftRight);
        assert_eq!(split.created_slot, LogicalRect::new(0, 0, 960, 1080));
        assert_eq!(split.donor.after, LogicalRect::new(960, 0, 960, 1080));
    }

    #[test]
    fn a_split_divides_the_longer_edge_of_a_tall_slot() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), LogicalRect::new(0, 0, 600, 1080));

        let split = slots
            .plan_split(&id("donor"), &id("new"), None)
            .expect("a tall slot can be halved");

        assert_eq!(split.axis, SplitAxis::TopBottom);
        // A top/bottom split keeps the donor on top.
        assert_eq!(split.donor.after, LogicalRect::new(0, 0, 600, 540));
        assert_eq!(split.created_slot, LogicalRect::new(0, 540, 600, 540));
    }

    #[test]
    fn a_forced_axis_is_used_instead_of_the_longer_edge() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), area());

        let split = slots
            .plan_split(&id("donor"), &id("new"), Some(SplitAxis::TopBottom))
            .expect("a full work area can be halved either way");

        assert_eq!(split.axis, SplitAxis::TopBottom);
        assert_eq!(split.donor.after, LogicalRect::new(0, 0, 1920, 540));
        assert_eq!(split.created_slot, LogicalRect::new(0, 540, 1920, 540));
    }

    #[test]
    fn an_applied_split_still_tiles_the_work_area() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), LogicalRect::new(0, 0, 1921, 1080));

        let split = slots
            .plan_split(&id("donor"), &id("new"), None)
            .expect("an odd width can still be halved");

        slots.apply_split(&split);

        // The donor keeps the odd remainder pixel, and the halves leave no uncovered column.
        assert_eq!(
            slots.get(&id("new")),
            Some(LogicalRect::new(0, 0, 960, 1080))
        );
        assert_eq!(
            slots.get(&id("donor")),
            Some(LogicalRect::new(960, 0, 961, 1080))
        );
        assert!(
            slots
                .validate_coverage(LogicalRect::new(0, 0, 1921, 1080))
                .is_ok()
        );
    }

    #[test]
    fn planning_a_split_writes_nothing_until_it_is_applied() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), area());

        let generation = slots.generation();
        let split = slots
            .plan_split(&id("donor"), &id("new"), None)
            .expect("a full work area can be halved");

        assert_eq!(slots.get(&id("donor")), Some(area()));
        assert!(!slots.contains(&id("new")));
        assert_eq!(slots.generation(), generation);

        slots.apply_split(&split);

        assert_eq!(slots.len(), 2);
        assert!(slots.generation() > generation);
    }

    #[test]
    fn a_slot_too_small_to_halve_refuses_to_split() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), LogicalRect::new(0, 0, 3, 3));

        assert!(slots.plan_split(&id("donor"), &id("new"), None).is_none());
    }

    #[test]
    fn a_split_is_refused_when_the_donor_or_the_new_container_is_wrong() {
        let mut slots = LogicalSlots::default();
        slots.set(id("donor"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("other"), LogicalRect::new(960, 0, 960, 1080));

        // A donor which holds no slot cannot give half of it away.
        assert!(slots.plan_split(&id("missing"), &id("new"), None).is_none());
        // A container which already holds a slot is not being created.
        assert!(slots.plan_split(&id("donor"), &id("other"), None).is_none());
        assert!(slots.plan_split(&id("donor"), &id("donor"), None).is_none());
    }

    #[test]
    fn a_neighbour_is_chosen_left_before_right_before_up_before_down() {
        let mut slots = LogicalSlots::default();
        slots.set(id("focused"), LogicalRect::new(960, 540, 960, 540));
        slots.set(id("left"), LogicalRect::new(0, 540, 960, 540));
        slots.set(id("up"), LogicalRect::new(960, 0, 960, 540));

        assert_eq!(slots.adjacent_neighbour(&id("focused")), Some(id("left")));

        // With the left neighbour gone the up neighbour is the only remaining candidate.
        slots.remove(&id("left"));
        assert_eq!(slots.adjacent_neighbour(&id("focused")), Some(id("up")));
    }

    #[test]
    fn several_neighbours_on_a_vertical_edge_are_taken_top_to_bottom() {
        let mut slots = LogicalSlots::default();
        slots.set(id("focused"), LogicalRect::new(960, 0, 960, 1080));
        slots.set(id("bottom_left"), LogicalRect::new(0, 540, 960, 540));
        slots.set(id("top_left"), LogicalRect::new(0, 0, 960, 540));

        assert_eq!(
            slots.adjacent_neighbour(&id("focused")),
            Some(id("top_left"))
        );
    }

    #[test]
    fn several_neighbours_on_a_horizontal_edge_are_taken_left_to_right() {
        let mut slots = LogicalSlots::default();
        slots.set(id("focused"), LogicalRect::new(0, 540, 1920, 540));
        slots.set(id("up_right"), LogicalRect::new(960, 0, 960, 540));
        slots.set(id("up_left"), LogicalRect::new(0, 0, 960, 540));

        assert_eq!(
            slots.adjacent_neighbour(&id("focused")),
            Some(id("up_left"))
        );
    }

    #[test]
    fn a_partial_neighbour_is_still_a_recipient() {
        let mut slots = three_slot_workspace();

        // `top_right` covers only half of `left`'s right edge, so it cannot absorb that slot, but
        // it can perfectly well receive a window.
        assert!(
            slots
                .complete_edge_group(
                    slots.get(&id("left")).unwrap(),
                    OperationDirection::Right,
                    &id("left")
                )
                .is_some()
        );
        slots.remove(&id("bottom_right"));
        assert!(
            slots
                .complete_edge_group(
                    slots.get(&id("left")).unwrap(),
                    OperationDirection::Right,
                    &id("left")
                )
                .is_none()
        );
        assert_eq!(slots.adjacent_neighbour(&id("left")), Some(id("top_right")));
    }

    #[test]
    fn a_lone_or_diagonal_slot_has_no_neighbour() {
        let mut slots = LogicalSlots::default();
        slots.set(id("only"), area());

        assert_eq!(slots.adjacent_neighbour(&id("only")), None);
        assert_eq!(slots.adjacent_neighbour(&id("missing")), None);

        // Sharing nothing but a corner is not adjacency.
        let mut diagonal = LogicalSlots::default();
        diagonal.set(id("a"), LogicalRect::new(0, 0, 960, 540));
        diagonal.set(id("b"), LogicalRect::new(960, 540, 960, 540));

        assert_eq!(diagonal.adjacent_neighbour(&id("a")), None);
    }

    #[test]
    fn rect_conversion_round_trips_width_and_height() {
        let slot = LogicalRect::new(10, 20, 300, 400);
        let rect: Rect = slot.into();

        assert_eq!(rect.left, 10);
        assert_eq!(rect.top, 20);
        assert_eq!(rect.right, 300);
        assert_eq!(rect.bottom, 400);
        assert_eq!(LogicalRect::from(rect), slot);
    }

    #[test]
    fn left_right_split_puts_the_new_container_on_the_left() {
        let split = area().split(SplitAxis::LeftRight).unwrap();

        assert_eq!(split.new_slot, LogicalRect::new(0, 0, 960, 1080));
        assert_eq!(split.existing_slot, LogicalRect::new(960, 0, 960, 1080));
    }

    #[test]
    fn top_bottom_split_keeps_the_existing_container_on_top() {
        let split = area().split(SplitAxis::TopBottom).unwrap();

        assert_eq!(split.existing_slot, LogicalRect::new(0, 0, 1920, 540));
        assert_eq!(split.new_slot, LogicalRect::new(0, 540, 1920, 540));
    }

    #[test]
    fn an_odd_remainder_pixel_belongs_to_the_existing_container() {
        let horizontal = LogicalRect::new(0, 0, 1001, 500)
            .split(SplitAxis::LeftRight)
            .unwrap();

        assert_eq!(horizontal.new_slot.width, 500);
        assert_eq!(horizontal.existing_slot.width, 501);

        let vertical = LogicalRect::new(0, 0, 500, 1001)
            .split(SplitAxis::TopBottom)
            .unwrap();

        assert_eq!(vertical.existing_slot.height, 501);
        assert_eq!(vertical.new_slot.height, 500);
    }

    #[test]
    fn a_split_tiles_the_original_slot_without_a_hole_or_an_overlap() {
        for slot in [
            LogicalRect::new(3, 7, 1001, 501),
            LogicalRect::new(0, 0, 640, 640),
            LogicalRect::new(-100, -50, 333, 777),
        ] {
            for axis in [SplitAxis::LeftRight, SplitAxis::TopBottom] {
                let split = slot.split(axis).unwrap();

                assert!(!split.new_slot.overlaps(split.existing_slot));
                assert_eq!(
                    split.new_slot.area() + split.existing_slot.area(),
                    slot.area()
                );
                assert!(slot.contains(split.new_slot));
                assert!(slot.contains(split.existing_slot));
            }
        }
    }

    #[test]
    fn the_longer_edge_is_split_and_a_square_splits_left_to_right() {
        assert_eq!(
            LogicalRect::new(0, 0, 800, 600).longer_edge_axis(),
            SplitAxis::LeftRight
        );
        assert_eq!(
            LogicalRect::new(0, 0, 600, 800).longer_edge_axis(),
            SplitAxis::TopBottom
        );
        assert_eq!(
            LogicalRect::new(0, 0, 700, 700).longer_edge_axis(),
            SplitAxis::LeftRight
        );
    }

    #[test]
    fn a_slot_that_cannot_be_halved_refuses_to_split() {
        assert!(
            LogicalRect::new(0, 0, 3, 100)
                .split(SplitAxis::LeftRight)
                .is_none()
        );
        assert!(
            LogicalRect::new(0, 0, 100, 3)
                .split(SplitAxis::TopBottom)
                .is_none()
        );
        assert!(LogicalRect::new(0, 0, 0, 100).split_longer_edge().is_none());
    }

    #[test]
    fn adjacency_needs_a_shared_edge_and_a_shared_length() {
        let left = LogicalRect::new(0, 0, 960, 1080);
        let right = LogicalRect::new(960, 0, 960, 1080);

        assert!(right.is_neighbour(left, OperationDirection::Left));
        assert!(left.is_neighbour(right, OperationDirection::Right));
        assert!(!left.is_neighbour(right, OperationDirection::Left));
    }

    #[test]
    fn a_gap_or_a_shared_corner_is_not_adjacency() {
        let anchor = LogicalRect::new(0, 0, 100, 100);

        // A one pixel gap between the two edges.
        assert!(!anchor.is_neighbour(
            LogicalRect::new(101, 0, 100, 100),
            OperationDirection::Right
        ));
        // Only the corner point is shared, so the shared edge length is zero.
        assert!(!anchor.is_neighbour(
            LogicalRect::new(100, 100, 100, 100),
            OperationDirection::Right
        ));
    }

    #[test]
    fn gaps_do_not_change_logical_adjacency() {
        // Render rectangles are inset by a gap; the logical slots they came from stay adjacent.
        let left = LogicalRect::new(0, 0, 960, 1080);
        let right = LogicalRect::new(960, 0, 960, 1080);

        let gap = 10;
        let rendered_left = Rect {
            left: left.left + gap,
            top: left.top + gap,
            right: left.width - gap * 2,
            bottom: left.height - gap * 2,
        };

        assert_ne!(rendered_left.left + rendered_left.right, right.left);
        assert!(left.is_neighbour(right, OperationDirection::Right));
    }

    #[test]
    fn moving_one_edge_absorbs_a_freed_slot_exactly() {
        let deleted = LogicalRect::new(960, 0, 960, 1080);
        let survivor = LogicalRect::new(0, 0, 960, 1080);

        let grown = survivor.with_edge_at(OperationDirection::Right, deleted.right());

        assert_eq!(grown, LogicalRect::new(0, 0, 1920, 1080));
        assert_eq!(grown.area(), survivor.area() + deleted.area());
        assert_eq!(grown.height, survivor.height);
    }

    #[test]
    fn moving_a_leading_edge_keeps_the_opposite_edge_fixed() {
        let slot = LogicalRect::new(960, 0, 960, 1080);
        let grown = slot.with_edge_at(OperationDirection::Left, 0);

        assert_eq!(grown, LogicalRect::new(0, 0, 1920, 1080));
        assert_eq!(grown.right(), slot.right());
    }

    #[test]
    fn slots_are_ordered_deterministically_per_direction() {
        let mut entries = vec![
            (id("bottom"), LogicalRect::new(0, 540, 960, 540)),
            (id("right"), LogicalRect::new(960, 0, 960, 1080)),
            (id("top"), LogicalRect::new(0, 0, 960, 540)),
        ];

        sort_slots(
            &mut entries,
            SlotOrder::for_direction(OperationDirection::Left),
        );
        assert_eq!(
            entries
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["top", "right", "bottom"]
        );

        sort_slots(
            &mut entries,
            SlotOrder::for_direction(OperationDirection::Up),
        );
        assert_eq!(
            entries
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["top", "bottom", "right"]
        );
    }

    #[test]
    fn a_full_tiling_passes_coverage_validation() {
        let mut slots = LogicalSlots::default();
        slots.set(id("left-top"), LogicalRect::new(0, 0, 960, 540));
        slots.set(id("left-bottom"), LogicalRect::new(0, 540, 960, 540));
        slots.set(id("right"), LogicalRect::new(960, 0, 960, 1080));

        assert_eq!(slots.validate_coverage(area()), Ok(()));
    }

    #[test]
    fn an_odd_split_leaves_no_uncovered_pixel_column() {
        let work_area = LogicalRect::new(0, 0, 1001, 1080);
        let split = work_area.split(SplitAxis::LeftRight).unwrap();

        let mut slots = LogicalSlots::default();
        slots.set(id("new"), split.new_slot);
        slots.set(id("existing"), split.existing_slot);

        assert_eq!(slots.validate_coverage(work_area), Ok(()));
    }

    #[test]
    fn coverage_validation_reports_an_overlap() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 1000, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let violations = slots.validate_coverage(area()).unwrap_err();

        assert!(violations.iter().any(|violation| matches!(
            violation,
            SlotViolation::Overlap { overlap, .. } if *overlap == LogicalRect::new(960, 0, 40, 1080)
        )));
    }

    #[test]
    fn coverage_validation_reports_a_hole() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 900, 1080));
        slots.set(id("b"), LogicalRect::new(960, 0, 960, 1080));

        let violations = slots.validate_coverage(area()).unwrap_err();

        assert!(
            violations
                .iter()
                .any(|violation| matches!(violation, SlotViolation::IncompleteCoverage { .. }))
        );
    }

    #[test]
    fn coverage_validation_reports_a_slot_outside_the_work_area() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 1920, 1080));
        slots.set(id("stray"), LogicalRect::new(1920, 0, 100, 1080));

        let violations = slots.validate_coverage(area()).unwrap_err();

        assert!(violations.iter().any(|violation| matches!(
            violation,
            SlotViolation::OutsideArea { container, .. } if container == &id("stray")
        )));
    }

    #[test]
    fn a_workspace_without_occupied_slots_is_vacuously_valid() {
        assert_eq!(LogicalSlots::default().validate_coverage(area()), Ok(()));
    }

    #[test]
    fn every_geometry_change_advances_the_generation() {
        let mut slots = LogicalSlots::default();
        let start = slots.generation();

        slots.set(id("a"), LogicalRect::new(0, 0, 100, 100));
        let after_insert = slots.generation();
        assert!(after_insert > start);

        // Writing the same slot again is not a geometry change.
        slots.set(id("a"), LogicalRect::new(0, 0, 100, 100));
        assert_eq!(slots.generation(), after_insert);

        slots.set(id("a"), LogicalRect::new(0, 0, 200, 100));
        assert!(slots.generation() > after_insert);

        let after_update = slots.generation();
        assert!(slots.remove(&id("missing")).is_none());
        assert_eq!(slots.generation(), after_update);

        assert!(slots.remove(&id("a")).is_some());
        assert!(slots.generation() > after_update);
    }

    #[test]
    fn retain_drops_slots_for_containers_that_no_longer_exist() {
        let mut slots = LogicalSlots::default();
        slots.set(id("kept"), LogicalRect::new(0, 0, 960, 1080));
        slots.set(id("gone"), LogicalRect::new(960, 0, 960, 1080));

        slots.retain(|id| id.as_str() == "kept");

        assert_eq!(slots.len(), 1);
        assert!(slots.contains(&id("kept")));
        assert!(!slots.contains(&id("gone")));
    }

    #[test]
    fn logical_slots_survive_a_serde_round_trip() {
        let mut slots = LogicalSlots::default();
        slots.set(id("a"), LogicalRect::new(0, 0, 960, 1080));

        let json = serde_json::to_string(&slots).unwrap();

        assert_eq!(serde_json::from_str::<LogicalSlots>(&json).unwrap(), slots);
        assert_eq!(
            serde_json::from_str::<LogicalSlots>("{}").unwrap(),
            LogicalSlots::default()
        );
    }
}
