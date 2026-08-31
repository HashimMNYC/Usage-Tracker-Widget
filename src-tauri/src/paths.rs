use std::{ffi::OsStr, path::PathBuf, time::UNIX_EPOCH};

use walkdir::WalkDir;

pub const MAX_CANDIDATE_FILES: usize = 128;

#[derive(Clone, Debug)]
pub struct CandidateFile {
    pub path: PathBuf,
    pub modified_at: i64,
}

pub fn resolve_codex_roots(
    codex_home: Option<&OsStr>,
    user_profile: &std::path::Path,
) -> Vec<PathBuf> {
    let home = codex_home
        .map(PathBuf::from)
        .unwrap_or_else(|| user_profile.join(".codex"));
    vec![home.join("sessions"), home.join("archived_sessions")]
}

pub fn discover_candidate_files(roots: &[PathBuf]) -> Vec<CandidateFile> {
    let mut candidates = Vec::new();

    for root in roots {
        for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(OsStr::to_str) != Some("jsonl")
            {
                continue;
            }

            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            candidates.push(CandidateFile {
                path: entry.into_path(),
                modified_at: modified_at_seconds(&metadata),
            });
            if candidates.len() > MAX_CANDIDATE_FILES {
                sort_and_cap(&mut candidates);
            }
        }
    }

    sort_and_cap(&mut candidates);
    candidates
}

pub(crate) fn modified_at_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .and_then(|value| i64::try_from(value.as_secs()).ok())
        .unwrap_or(0)
}

pub(crate) fn normalized_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

pub(crate) fn sort_and_cap(candidates: &mut Vec<CandidateFile>) {
    candidates.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| normalized_path(&left.path).cmp(&normalized_path(&right.path)))
    });
    candidates.truncate(MAX_CANDIDATE_FILES);
}
