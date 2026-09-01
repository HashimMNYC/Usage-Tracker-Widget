use usage_widget::model::{
    remaining_percent, ProviderId, ProviderSnapshot, ValidationError, WindowSnapshot,
};

const NOW: i64 = 2_000_000_000;

fn valid_snapshot() -> ProviderSnapshot {
    ProviderSnapshot {
        provider: ProviderId::Codex,
        observed_at: NOW - 10,
        short_window: Some(WindowSnapshot {
            duration_minutes: 300,
            used_percent: 38.4,
            resets_at: NOW + 3_600,
        }),
        weekly_window: Some(WindowSnapshot {
            duration_minutes: 10_080,
            used_percent: 62.0,
            resets_at: NOW + 86_400,
        }),
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
    expired.short_window.as_mut().unwrap().resets_at = NOW;
    assert_eq!(expired.validate(NOW), Err(ValidationError::ExpiredReset));

    let mut wrong = valid_snapshot();
    wrong.weekly_window.as_mut().unwrap().duration_minutes = 1_440;
    assert_eq!(wrong.validate(NOW), Err(ValidationError::WrongDuration));
}

#[test]
fn rejects_non_finite_and_out_of_range_percentages() {
    for value in [f64::NAN, f64::INFINITY, -0.1, 100.1] {
        let mut snapshot = valid_snapshot();
        snapshot.short_window.as_mut().unwrap().used_percent = value;
        assert_eq!(snapshot.validate(NOW), Err(ValidationError::InvalidPercent));
    }
}

#[test]
fn claude_requires_both_exact_windows_while_codex_may_be_partial() {
    let mut weekly_only = valid_snapshot();
    weekly_only.short_window = None;
    assert_eq!(weekly_only.validate(NOW), Ok(()));

    weekly_only.provider = ProviderId::Claude;
    assert_eq!(
        weekly_only.validate(NOW),
        Err(ValidationError::MissingWindows)
    );

    let mut short_only = valid_snapshot();
    short_only.provider = ProviderId::Claude;
    short_only.weekly_window = None;
    assert_eq!(
        short_only.validate(NOW),
        Err(ValidationError::MissingWindows)
    );
}
