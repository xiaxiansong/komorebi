use std::fmt;

use serde::Deserialize;
use serde::Serialize;

use crate::invariants::InvariantViolation;
use crate::managed_window::FloatingRejection;
use crate::window_manager::FloatingOutcome;

/// What a command did, as a value the caller can branch on.
///
/// A command which declines to act is not a failure: "you asked to move a tiled window" is a fact
/// about the window, and "komorebi could not reach the window manager" is a fact about komorebi.
/// Both used to reach a `komorebic` caller as the same silent exit code, so a script could not
/// tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CommandOutcome {
    /// The command changed the model, the desktop, or both.
    Success,
    /// The command was valid, found its target, and had nothing to change.
    NoOp,
    /// The target window is positioned by its container, so it has no rectangle of its own.
    NotFloating,
    /// The target window is minimized, so it has no rectangle on screen.
    Minimized,
    /// There is no window, container, or workspace for the command to act on.
    NoTarget,
    /// The target window is ignored by configuration and never enters the model.
    Ignored,
    /// The target window is in the runtime suspension set, so komorebi does not own it.
    Suspended,
    /// The command was refused because the model would not have been left consistent.
    InvariantViolation,
}

impl CommandOutcome {
    /// The process exit code `komorebic` reports for this outcome.
    ///
    /// Every outcome gets its own code rather than collapsing into success and failure, because
    /// telling them apart is the point of the type. The codes start at 10 to stay clear of the 1
    /// which a transport or parsing failure already uses.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::NoOp => 10,
            Self::NotFloating => 11,
            Self::Minimized => 12,
            Self::NoTarget => 13,
            Self::Ignored => 14,
            Self::Suspended => 15,
            Self::InvariantViolation => 16,
        }
    }

    /// Whether the command changed anything. Only `Success` did.
    pub const fn changed_state(self) -> bool {
        matches!(self, Self::Success)
    }
}

impl fmt::Display for CommandOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            Self::Success => "success",
            Self::NoOp => "no-op",
            Self::NotFloating => "the target window is not floating",
            Self::Minimized => "the target window is minimized",
            Self::NoTarget => "there is no valid target",
            Self::Ignored => "the target window is ignored by configuration",
            Self::Suspended => "the target window is temporarily unmanaged",
            Self::InvariantViolation => "the command would have broken a model invariant",
        };

        f.write_str(description)
    }
}

/// A command outcome and, when there is something to add, one line of detail about it.
///
/// This is the only thing komorebi writes back for a mutating command, and it is written as a
/// single JSON object followed by a newline. A caller which does not read the reply - which is
/// every caller written against an older komorebi - is unaffected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CommandResponse {
    pub outcome: CommandOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CommandResponse {
    pub fn new(outcome: CommandOutcome, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            detail: Some(detail.into()),
        }
    }

    pub const fn bare(outcome: CommandOutcome) -> Self {
        Self {
            outcome,
            detail: None,
        }
    }

    pub const fn success() -> Self {
        Self::bare(CommandOutcome::Success)
    }

    /// The response for a command refused by validation, naming every rule it would have broken.
    pub fn from_violations(violations: &[InvariantViolation]) -> Self {
        if violations.is_empty() {
            return Self::success();
        }

        Self::new(
            CommandOutcome::InvariantViolation,
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// The wire form: one JSON object, newline terminated, so several replies on one connection
    /// stay separable.
    pub fn as_line(&self) -> String {
        let mut line = serde_json::to_string(self)
            .unwrap_or_else(|_| String::from("{\"outcome\":\"Success\"}"));
        line.push('\n');
        line
    }
}

impl From<CommandOutcome> for CommandResponse {
    fn from(outcome: CommandOutcome) -> Self {
        Self::bare(outcome)
    }
}

impl From<FloatingRejection> for CommandResponse {
    /// A presented window is reported as `NotFloating` rather than as an outcome of its own: what
    /// the caller needs to know is that the window is not currently positioned by a floating
    /// rectangle, and the detail says which presentation is responsible.
    fn from(rejection: FloatingRejection) -> Self {
        let outcome = match rejection {
            FloatingRejection::NoSubject | FloatingRejection::UnknownGeometry => {
                CommandOutcome::NoTarget
            }
            FloatingRejection::NotFloating | FloatingRejection::Presented(_) => {
                CommandOutcome::NotFloating
            }
            FloatingRejection::Minimized => CommandOutcome::Minimized,
        };

        Self::new(outcome, rejection.to_string())
    }
}

impl From<FloatingOutcome> for CommandResponse {
    fn from(outcome: FloatingOutcome) -> Self {
        match outcome {
            FloatingOutcome::Applied(rect) => Self::new(
                CommandOutcome::Success,
                format!(
                    "the floating window is now at {},{} {}x{}",
                    rect.left, rect.top, rect.right, rect.bottom
                ),
            ),
            FloatingOutcome::NoOp => Self::new(
                CommandOutcome::NoOp,
                "the floating window is already against its limit",
            ),
            FloatingOutcome::Rejected(rejection) => Self::from(rejection),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Rect;
    use crate::invariants::Invariant;
    use crate::managed_window::Presentation;

    const ALL: [CommandOutcome; 8] = [
        CommandOutcome::Success,
        CommandOutcome::NoOp,
        CommandOutcome::NotFloating,
        CommandOutcome::Minimized,
        CommandOutcome::NoTarget,
        CommandOutcome::Ignored,
        CommandOutcome::Suspended,
        CommandOutcome::InvariantViolation,
    ];

    #[test]
    fn every_outcome_has_its_own_exit_code() {
        let mut codes: Vec<i32> = ALL.iter().map(|outcome| outcome.exit_code()).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();

        assert_eq!(codes.len(), count);
        assert_eq!(CommandOutcome::Success.exit_code(), 0);
        assert!(
            ALL.iter()
                .filter(|outcome| **outcome != CommandOutcome::Success)
                .all(|outcome| outcome.exit_code() > 1)
        );
    }

    #[test]
    fn only_success_reports_a_state_change() {
        for outcome in ALL {
            assert_eq!(
                outcome.changed_state(),
                outcome == CommandOutcome::Success,
                "{outcome} reported the wrong state change"
            );
        }
    }

    #[test]
    fn a_response_survives_the_wire_form() {
        for outcome in ALL {
            let response = CommandResponse::new(outcome, "detail");
            let line = response.as_line();

            assert!(line.ends_with('\n'));
            assert_eq!(
                serde_json::from_str::<CommandResponse>(line.trim_end()).unwrap(),
                response
            );
        }
    }

    #[test]
    fn a_bare_response_omits_the_detail_field() {
        let line = CommandResponse::success().as_line();

        assert!(!line.contains("detail"), "{line}");
        assert_eq!(
            serde_json::from_str::<CommandResponse>(line.trim_end()).unwrap(),
            CommandResponse::bare(CommandOutcome::Success)
        );
    }

    #[test]
    fn a_rejection_keeps_the_reason_it_gave() {
        let cases = [
            (FloatingRejection::NoSubject, CommandOutcome::NoTarget),
            (FloatingRejection::UnknownGeometry, CommandOutcome::NoTarget),
            (FloatingRejection::NotFloating, CommandOutcome::NotFloating),
            (
                FloatingRejection::Presented(Presentation::Maximized),
                CommandOutcome::NotFloating,
            ),
            (FloatingRejection::Minimized, CommandOutcome::Minimized),
        ];

        for (rejection, expected) in cases {
            let response = CommandResponse::from(rejection);

            assert_eq!(response.outcome, expected);
            assert_eq!(response.detail.unwrap(), rejection.to_string());
        }
    }

    #[test]
    fn an_applied_floating_change_reports_where_the_window_went() {
        let rect = Rect {
            left: 10,
            top: 20,
            right: 300,
            bottom: 400,
        };

        let response = CommandResponse::from(FloatingOutcome::Applied(rect));

        assert_eq!(response.outcome, CommandOutcome::Success);
        assert_eq!(
            response.detail.unwrap(),
            "the floating window is now at 10,20 300x400"
        );
    }

    #[test]
    fn a_floating_no_op_is_not_a_success() {
        assert_eq!(
            CommandResponse::from(FloatingOutcome::NoOp).outcome,
            CommandOutcome::NoOp
        );
    }

    #[test]
    fn violations_are_reported_together() {
        let violations = vec![
            InvariantViolation {
                invariant: Invariant::WindowOwnership,
                detail: String::from("first"),
            },
            InvariantViolation {
                invariant: Invariant::NonEmptyContainer,
                detail: String::from("second"),
            },
        ];

        let response = CommandResponse::from_violations(&violations);

        assert_eq!(response.outcome, CommandOutcome::InvariantViolation);
        let detail = response.detail.unwrap();
        assert!(detail.contains("first"), "{detail}");
        assert!(detail.contains("second"), "{detail}");
    }

    #[test]
    fn no_violations_is_a_success() {
        assert_eq!(
            CommandResponse::from_violations(&[]),
            CommandResponse::success()
        );
    }
}
