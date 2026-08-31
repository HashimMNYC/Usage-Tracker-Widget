import test from "node:test";
import assert from "node:assert/strict";
import {
  formatCountdown, layoutForProviderCount, meterText, meterTone,
  remainingPercent, visibleProviders
} from "../../ui/ui-model.js";

test("renders the approved ten-cell block meter", () => {
  assert.equal(remainingPercent(38.4), 62);
  assert.equal(meterText(62), "[██████░░░░]");
  assert.equal(meterText(96), "[██████████]");
  assert.equal(meterTone(30), "provider");
  assert.equal(meterTone(29), "amber");
  assert.equal(meterTone(9), "red");
});

test("formats reset countdowns", () => {
  const now = 1_000_000;
  assert.equal(formatCountdown(now / 1000 + 90_061, now), "1D 01H");
  assert.equal(formatCountdown(now / 1000 + 3_661, now), "01H 01M");
  assert.equal(formatCountdown(now / 1000 + 59, now), "00M 59S");
});

test("hides incomplete and expired providers", () => {
  const complete = {
    provider: "codex",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };
  assert.deepEqual(visibleProviders({providers: [complete]}, 100), [complete]);
  assert.deepEqual(visibleProviders({providers: [complete]}, 200), []);
  assert.equal(layoutForProviderCount(0), "empty");
  assert.equal(layoutForProviderCount(1), "single");
  assert.equal(layoutForProviderCount(2), "dual");
});

test("fails closed for malformed provider views and fields", () => {
  const valid = {
    provider: "codex",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };
  const malformedViews = [
    null,
    {providers: "not-an-array"},
    {providers: [null, "codex", 42]},
    {providers: [{...valid, observed_at: undefined}]},
    {providers: [{...valid, observed_at: Infinity}]},
    {providers: [{...valid, short_window: {...valid.short_window, resets_at: "200"}}]},
    {providers: [{...valid, short_window: {...valid.short_window, resets_at: Infinity}}]},
    {providers: [{...valid, weekly_window: {...valid.weekly_window, resets_at: "300"}}]}
  ];

  for (const view of malformedViews) {
    assert.deepEqual(visibleProviders(view, 100), []);
  }
});

test("fails closed on duplicate identities and keeps fixed provider order", () => {
  const codex = {
    provider: "codex",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };
  const claude = {
    provider: "claude",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };

  assert.deepEqual(
    visibleProviders({providers: [claude, codex]}, 100).map((item) => item.provider),
    ["codex", "claude"]
  );
  assert.deepEqual(
    visibleProviders({providers: [codex, codex, claude]}, 100).map((item) => item.provider),
    ["claude"]
  );
  assert.deepEqual(visibleProviders({providers: [codex, codex]}, 100), []);
});

test("uses safe text defaults for invalid numeric presentation input", () => {
  assert.equal(remainingPercent(Number.NaN), 0);
  assert.equal(meterText(Number.NaN), "[░░░░░░░░░░]");
  assert.equal(formatCountdown(Number.NaN, 1_000), "00M 00S");
  assert.equal(formatCountdown(Infinity, 1_000), "00M 00S");
  assert.equal(formatCountdown(100, Infinity), "00M 00S");
});
