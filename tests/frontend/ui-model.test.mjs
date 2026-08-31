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
