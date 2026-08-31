//! Geometry for a window its container does not position.
//!
//! A floating window's rectangle is its own. Nothing here reads or writes a logical slot, so a
//! floating move or resize cannot change the arrangement, and the arithmetic is deliberately
//! separate from [`crate::geometry`]: that module tiles an area with rectangles which must not
//! overlap, this one moves one rectangle which is allowed to overlap anything.
//!
//! Everything in this module is pure. Deciding which window may be acted on, applying the result
//! through Win32 and recording what Win32 accepted all happen at the call site.

use crate::core::OperationDirection;
use crate::core::Rect;
use crate::core::Sizing;

/// The extent of a window which is kept inside the work area when it cannot fit inside it.
///
/// This is a strip large enough to hold a title bar or the draggable edge of a borderless
/// window, so a window can always be brought back with the mouse.
pub const MIN_VISIBLE_EXTENT: i32 = 24;

/// The smallest rectangle a floating resize will produce.
///
/// Applications refuse sizes below their own minimum and Win32 reports what they settled on, so
/// this is a floor which keeps the window addressable rather than a claim about what the
/// application will accept.
pub const MIN_FLOATING_EXTENT: i32 = 64;

/// The area a floating window is kept reachable within, and how much of it has to stay inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingBounds {
    pub area: Rect,
    pub min_visible: i32,
}

impl FloatingBounds {
    #[must_use]
    pub const fn new(area: Rect) -> Self {
        Self {
            area,
            min_visible: MIN_VISIBLE_EXTENT,
        }
    }
}

/// The smallest width and height a floating resize may produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingLimits {
    pub min_width: i32,
    pub min_height: i32,
}

impl Default for FloatingLimits {
    fn default() -> Self {
        Self {
            min_width: MIN_FLOATING_EXTENT,
            min_height: MIN_FLOATING_EXTENT,
        }
    }
}

/// Convert a configured delta in logical units into physical pixels for one monitor.
///
/// A delta is configured once and used on every monitor, so it is a logical quantity: 50 means
/// the same apparent distance at 100% and at 150%, which is what stops a window from crawling on
/// a scaled display. A configured delta never scales to nothing, because a key which moves a
/// window by zero pixels reads as a broken binding rather than as a small step.
#[must_use]
pub fn scale_delta(delta: i32, dpi_scale: f32) -> i32 {
    if !dpi_scale.is_finite() || dpi_scale <= 0.0 {
        return delta;
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let scaled = (delta as f32 * dpi_scale).round() as i32;

    if delta != 0 && scaled == 0 {
        delta.signum()
    } else {
        scaled
    }
}

/// Move a floating window by `delta` along one axis, changing nothing but its position.
///
/// The returned rectangle always has the same width and height as `rect`. A delta which would
/// carry the window out of reach settles against the clamp instead of being refused, so holding
/// a movement key down comes to rest at the edge.
#[must_use]
pub fn plan_move(
    rect: Rect,
    direction: OperationDirection,
    delta: i32,
    bounds: FloatingBounds,
) -> Rect {
    let mut moved = rect;

    match direction {
        OperationDirection::Left => moved.left -= delta,
        OperationDirection::Right => moved.left += delta,
        OperationDirection::Up => moved.top -= delta,
        OperationDirection::Down => moved.top += delta,
    }

    clamp_position(moved, bounds)
}

/// Keep a floating window reachable inside `bounds` without changing its size.
///
/// The horizontal rule and the vertical rule differ on purpose. A window may hang off the left or
/// the right of the work area once it is too wide to fit, because either side still leaves a
/// grabbable strip. Its top edge may never rise above the work area, because the title bar is
/// there: a window pushed above the top cannot be dragged back with the mouse.
#[must_use]
pub fn clamp_position(rect: Rect, bounds: FloatingBounds) -> Rect {
    let area = bounds.area;
    let mut clamped = rect;

    clamped.left = clamp_axis(
        rect.left,
        rect.right,
        area.left,
        area.right,
        bounds.min_visible,
        false,
    );

    clamped.top = clamp_axis(
        rect.top,
        rect.bottom,
        area.top,
        area.bottom,
        bounds.min_visible,
        true,
    );

    clamped
}

/// Keep a floating window reachable inside `bounds` without insisting that it fits inside it.
///
/// This is the rule for a rectangle the user chose with the mouse. A window dropped half off an
/// edge is left exactly where it was dropped, because moving it back is the jump the user did not
/// ask for; only a window which has been carried out of reach is pulled back, and only far enough
/// to leave `min_visible` of it inside the area.
///
/// [`clamp_position`] is the stricter rule the movement commands use: it contains a window which
/// fits, and falls back to this one when it does not.
#[must_use]
pub fn clamp_reachable(rect: Rect, bounds: FloatingBounds) -> Rect {
    let area = bounds.area;
    let mut clamped = rect;

    clamped.left = clamp_reachable_axis(
        rect.left,
        rect.right,
        area.left,
        area.right,
        bounds.min_visible,
        false,
    );

    clamped.top = clamp_reachable_axis(
        rect.top,
        rect.bottom,
        area.top,
        area.bottom,
        bounds.min_visible,
        true,
    );

    clamped
}

/// Clamp one axis of a window position.
///
/// When the window fits, it is contained. When it does not, the reachability rule below applies
/// instead.
fn clamp_axis(
    position: i32,
    size: i32,
    area_start: i32,
    area_extent: i32,
    min_visible: i32,
    keep_start_inside: bool,
) -> i32 {
    let contained_upper = area_start + area_extent - size;

    if contained_upper >= area_start {
        return position.clamp(area_start, contained_upper);
    }

    clamp_reachable_axis(
        position,
        size,
        area_start,
        area_extent,
        min_visible,
        keep_start_inside,
    )
}

/// Clamp one axis so that `min_visible` of the window stays inside the area.
///
/// `keep_start_inside` decides whether the near edge is pinned to the area or whether the window
/// may hang off either side. It is set for the vertical axis, because the title bar is at the top:
/// a window pushed above the top of the work area cannot be dragged back with the mouse.
fn clamp_reachable_axis(
    position: i32,
    size: i32,
    area_start: i32,
    area_extent: i32,
    min_visible: i32,
    keep_start_inside: bool,
) -> i32 {
    let visible = min_visible.min(size).min(area_extent);
    let lower = if keep_start_inside {
        area_start
    } else {
        area_start + visible - size
    };
    let upper = (area_start + area_extent - visible).max(lower);

    position.clamp(lower, upper)
}

/// Move one edge of a floating window, leaving the opposite edge where it is.
///
/// `direction` names the edge which moves and `sizing` says whether that edge moves outwards or
/// inwards, so `left`/`increase` grows the window to the left while `right`/`increase` grows it
/// to the right. The opposite edge is the fixed point of the operation in every case, which is
/// what makes this an edge resize rather than a resize plus a move.
#[must_use]
pub fn plan_edge_resize(
    rect: Rect,
    direction: OperationDirection,
    sizing: Sizing,
    delta: i32,
    limits: FloatingLimits,
) -> Rect {
    let signed = match sizing {
        Sizing::Increase => delta,
        Sizing::Decrease => -delta,
    };

    let mut resized = rect;

    match direction {
        OperationDirection::Left | OperationDirection::Right => {
            let minimum = limits.min_width.max(1);
            let width = (rect.right + signed).max(minimum);

            resized.right = width;

            if matches!(direction, OperationDirection::Left) {
                // The right edge is the fixed point, so the left edge is derived from it rather
                // than adjusted by the delta: a clamped width then moves the left edge by less
                // than the delta instead of tearing the window off its right edge.
                resized.left = rect.left + rect.right - width;
            }
        }
        OperationDirection::Up | OperationDirection::Down => {
            let minimum = limits.min_height.max(1);
            let height = (rect.bottom + signed).max(minimum);

            resized.bottom = height;

            if matches!(direction, OperationDirection::Up) {
                resized.top = rect.top + rect.bottom - height;
            }
        }
    }

    resized
}

/// Carry a floating rectangle from one work area to another.
///
/// A floating rectangle is the only rectangle in the model which no arrangement will ever correct,
/// so it is the only one which has to survive a monitor transfer by being rewritten. Slots are
/// recalculated on arrival and manual resize dimensions are discarded, but a floating window keeps
/// exactly the rectangle it is given.
///
/// Position and size are both scaled by the ratio between the areas, so a window occupying the
/// middle third of a 1920-wide work area occupies the middle third of a 2560-wide one, and it does
/// not shrink to a quarter of the screen when it lands on a display with twice the pixels. The
/// result is then clamped into the target, which is what makes the window reachable on arrival
/// even when the target is smaller than the source.
///
/// A transfer between identical areas is the identity, and a degenerate source area - one with no
/// width or no height, which a disconnected monitor can briefly report - leaves the size alone and
/// only clamps, because there is no meaningful ratio to scale by.
#[must_use]
pub fn transfer_between_areas(rect: Rect, from: Rect, to: Rect) -> Rect {
    if from == to {
        return rect;
    }

    let scale = |value: i32, numerator: i32, denominator: i32| -> i32 {
        if denominator == 0 {
            return value;
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        {
            ((value as f32) * (numerator as f32) / (denominator as f32)).round() as i32
        }
    };

    let transferred = Rect {
        left: to.left + scale(rect.left - from.left, to.right, from.right),
        top: to.top + scale(rect.top - from.top, to.bottom, from.bottom),
        right: scale(rect.right, to.right, from.right).max(1),
        bottom: scale(rect.bottom, to.bottom, from.bottom).max(1),
    };

    clamp_position(transferred, FloatingBounds::new(to))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    fn bounds() -> FloatingBounds {
        FloatingBounds::new(AREA)
    }

    fn rect(left: i32, top: i32, width: i32, height: i32) -> Rect {
        Rect {
            left,
            top,
            right: width,
            bottom: height,
        }
    }

    #[test]
    fn a_dropped_window_which_hangs_off_an_edge_stays_where_it_was_dropped() {
        // What the mouse chose is kept: the strict rule would pull each of these back, which is
        // the jump a drag must not produce.
        for dropped in [
            rect(-200, 100, 800, 600),
            rect(1700, 100, 800, 600),
            rect(400, 800, 800, 600),
        ] {
            assert_eq!(clamp_reachable(dropped, bounds()), dropped);
            assert_ne!(clamp_position(dropped, bounds()), dropped);
        }
    }

    #[test]
    fn a_dropped_window_carried_out_of_reach_is_pulled_back_to_its_visible_strip() {
        let below = clamp_reachable(rect(400, 2000, 800, 600), bounds());
        assert_eq!(below.top, AREA.bottom - MIN_VISIBLE_EXTENT);
        assert_eq!((below.left, below.right, below.bottom), (400, 800, 600));

        let above = clamp_reachable(rect(400, -900, 800, 600), bounds());
        assert_eq!(above.top, AREA.top);

        let left = clamp_reachable(rect(-1000, 100, 800, 600), bounds());
        assert_eq!(left.left, MIN_VISIBLE_EXTENT - 800);

        let right = clamp_reachable(rect(3000, 100, 800, 600), bounds());
        assert_eq!(right.left, AREA.right - MIN_VISIBLE_EXTENT);
    }

    #[test]
    fn a_dropped_window_larger_than_the_work_area_is_left_alone_while_it_is_reachable() {
        let oversized = rect(-100, 0, 2200, 1200);

        assert_eq!(clamp_reachable(oversized, bounds()), oversized);
    }

    #[test]
    fn the_strict_rule_falls_back_to_the_reachable_one_when_the_window_cannot_fit() {
        let oversized = rect(-3000, 2000, 2200, 1200);

        assert_eq!(
            clamp_position(oversized, bounds()),
            clamp_reachable(oversized, bounds())
        );
    }

    #[test]
    fn a_move_changes_position_only() {
        let start = rect(400, 300, 800, 600);

        for direction in [
            OperationDirection::Left,
            OperationDirection::Right,
            OperationDirection::Up,
            OperationDirection::Down,
        ] {
            let moved = plan_move(start, direction, 50, bounds());

            assert_eq!(moved.right, start.right);
            assert_eq!(moved.bottom, start.bottom);
        }

        assert_eq!(
            plan_move(start, OperationDirection::Left, 50, bounds()),
            rect(350, 300, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Right, 50, bounds()),
            rect(450, 300, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Up, 50, bounds()),
            rect(400, 250, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Down, 50, bounds()),
            rect(400, 350, 800, 600)
        );
    }

    #[test]
    fn a_move_settles_against_each_edge_of_the_work_area() {
        let start = rect(10, 10, 800, 600);

        assert_eq!(
            plan_move(start, OperationDirection::Left, 500, bounds()),
            rect(0, 10, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Up, 500, bounds()),
            rect(10, 0, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Right, 5000, bounds()),
            rect(1120, 10, 800, 600)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Down, 5000, bounds()),
            rect(10, 440, 800, 600)
        );
    }

    #[test]
    fn a_move_respects_a_work_area_which_does_not_start_at_the_origin() {
        let offset = FloatingBounds::new(rect(1920, 40, 1280, 1000));
        let start = rect(2000, 100, 400, 300);

        assert_eq!(
            plan_move(start, OperationDirection::Left, 500, offset),
            rect(1920, 100, 400, 300)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Up, 500, offset),
            rect(2000, 40, 400, 300)
        );
        assert_eq!(
            plan_move(start, OperationDirection::Right, 5000, offset),
            rect(2800, 100, 400, 300)
        );
    }

    #[test]
    fn an_oversized_window_keeps_a_grabbable_strip_and_its_title_bar() {
        let wide = rect(-100, 10, 2400, 600);
        let moved = plan_move(wide, OperationDirection::Left, 5000, bounds());

        assert_eq!(moved.left, MIN_VISIBLE_EXTENT - 2400);
        assert_eq!(moved.right, 2400);

        let moved = plan_move(wide, OperationDirection::Right, 5000, bounds());
        assert_eq!(moved.left, 1920 - MIN_VISIBLE_EXTENT);

        // A window taller than the work area may still be pushed down, but never up past the top
        // edge, because that is where the title bar it has to be dragged by lives.
        let tall = rect(10, -50, 400, 1400);
        assert_eq!(
            plan_move(tall, OperationDirection::Up, 500, bounds()).top,
            0
        );
        assert_eq!(
            plan_move(tall, OperationDirection::Down, 5000, bounds()).top,
            1040 - MIN_VISIBLE_EXTENT
        );
    }

    #[test]
    fn clamping_is_idempotent_and_leaves_a_contained_window_alone() {
        let contained = rect(400, 300, 800, 600);
        assert_eq!(clamp_position(contained, bounds()), contained);

        let escaped = rect(-400, -300, 800, 600);
        let once = clamp_position(escaped, bounds());
        assert_eq!(once, rect(0, 0, 800, 600));
        assert_eq!(clamp_position(once, bounds()), once);
    }

    #[test]
    fn each_resize_edge_moves_only_itself() {
        let start = rect(400, 300, 800, 600);
        let limits = FloatingLimits::default();

        // The named edge moves outwards on increase and inwards on decrease; the opposite edge
        // never moves, which for a left or top edge means the position changes with the size.
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Left,
                Sizing::Increase,
                50,
                limits
            ),
            rect(350, 300, 850, 600)
        );
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Left,
                Sizing::Decrease,
                50,
                limits
            ),
            rect(450, 300, 750, 600)
        );
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Right,
                Sizing::Increase,
                50,
                limits
            ),
            rect(400, 300, 850, 600)
        );
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Right,
                Sizing::Decrease,
                50,
                limits
            ),
            rect(400, 300, 750, 600)
        );
        assert_eq!(
            plan_edge_resize(start, OperationDirection::Up, Sizing::Increase, 50, limits),
            rect(400, 250, 800, 650)
        );
        assert_eq!(
            plan_edge_resize(start, OperationDirection::Up, Sizing::Decrease, 50, limits),
            rect(400, 350, 800, 550)
        );
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Down,
                Sizing::Increase,
                50,
                limits
            ),
            rect(400, 300, 800, 650)
        );
        assert_eq!(
            plan_edge_resize(
                start,
                OperationDirection::Down,
                Sizing::Decrease,
                50,
                limits
            ),
            rect(400, 300, 800, 550)
        );
    }

    #[test]
    fn opposite_edges_stay_fixed_through_a_resize() {
        let start = rect(400, 300, 800, 600);
        let limits = FloatingLimits::default();

        for (direction, sizing) in [
            (OperationDirection::Left, Sizing::Increase),
            (OperationDirection::Left, Sizing::Decrease),
            (OperationDirection::Right, Sizing::Increase),
            (OperationDirection::Right, Sizing::Decrease),
        ] {
            let resized = plan_edge_resize(start, direction, sizing, 50, limits);
            assert_eq!(resized.top, start.top);
            assert_eq!(resized.bottom, start.bottom);

            match direction {
                OperationDirection::Left => {
                    assert_eq!(resized.left + resized.right, start.left + start.right);
                }
                _ => assert_eq!(resized.left, start.left),
            }
        }

        for (direction, sizing) in [
            (OperationDirection::Up, Sizing::Increase),
            (OperationDirection::Up, Sizing::Decrease),
            (OperationDirection::Down, Sizing::Increase),
            (OperationDirection::Down, Sizing::Decrease),
        ] {
            let resized = plan_edge_resize(start, direction, sizing, 50, limits);
            assert_eq!(resized.left, start.left);
            assert_eq!(resized.right, start.right);

            match direction {
                OperationDirection::Up => {
                    assert_eq!(resized.top + resized.bottom, start.top + start.bottom);
                }
                _ => assert_eq!(resized.top, start.top),
            }
        }
    }

    #[test]
    fn a_resize_settles_on_the_minimum_size_without_moving_the_fixed_edge() {
        let start = rect(400, 300, 200, 200);
        let limits = FloatingLimits {
            min_width: 120,
            min_height: 150,
        };

        let narrow = plan_edge_resize(
            start,
            OperationDirection::Left,
            Sizing::Decrease,
            500,
            limits,
        );
        assert_eq!(narrow.right, 120);
        assert_eq!(narrow.left + narrow.right, start.left + start.right);

        let narrow = plan_edge_resize(
            start,
            OperationDirection::Right,
            Sizing::Decrease,
            500,
            limits,
        );
        assert_eq!(narrow, rect(400, 300, 120, 200));

        let short = plan_edge_resize(start, OperationDirection::Up, Sizing::Decrease, 500, limits);
        assert_eq!(short.bottom, 150);
        assert_eq!(short.top + short.bottom, start.top + start.bottom);

        let short = plan_edge_resize(
            start,
            OperationDirection::Down,
            Sizing::Decrease,
            500,
            limits,
        );
        assert_eq!(short, rect(400, 300, 200, 150));
    }

    #[test]
    fn a_delta_is_scaled_by_the_monitor_factor() {
        assert_eq!(scale_delta(50, 1.0), 50);
        assert_eq!(scale_delta(50, 1.5), 75);
        assert_eq!(scale_delta(50, 2.0), 100);
        assert_eq!(scale_delta(30, 1.25), 38);
    }

    #[test]
    fn a_configured_delta_never_scales_away() {
        assert_eq!(scale_delta(1, 0.25), 1);
        assert_eq!(scale_delta(0, 2.0), 0);
    }

    #[test]
    fn an_unusable_scale_factor_leaves_the_delta_alone() {
        assert_eq!(scale_delta(50, 0.0), 50);
        assert_eq!(scale_delta(50, -1.0), 50);
        assert_eq!(scale_delta(50, f32::NAN), 50);
    }

    #[test]
    fn a_scaled_move_covers_the_same_apparent_distance_on_a_scaled_monitor() {
        let start = rect(400, 300, 800, 600);

        let unscaled = plan_move(
            start,
            OperationDirection::Right,
            scale_delta(50, 1.0),
            bounds(),
        );
        let scaled = plan_move(
            start,
            OperationDirection::Right,
            scale_delta(50, 1.5),
            bounds(),
        );

        assert_eq!(unscaled.left - start.left, 50);
        assert_eq!(scaled.left - start.left, 75);
    }

    const SECOND_AREA: Rect = Rect {
        left: 1920,
        top: 0,
        right: 2560,
        bottom: 1400,
    };

    #[test]
    fn a_transfer_between_identical_areas_changes_nothing() {
        let start = rect(400, 300, 800, 600);

        assert_eq!(transfer_between_areas(start, AREA, AREA), start);
    }

    #[test]
    fn a_transfer_keeps_the_window_in_the_same_relative_place() {
        // The window occupies the middle third of the source area horizontally.
        let start = rect(640, 0, 640, 520);

        let transferred = transfer_between_areas(start, AREA, SECOND_AREA);

        // And the middle third of the target, in the target's own coordinates.
        assert_eq!(transferred.left, SECOND_AREA.left + 853);
        assert_eq!(transferred.right, 853);
        // Half the height of the source becomes half the height of the target.
        assert_eq!(transferred.top, SECOND_AREA.top);
        assert_eq!(transferred.bottom, 700);
    }

    #[test]
    fn a_transfer_onto_a_smaller_area_keeps_the_window_reachable() {
        let small = rect(0, 0, 800, 600);
        let start = rect(1600, 900, 300, 200);

        let transferred = transfer_between_areas(start, AREA, small);

        assert!(transferred.left >= small.left);
        assert!(transferred.top >= small.top);
        assert!(transferred.left + transferred.right <= small.left + small.right);
        assert!(transferred.top + transferred.bottom <= small.top + small.bottom);
    }

    #[test]
    fn a_transfer_clamps_a_window_which_cannot_fit_the_target() {
        let narrow = Rect {
            left: 0,
            top: 0,
            right: 200,
            bottom: 200,
        };

        // A window wider than the target after scaling still leaves a grabbable strip inside it.
        let start = rect(0, 0, 1920, 1040);
        let transferred = transfer_between_areas(start, AREA, narrow);

        assert!(transferred.left <= narrow.left + narrow.right - MIN_VISIBLE_EXTENT);
        assert!(transferred.left + transferred.right >= narrow.left + MIN_VISIBLE_EXTENT);
        // The top edge may never rise above the work area, because the title bar is there.
        assert!(transferred.top >= narrow.top);
    }

    #[test]
    fn a_transfer_from_a_degenerate_area_only_clamps() {
        let degenerate = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };

        let start = rect(100, 100, 400, 300);
        let transferred = transfer_between_areas(start, degenerate, AREA);

        assert_eq!(transferred.right, start.right);
        assert_eq!(transferred.bottom, start.bottom);
    }

    #[test]
    fn a_transferred_window_never_collapses_to_nothing() {
        let tiny = Rect {
            left: 0,
            top: 0,
            right: 4,
            bottom: 4,
        };

        let start = rect(0, 0, 100, 100);
        let transferred = transfer_between_areas(start, AREA, tiny);

        assert!(transferred.right >= 1);
        assert!(transferred.bottom >= 1);
    }
}
