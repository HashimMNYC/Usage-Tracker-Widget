use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, FileTimes, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::{Duration, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tempfile::TempDir;
use usage_widget::{
    diagnostics::DiagnosticCode,
    model::ProviderId,
    paths::{discover_candidate_files, resolve_codex_roots},
    providers::codex::{
        extract_codex_snapshot, read_jsonl_reverse, CodexCollector, ExtractError,
        MAX_CANDIDATE_FILES, MAX_JSONL_RECORD_BYTES,
    },
};

const NOW: i64 = 2_000_000_000;

fn complete_record(observed_at: i64) -> Value {
    json!({
        "timestamp": observed_at,
        "payload": {
            "type": "token_count",
            "rate_limits": {
                "secondary": {
                    "used_percent": 62.0,
                    "window_minutes": 10_080,
                    "resets_at": NOW + 86_400
                },
                "primary": {
                    "used_percent": 38.4,
                    "window_minutes": 300,
                    "resets_at": NOW + 3_600
                }
            }
        }
    })
}

fn write_jsonl(path: &Path, records: &[Value]) {
    let body = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{body}\n")).unwrap();
}

#[test]
fn extracts_complete_snapshot_and_classifies_windows_by_exact_duration() {
    let complete = json!({
        "timestamp": "2033-05-18T03:33:10Z",
        "payload": {
            "type": "token_count",
            "rate_limits": {
                "secondary": {
                    "used_percent": 62.0,
                    "window_minutes": 10_080,
                    "resets_at": NOW + 86_400
                },
                "primary": {
                    "used_percent": 38.4,
                    "window_minutes": 300,
                    "resets_at": NOW + 3_600
                }
            }
        }
    });

    let snapshot = extract_codex_snapshot(&complete, NOW - 10, NOW).unwrap();

    assert_eq!(snapshot.provider, ProviderId::Codex);
    assert_eq!(snapshot.observed_at, NOW - 10);
    assert_eq!(snapshot.short_window.duration_minutes, 300);
    assert_eq!(snapshot.short_window.used_percent, 38.4);
    assert_eq!(snapshot.weekly_window.duration_minutes, 10_080);
    assert_eq!(snapshot.weekly_window.used_percent, 62.0);
}

#[test]
fn reversed_window_labels_and_unknown_children_do_not_change_duration_classification() {
    let record = json!({
        "timestamp": NOW - 10,
        "payload": {
            "rate_limits": {
                "primary": {
                    "used_percent": 55.0,
                    "window_minutes": 10_080,
                    "resets_at": NOW + 86_400
                },
                "secondary": {
                    "used_percent": 25.0,
                    "window_minutes": 300,
                    "resets_at": NOW + 3_600
                },
                "unexpected": {
                    "used_percent": 99.0,
                    "window_minutes": 300,
                    "resets_at": NOW + 3_600
                }
            }
        }
    });

    let snapshot = extract_codex_snapshot(&record, NOW - 20, NOW).unwrap();

    assert_eq!(snapshot.short_window.used_percent, 25.0);
    assert_eq!(snapshot.weekly_window.used_percent, 55.0);
}

#[test]
fn rejects_two_exact_short_windows_as_ambiguous() {
    let record = json!({
        "timestamp": NOW - 10,
        "payload": {
            "rate_limits": {
                "primary": {
                    "used_percent": 10.0,
                    "window_minutes": 300,
                    "resets_at": NOW + 3_600
                },
                "secondary": {
                    "used_percent": 20.0,
                    "window_minutes": 300,
                    "resets_at": NOW + 7_200
                }
            }
        }
    });

    assert_eq!(
        extract_codex_snapshot(&record, NOW - 10, NOW),
        Err(ExtractError::AmbiguousWindow)
    );
}

#[test]
fn ignores_invalid_partial_final_line_and_returns_preceding_complete_record() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(
        &path,
        format!("{}\n{{\"payload\":{{", complete_record(NOW - 10)),
    )
    .unwrap();

    let result = read_jsonl_reverse(&path);

    assert_eq!(result.records, vec![complete_record(NOW - 10)]);
    assert_eq!(result.diagnostics, vec![DiagnosticCode::MalformedRecord]);
}

#[test]
fn normalizes_epoch_milliseconds_and_rfc3339_reset_timestamps() {
    let record = json!({
        "timestamp": NOW - 10,
        "payload": {
            "rate_limits": {
                "primary": {
                    "usedPercentage": 12.5,
                    "windowMinutes": 300,
                    "resetsAt": (NOW + 3_600) * 1_000
                },
                "secondary": {
                    "percentUsed": 45.5,
                    "windowDurationMins": 10_080,
                    "resetAt": "2033-05-19T03:33:20Z"
                }
            }
        }
    });

    let snapshot = extract_codex_snapshot(&record, NOW - 20, NOW).unwrap();

    assert_eq!(snapshot.short_window.resets_at, NOW + 3_600);
    assert_eq!(snapshot.weekly_window.resets_at, NOW + 86_400);
}

#[test]
fn skips_record_over_sixty_four_kib_before_json_parsing() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oversized.jsonl");
    let oversized = format!(
        "{{\"payload\":{{\"padding\":\"{}\"}}}}\n",
        "x".repeat(MAX_JSONL_RECORD_BYTES)
    );
    fs::write(&path, oversized).unwrap();

    let result = read_jsonl_reverse(&path);

    assert!(result.records.is_empty());
    assert_eq!(result.diagnostics, vec![DiagnosticCode::OversizedRecord]);
}

#[test]
fn newer_malformed_record_cannot_mask_older_valid_current_snapshot() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let path = sessions.join("rollout.jsonl");
    let malformed_newer = json!({
        "timestamp": NOW - 1,
        "payload": {"rate_limits": {"primary": {"used_percent": "not-a-number"}}}
    });
    write_jsonl(&path, &[complete_record(NOW - 10), malformed_newer]);

    let result = CodexCollector::new(vec![sessions]).initial_scan(NOW);

    assert_eq!(result.snapshot.unwrap().observed_at, NOW - 10);
    assert_eq!(result.diagnostic, None);
}

#[test]
fn selects_newest_valid_snapshot_by_observation_not_file_modification_time() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    write_jsonl(
        &sessions.join("older-file.jsonl"),
        &[complete_record(NOW - 5)],
    );
    thread::sleep(Duration::from_millis(20));
    write_jsonl(
        &sessions.join("newer-file.jsonl"),
        &[complete_record(NOW - 50)],
    );

    let result = CodexCollector::new(vec![sessions]).full_rescan(NOW);

    assert_eq!(result.snapshot.unwrap().observed_at, NOW - 5);
}

#[test]
fn resolves_only_codex_session_roots_and_discovery_sorts_and_caps_jsonl_files() {
    let temp = TempDir::new().unwrap();
    let custom_home = temp.path().join("custom-codex");
    let roots = resolve_codex_roots(Some(custom_home.as_os_str()), temp.path());
    assert_eq!(
        roots,
        vec![
            custom_home.join("sessions"),
            custom_home.join("archived_sessions")
        ]
    );
    assert_eq!(
        resolve_codex_roots(None, temp.path()),
        vec![
            temp.path().join(".codex").join("sessions"),
            temp.path().join(".codex").join("archived_sessions")
        ]
    );

    for root in &roots {
        fs::create_dir_all(root).unwrap();
    }
    let outside = custom_home.join("not-a-session");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("outside.jsonl"), "{}\n").unwrap();
    fs::write(roots[0].join("ignored.txt"), "{}\n").unwrap();
    let newest_path = roots[0].join("newest.jsonl");
    fs::write(&newest_path, "{}\n").unwrap();
    OpenOptions::new()
        .write(true)
        .open(&newest_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(NOW as u64)))
        .unwrap();

    for index in 0..(MAX_CANDIDATE_FILES + 2) {
        let root = &roots[index % roots.len()];
        fs::write(root.join(format!("rollout-{index:03}.jsonl")), "{}\n").unwrap();
    }

    let candidates = discover_candidate_files(&roots);

    assert_eq!(candidates.len(), MAX_CANDIDATE_FILES);
    assert_eq!(candidates[0].path, newest_path);
    assert!(candidates
        .windows(2)
        .all(|pair| pair[0].modified_at >= pair[1].modified_at));
    assert!(candidates.iter().all(|candidate| {
        candidate.path.extension() == Some(OsStr::new("jsonl"))
            && roots.iter().any(|root| candidate.path.starts_with(root))
    }));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.path == outside.join("outside.jsonl")));
}

#[test]
fn refresh_changed_ignores_paths_outside_configured_roots() {
    let temp = TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    let outside = temp.path().join("outside");
    fs::create_dir(&sessions).unwrap();
    fs::create_dir(&outside).unwrap();
    let inside_path = sessions.join("inside.jsonl");
    let outside_path = outside.join("outside.jsonl");
    write_jsonl(&inside_path, &[complete_record(NOW - 20)]);
    write_jsonl(&outside_path, &[complete_record(NOW - 1)]);
    let disguised_outside_path = sessions.join("..").join("outside").join("outside.jsonl");
    let changed = BTreeSet::from([
        PathBuf::from(&inside_path),
        outside_path,
        disguised_outside_path,
    ]);

    let result = CodexCollector::new(vec![sessions]).refresh_changed(&changed, NOW);

    assert_eq!(result.snapshot.unwrap().observed_at, NOW - 20);
}
