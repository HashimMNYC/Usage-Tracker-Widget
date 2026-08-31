use std::{ffi::OsStr, io::Read};

use serde_json::Value;

use crate::{
    model::{
        remaining_percent, ProviderId, ProviderSnapshot, ValidationError, WindowSnapshot,
        SHORT_WINDOW_MINUTES, WEEKLY_WINDOW_MINUTES,
    },
    state_store::{StateMutation, StateStore},
};

pub const MAX_CLAUDE_STDIN_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("capture input is oversized")]
    Oversized,
    #[error("capture input is invalid")]
    Invalid,
    #[error("capture input has no complete exact limits")]
    MissingLimits,
    #[error("capture input contains expired limits")]
    Expired,
}

pub fn parse_claude_statusline(bytes: &[u8], now: i64) -> Result<ProviderSnapshot, CaptureError> {
    if bytes.len() > MAX_CLAUDE_STDIN_BYTES {
        return Err(CaptureError::Oversized);
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| CaptureError::Invalid)?;
    let rate_limits = root
        .get("rate_limits")
        .and_then(Value::as_object)
        .ok_or(CaptureError::MissingLimits)?;
    let short_window = parse_window(rate_limits.get("five_hour"), SHORT_WINDOW_MINUTES)?;
    let weekly_window = parse_window(rate_limits.get("seven_day"), WEEKLY_WINDOW_MINUTES)?;
    let snapshot = ProviderSnapshot {
        provider: ProviderId::Claude,
        observed_at: now,
        short_window,
        weekly_window,
    };
    snapshot.validate(now).map_err(|error| match error {
        ValidationError::ExpiredReset => CaptureError::Expired,
        _ => CaptureError::Invalid,
    })?;
    Ok(snapshot)
}

pub fn render_capture_status(snapshot: &ProviderSnapshot) -> String {
    format!(
        "USAGE 5H {}% LEFT | 7D {}% LEFT",
        remaining_percent(snapshot.short_window.used_percent),
        remaining_percent(snapshot.weekly_window.used_percent)
    )
}

pub fn run_claude_capture(
    mut stdin: impl std::io::Read,
    mut stdout: impl std::io::Write,
    mut stderr: impl std::io::Write,
    store: &dyn StateStore,
    now: i64,
) -> i32 {
    let mut bytes = Vec::with_capacity(MAX_CLAUDE_STDIN_BYTES + 1);
    let parsed = stdin
        .by_ref()
        .take((MAX_CLAUDE_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()
        .and_then(|_| parse_claude_statusline(&bytes, now).ok());
    let Some(snapshot) = parsed else {
        let _ = writeln!(stdout, "USAGE: NO EXACT LIMITS");
        return 0;
    };

    let status = render_capture_status(&snapshot);
    if store
        .apply(now, StateMutation::UpsertSnapshot(snapshot))
        .is_err()
    {
        let _ = writeln!(stderr, "USAGE: LOCAL STATE ERROR");
        return 2;
    }
    let _ = writeln!(stdout, "{status}");
    0
}

pub fn capture_mode_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut args = args.into_iter();
    args.next().is_some()
        && args
            .next()
            .is_some_and(|argument| argument.as_ref() == OsStr::new("claude-capture"))
        && args.next().is_none()
}

fn parse_window(
    value: Option<&Value>,
    duration_minutes: u32,
) -> Result<WindowSnapshot, CaptureError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(CaptureError::MissingLimits)?;
    let used_percent = object
        .get("used_percentage")
        .ok_or(CaptureError::MissingLimits)?
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or(CaptureError::Invalid)?;
    let resets_at = object
        .get("resets_at")
        .ok_or(CaptureError::MissingLimits)
        .and_then(parse_integer)?;
    Ok(WindowSnapshot {
        duration_minutes,
        used_percent,
        resets_at,
    })
}

fn parse_integer(value: &Value) -> Result<i64, CaptureError> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
        .or_else(|| {
            value.as_f64().and_then(|raw| {
                (raw.is_finite()
                    && raw.fract() == 0.0
                    && raw >= i64::MIN as f64
                    && raw <= i64::MAX as f64)
                    .then_some(raw as i64)
            })
        })
        .ok_or(CaptureError::Invalid)
}
