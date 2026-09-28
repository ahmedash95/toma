use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Queued,
    Reading,
    Working,
    WaitingForInput,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl WorkStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn transition_to(self, next: Self) -> Result<Self, InvalidStatusTransition> {
        let allowed = matches!(
            (self, next),
            (
                Self::Queued,
                Self::Reading | Self::Working | Self::Cancelled | Self::Failed
            ) | (
                Self::Reading,
                Self::Working
                    | Self::WaitingForInput
                    | Self::Blocked
                    | Self::Cancelled
                    | Self::Failed
            ) | (
                Self::Working,
                Self::WaitingForInput
                    | Self::Blocked
                    | Self::Completed
                    | Self::Cancelled
                    | Self::Failed
            ) | (
                Self::WaitingForInput,
                Self::Reading | Self::Working | Self::Blocked | Self::Cancelled | Self::Failed
            ) | (
                Self::Blocked,
                Self::Reading | Self::Working | Self::Cancelled | Self::Failed
            )
        ) || self == next;

        allowed.then_some(next).ok_or(InvalidStatusTransition {
            from: self,
            to: next,
        })
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("cannot transition work from {from:?} to {to:?}")]
pub struct InvalidStatusTransition {
    pub from: WorkStatus,
    pub to: WorkStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_workflow_is_valid() {
        let status = WorkStatus::Queued
            .transition_to(WorkStatus::Reading)
            .unwrap()
            .transition_to(WorkStatus::Working)
            .unwrap()
            .transition_to(WorkStatus::WaitingForInput)
            .unwrap()
            .transition_to(WorkStatus::Working)
            .unwrap()
            .transition_to(WorkStatus::Completed)
            .unwrap();
        assert_eq!(status, WorkStatus::Completed);
    }

    #[test]
    fn terminal_work_does_not_restart_implicitly() {
        assert!(
            WorkStatus::Completed
                .transition_to(WorkStatus::Working)
                .is_err()
        );
    }
}
