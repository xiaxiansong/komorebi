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
