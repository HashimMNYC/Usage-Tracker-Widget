use usage_widget::model::{
    remaining_percent, ProviderId, ProviderSnapshot, ValidationError, WindowSnapshot,
};

const NOW: i64 = 2_000_000_000;

fn valid_snapshot() -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at: NOW - 10,
        short_window: WindowSnapshot {
            duration_minutes: 300,
            used_percent: 38.4,
            resets_at: NOW + 3_600,
        },
        weekly_window: WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 62.0,
            resets_at: NOW + 86_400,
        },
    }
}

#[test]
fn validates_both_exact_windows_and_derives_remaining() {
    assert_eq!(valid_snapshot().validate(NOW), Ok(()));
    assert_eq!(remaining_percent(38.4), 62);
    assert_eq!(remaining_percent(100.0), 0);
}

#[test]
fn rejects_expired_or_wrong_duration_windows() {
    let mut expired = valid_snapshot();
    expired.short_window.resets_at = NOW;
    assert_eq!(expired.validate(NOW), Err(ValidationError::ExpiredReset));

    let mut wrong = valid_snapshot();
    wrong.weekly_window.duration_minutes = 1_440;
    assert_eq!(wrong.validate(NOW), Err(ValidationError::WrongDuration));
}

#[test]
fn rejects_non_finite_and_out_of_range_percentages() {
    for value in [f64::NAN, f64::INFINITY, -0.1, 100.1] {
        let mut snapshot = valid_snapshot();
        snapshot.short_window.used_percent = value;
        assert_eq!(snapshot.validate(NOW), Err(ValidationError::InvalidPercent));
    }
}
