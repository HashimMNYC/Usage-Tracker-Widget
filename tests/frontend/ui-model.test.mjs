import test from "node:test";
import assert from "node:assert/strict";
import {
  createHeightSynchronizer, formatCountdown, measuredWidgetHeight, meterText, meterTone,
  providerWindowEntries, remainingPercent, visibleProviders
} from "../../ui/ui-model.js";

test("retries a failed measured height until it is successfully committed", async () => {
  const attempts = [];
  const committed = [];
  let shouldFail = true;
  const layout = createHeightSynchronizer(async (value) => {
    attempts.push(value);
    if (shouldFail) throw new Error("injected layout failure");
  }, (count) => committed.push(count));

  await assert.rejects(layout.sync(187), /injected layout failure/);
  shouldFail = false;
  await layout.sync(187);
  await layout.sync(187);

  assert.deepEqual(attempts, [187, 187]);
  assert.deepEqual(committed, [187]);
});

test("coalesces pending height calls and follows a changed measurement", async () => {
  const attempts = [];
  const committed = [];
  let releaseFirst;
  const firstGate = new Promise((resolve) => { releaseFirst = resolve; });
  const layout = createHeightSynchronizer(async (value) => {
    attempts.push(value);
    if (attempts.length === 1) await firstGate;
  }, (count) => committed.push(count));

  const first = layout.sync(187);
  const coalesced = layout.sync(263);
  assert.equal(first, coalesced);
  releaseFirst();
  await first;
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(attempts, [187, 263]);
  assert.deepEqual(committed, [187, 263]);
});

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
});

test("shows an exact weekly-only provider and leaves the short window absent", () => {
  const weeklyOnly = {
    provider: "codex",
    observed_at: 90,
    weekly_window: {duration_minutes: 10080, used_percent: 35, resets_at: 300}
  };

  assert.deepEqual(visibleProviders({providers: [weeklyOnly]}, 100), [weeklyOnly]);
  assert.equal(remainingPercent(weeklyOnly.weekly_window.used_percent), 65);
  assert.deepEqual(providerWindowEntries(weeklyOnly), [
    ["7D", weeklyOnly.weekly_window]
  ]);
});

test("hides Claude unless both exact windows are present", () => {
  const complete = {
    provider: "claude",
    observed_at: 90,
    short_window: {duration_minutes: 300, used_percent: 10, resets_at: 200},
    weekly_window: {duration_minutes: 10080, used_percent: 20, resets_at: 300}
  };

  assert.deepEqual(visibleProviders({providers: [complete]}, 100), [complete]);
  assert.deepEqual(
    visibleProviders({providers: [{...complete, short_window: null}]}, 100),
    []
  );
  assert.deepEqual(
    visibleProviders({providers: [{...complete, weekly_window: null}]}, 100),
    []
  );
});

test("measures the rendered body height with a rounding safety pixel", () => {
  const body = {
    scrollHeight: 188,
    getBoundingClientRect: () => ({height: 188.2})
  };

  assert.equal(measuredWidgetHeight(body), 190);
  assert.equal(measuredWidgetHeight(null), null);
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
