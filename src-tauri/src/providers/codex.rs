use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::DateTime;
use serde_json::Value;

use crate::{
    diagnostics::DiagnosticCode,
    model::{
        ProviderId, ProviderSnapshot, ValidationError, WindowSnapshot, SHORT_WINDOW_MINUTES,
        WEEKLY_WINDOW_MINUTES,
    },
    paths::{
        discover_candidate_files, modified_at_seconds, normalized_path, sort_and_cap, CandidateFile,
    },
};

pub use crate::paths::MAX_CANDIDATE_FILES;

pub const MAX_JSONL_TAIL_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_JSONL_RECORD_BYTES: usize = 64 * 1024;

const USED_PERCENT_KEYS: &[&str] = &[
    "used_percent",
    "usedPercentage",
    "used_percentage",
    "percent_used",
    "percentUsed",
];
const DURATION_KEYS: &[&str] = &[
    "window_minutes",
    "windowMinutes",
    "window_duration_mins",
    "windowDurationMins",
    "window_duration_minutes",
];
const RESET_KEYS: &[&str] = &["resets_at", "resetsAt", "reset_at", "resetAt"];
const MIN_CREDIBLE_EPOCH_SECONDS: i64 = 1_000_000_000;
const MAX_CREDIBLE_EPOCH_SECONDS: i64 = 9_999_999_999;
const MIN_CREDIBLE_EPOCH_MILLISECONDS: i64 = MIN_CREDIBLE_EPOCH_SECONDS * 1_000;
const MAX_CREDIBLE_EPOCH_MILLISECONDS: i64 = MAX_CREDIBLE_EPOCH_SECONDS * 1_000 + 999;

#[derive(Clone, Debug)]
pub struct ReverseReadResult {
    pub records: Vec<Value>,
    pub diagnostics: Vec<DiagnosticCode>,
}

#[derive(Clone, Debug)]
pub struct CollectResult {
    pub snapshot: Option<ProviderSnapshot>,
    pub diagnostic: Option<DiagnosticCode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractError {
    MissingRateLimits,
    MissingWindow,
    AmbiguousWindow,
    InvalidField,
    Expired,
}

pub fn read_jsonl_reverse(path: &Path) -> ReverseReadResult {
    let mut result = ReverseReadResult {
        records: Vec::new(),
        diagnostics: Vec::new(),
    };
    let Ok(mut file) = File::open(path) else {
        result.diagnostics.push(DiagnosticCode::SourceUnreadable);
        return result;
    };
    let Ok(file_len) = file.metadata().map(|metadata| metadata.len()) else {
        result.diagnostics.push(DiagnosticCode::SourceUnreadable);
        return result;
    };
    let start = file_len.saturating_sub(MAX_JSONL_TAIL_BYTES);
    let starts_on_line_boundary = if start == 0 {
        true
    } else {
        let mut previous = [0_u8; 1];
        if file.seek(SeekFrom::Start(start - 1)).is_err() || file.read_exact(&mut previous).is_err()
        {
            result.diagnostics.push(DiagnosticCode::SourceUnreadable);
            return result;
        }
        previous[0] == b'\n'
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        result.diagnostics.push(DiagnosticCode::SourceUnreadable);
        return result;
    }

    let mut bytes = Vec::with_capacity((file_len - start) as usize);
    if file
        .take(MAX_JSONL_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        result.diagnostics.push(DiagnosticCode::SourceUnreadable);
        return result;
    }

    if !starts_on_line_boundary {
        let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') else {
            return result;
        };
        bytes.drain(..=first_newline);
    }

    for raw_line in bytes.rsplit(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_RECORD_BYTES {
            result.diagnostics.push(DiagnosticCode::OversizedRecord);
            continue;
        }

        match serde_json::from_slice(line) {
            Ok(record) => result.records.push(record),
            Err(_) => result.diagnostics.push(DiagnosticCode::MalformedRecord),
        }
    }

    result
}

pub fn extract_codex_snapshot(
    record: &Value,
    observed_at_fallback: i64,
    now: i64,
) -> Result<ProviderSnapshot, ExtractError> {
    let rate_limits = record
        .get("payload")
        .and_then(|payload| payload.get("rate_limits"))
        .ok_or(ExtractError::MissingRateLimits)?
        .as_object()
        .ok_or(ExtractError::InvalidField)?;

    let mut short_windows = Vec::new();
    let mut weekly_windows = Vec::new();
    for name in ["primary", "secondary"] {
        let Some(value) = rate_limits.get(name) else {
            continue;
        };
        let window = parse_window(value)?;
        match window.duration_minutes {
            SHORT_WINDOW_MINUTES => short_windows.push(window),
            WEEKLY_WINDOW_MINUTES => weekly_windows.push(window),
            _ => {}
        }
    }

    if short_windows.len() > 1 || weekly_windows.len() > 1 {
        return Err(ExtractError::AmbiguousWindow);
    }
    if short_windows.len() != 1 || weekly_windows.len() != 1 {
        return Err(ExtractError::MissingWindow);
    }

    let observed_at = match record.get("timestamp") {
        Some(value) => normalize_timestamp(value).ok_or(ExtractError::InvalidField)?,
        None => observed_at_fallback,
    };
    let snapshot = ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at,
        short_window: short_windows.pop().expect("length checked"),
        weekly_window: weekly_windows.pop().expect("length checked"),
    };
    snapshot.validate(now).map_err(|error| match error {
        ValidationError::ExpiredReset => ExtractError::Expired,
        _ => ExtractError::InvalidField,
    })?;
    Ok(snapshot)
}

pub struct CodexCollector {
    roots: Vec<PathBuf>,
}

impl CodexCollector {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn initial_scan(&self, now: i64) -> CollectResult {
        self.collect(discover_candidate_files(&self.roots), now)
    }

    pub fn refresh_changed(&self, paths: &BTreeSet<PathBuf>, now: i64) -> CollectResult {
        let canonical_roots: Vec<_> = self
            .roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect();
        let mut canonical_paths = BTreeSet::new();
        for path in paths {
            let Ok(path) = std::fs::canonicalize(path) else {
                continue;
            };
            if canonical_roots.iter().any(|root| path.starts_with(root)) {
                canonical_paths.insert(path);
            }
        }
        let mut candidates = canonical_paths
            .into_iter()
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .filter_map(|path| {
                std::fs::metadata(&path).ok().map(|metadata| CandidateFile {
                    path,
                    modified_at: modified_at_seconds(&metadata),
                })
            })
            .collect();
        sort_and_cap(&mut candidates);
        self.collect(candidates, now)
    }

    pub fn full_rescan(&self, now: i64) -> CollectResult {
        self.collect(discover_candidate_files(&self.roots), now)
    }

    fn collect(&self, candidates: Vec<CandidateFile>, now: i64) -> CollectResult {
        if candidates.is_empty() {
            return CollectResult {
                snapshot: None,
                diagnostic: Some(DiagnosticCode::NoFiles),
            };
        }

        let mut best: Option<(ProviderSnapshot, i64, String)> = None;
        let mut diagnostic = None;
        for candidate in candidates {
            let path_key = normalized_path(&candidate.path);
            let reverse = read_jsonl_reverse(&candidate.path);
            for code in reverse.diagnostics {
                record_diagnostic(&mut diagnostic, code);
            }
            for record in reverse.records {
                match extract_codex_snapshot(&record, candidate.modified_at, now) {
                    Ok(snapshot) => {
                        let replace = best.as_ref().is_none_or(|current| {
                            snapshot.observed_at > current.0.observed_at
                                || (snapshot.observed_at == current.0.observed_at
                                    && (candidate.modified_at > current.1
                                        || (candidate.modified_at == current.1
                                            && path_key < current.2)))
                        });
                        if replace {
                            best = Some((snapshot, candidate.modified_at, path_key.clone()));
                        }
                    }
                    Err(error) => record_diagnostic(&mut diagnostic, diagnostic_for_extract(error)),
                }
            }
        }

        match best {
            Some((snapshot, _, _)) => CollectResult {
                snapshot: Some(snapshot),
                diagnostic: None,
            },
            None => CollectResult {
                snapshot: None,
                diagnostic: Some(diagnostic.unwrap_or(DiagnosticCode::NoExactLimits)),
            },
        }
    }
}

fn parse_window(value: &Value) -> Result<WindowSnapshot, ExtractError> {
    let object = value.as_object().ok_or(ExtractError::InvalidField)?;
    let used_percent = single_field(object, USED_PERCENT_KEYS)?
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or(ExtractError::InvalidField)?;
    let duration = single_field(object, DURATION_KEYS)?;
    let duration_minutes = duration
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ExtractError::InvalidField)?;
    let resets_at =
        normalize_timestamp(single_field(object, RESET_KEYS)?).ok_or(ExtractError::InvalidField)?;

    Ok(WindowSnapshot {
        duration_minutes,
        used_percent,
        resets_at,
    })
}

fn single_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    accepted_keys: &[&str],
) -> Result<&'a Value, ExtractError> {
    let mut fields = accepted_keys.iter().filter_map(|key| object.get(*key));
    let field = fields.next().ok_or(ExtractError::InvalidField)?;
    if fields.next().is_some() {
        return Err(ExtractError::InvalidField);
    }
    Ok(field)
}

fn normalize_timestamp(value: &Value) -> Option<i64> {
    if let Some(raw) = value.as_i64() {
        return normalize_epoch(raw);
    }
    if let Some(raw) = value.as_u64() {
        return i64::try_from(raw).ok().and_then(normalize_epoch);
    }
    if let Some(raw) = value.as_f64() {
        if raw.is_finite() && raw.fract() == 0.0 && raw >= i64::MIN as f64 && raw <= i64::MAX as f64
        {
            return normalize_epoch(raw as i64);
        }
        return None;
    }
    value
        .as_str()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|timestamp| timestamp.timestamp())
}

fn normalize_epoch(value: i64) -> Option<i64> {
    if (MIN_CREDIBLE_EPOCH_SECONDS..=MAX_CREDIBLE_EPOCH_SECONDS).contains(&value) {
        Some(value)
    } else if (MIN_CREDIBLE_EPOCH_MILLISECONDS..=MAX_CREDIBLE_EPOCH_MILLISECONDS).contains(&value) {
        Some(value / 1_000)
    } else {
        None
    }
}

fn diagnostic_for_extract(error: ExtractError) -> DiagnosticCode {
    match error {
        ExtractError::MissingRateLimits | ExtractError::MissingWindow => {
            DiagnosticCode::NoExactLimits
        }
        ExtractError::AmbiguousWindow => DiagnosticCode::AmbiguousWindow,
        ExtractError::InvalidField => DiagnosticCode::InvalidSchema,
        ExtractError::Expired => DiagnosticCode::ExpiredSnapshot,
    }
}

fn record_diagnostic(current: &mut Option<DiagnosticCode>, candidate: DiagnosticCode) {
    if current.is_none() {
        *current = Some(candidate);
    }
}
