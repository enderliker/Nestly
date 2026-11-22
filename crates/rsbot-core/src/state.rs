use std::fmt;

use serde::{Deserialize, Serialize};

/// Lifecycle state of a supervised service or module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleState {
    Starting,
    Ready,
    Degraded,
    Restarting,
    Failed,
    Disabled,
    Stopped,
}

impl ModuleState {
    /// Whether interactions may be routed to a service in this state.
    #[must_use]
    pub const fn is_routable(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Restarting => "restarting",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Stopped => "stopped",
        }
    }
}

impl fmt::Display for ModuleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an interaction could not be completed by its owning module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    Starting,
    Restarting,
    Failed,
    Disabled,
    Timeout,
    Crashed,
    Stopped,
}

impl FallbackReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Restarting => "restarting",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Timeout => "timeout",
            Self::Crashed => "crashed",
            Self::Stopped => "stopped",
        }
    }

    #[must_use]
    pub fn user_message(self, module: &str) -> String {
        let message = match self {
            Self::Starting => "This module is starting. Please try again in a moment.",
            Self::Restarting => "This module is restarting. Please try again shortly.",
            Self::Failed => {
                "This module is temporarily unavailable. The operators have been notified."
            }
            Self::Disabled => "This module is currently disabled.",
            Self::Timeout => "This module did not respond in time. Please try again.",
            Self::Crashed => "This module stopped unexpectedly. Please try again.",
            Self::Stopped => "This module is temporarily unavailable.",
        };
        format!("{message} ({module})")
    }
}

#[cfg(test)]
mod tests {
    use super::{FallbackReason, ModuleState};

    #[test]
    fn routable_states() {
        assert!(ModuleState::Ready.is_routable());
        assert!(ModuleState::Degraded.is_routable());
        assert!(!ModuleState::Restarting.is_routable());
        assert!(!ModuleState::Failed.is_routable());
    }

    #[test]
    fn reasons_have_messages() {
        for reason in [
            FallbackReason::Starting,
            FallbackReason::Restarting,
            FallbackReason::Failed,
            FallbackReason::Disabled,
            FallbackReason::Timeout,
            FallbackReason::Crashed,
            FallbackReason::Stopped,
        ] {
            assert_ne!(reason.as_str(), "");
        }
    }
}
