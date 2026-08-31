use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    NoFiles,
    NoExactLimits,
    ExpiredSnapshot,
    SourceUnreadable,
    OversizedRecord,
    MalformedRecord,
    InvalidSchema,
    AmbiguousWindow,
    WatcherUnavailable,
    WatcherOverflow,
    CorruptState,
    StateWriteFailed,
    ClaudeDisabled,
    ClaudeInputInvalid,
    ClaudeInputOversized,
    SettingsMissing,
    SettingsInvalid,
    SettingsConflict,
    SettingsChangedDuringUpdate,
    SettingsWriteFailed,
    StartupUnavailable,
    StartupNeedsRepair,
    StartupWriteFailed,
}
