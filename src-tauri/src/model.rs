use serde::{Deserialize, Serialize};

pub const SHORT_WINDOW_MINUTES: u32 = 300;
pub const WEEKLY_WINDOW_MINUTES: u32 = 10_080;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Codex,
    Claude,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowSnapshot {
    pub duration_minutes: u32,
    pub used_percent: f64,
    pub resets_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderSnapshot {
    pub provider: ProviderId,
    pub observed_at: i64,
    pub short_window: WindowSnapshot,
    pub weekly_window: WindowSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("invalid observation time")]
    InvalidObservedAt,
    #[error("unexpected window duration")]
    WrongDuration,
    #[error("invalid percentage")]
    InvalidPercent,
    #[error("reset has expired")]
    ExpiredReset,
}

impl ProviderSnapshot {
    pub fn validate(&self, now: i64) -> Result<(), ValidationError> {
        if self.observed_at <= 0 {
            return Err(ValidationError::InvalidObservedAt);
        }

        for (window, expected) in [
            (&self.short_window, SHORT_WINDOW_MINUTES),
            (&self.weekly_window, WEEKLY_WINDOW_MINUTES),
        ] {
            if window.duration_minutes != expected {
                return Err(ValidationError::WrongDuration);
            }
            if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) {
                return Err(ValidationError::InvalidPercent);
            }
            if window.resets_at <= now {
                return Err(ValidationError::ExpiredReset);
            }
        }

        Ok(())
    }

    pub fn is_current(&self, now: i64) -> bool {
        self.validate(now).is_ok()
    }
}

pub fn remaining_percent(used_percent: f64) -> u8 {
    (100.0 - used_percent).clamp(0.0, 100.0).round() as u8
}
