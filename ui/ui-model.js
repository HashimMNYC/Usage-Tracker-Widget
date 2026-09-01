export const METER_CELLS = 10;

export const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

const isFiniteNumber = (value) => typeof value === "number" && Number.isFinite(value);
const isRecord = (value) => typeof value === "object" && value !== null && !Array.isArray(value);

export const remainingPercent = (used) => isFiniteNumber(used) ? Math.round(clamp(100 - used, 0, 100)) : 0;

export function meterText(remaining) {
  const cells = isFiniteNumber(remaining) ? clamp(Math.round(remaining / 10), 0, METER_CELLS) : 0;
  return `[${"█".repeat(cells)}${"░".repeat(METER_CELLS - cells)}]`;
}

export const meterTone = (remaining) => !isFiniteNumber(remaining) || remaining < 10 ? "red" : remaining < 30 ? "amber" : "provider";

export function formatCountdown(resetsAtSeconds, nowMs) {
  if (!isFiniteNumber(resetsAtSeconds) || !isFiniteNumber(nowMs)) return "00M 00S";
  let seconds = Math.max(0, Math.floor(resetsAtSeconds - nowMs / 1000));
  const days = Math.floor(seconds / 86400); seconds %= 86400;
  const hours = Math.floor(seconds / 3600); seconds %= 3600;
  const minutes = Math.floor(seconds / 60); const secs = seconds % 60;
  const pad = (n) => String(n).padStart(2, "0");
  if (days >= 1) return `${days}D ${pad(hours)}H`;
  if (hours >= 1) return `${pad(hours)}H ${pad(minutes)}M`;
  return `${pad(minutes)}M ${pad(secs)}S`;
}

export function visibleProviders(view, nowSeconds) {
  if (!isRecord(view) || !Array.isArray(view.providers) || !isFiniteNumber(nowSeconds)) return [];

  const candidates = new Map([["codex", []], ["claude", []]]);
  for (const item of view.providers) {
    if (!isRecord(item) || !candidates.has(item.provider) || !isFiniteNumber(item.observed_at)) continue;
    const shortWindow = item.short_window;
    const weeklyWindow = item.weekly_window;
    if (!isRecord(shortWindow) || !isRecord(weeklyWindow)) continue;
    if (
      shortWindow.duration_minutes !== 300 ||
      weeklyWindow.duration_minutes !== 10080 ||
      !isFiniteNumber(shortWindow.resets_at) ||
      !isFiniteNumber(weeklyWindow.resets_at) ||
      shortWindow.resets_at <= nowSeconds ||
      weeklyWindow.resets_at <= nowSeconds ||
      !isFiniteNumber(shortWindow.used_percent) ||
      !isFiniteNumber(weeklyWindow.used_percent) ||
      shortWindow.used_percent < 0 || shortWindow.used_percent > 100 ||
      weeklyWindow.used_percent < 0 || weeklyWindow.used_percent > 100
    ) continue;
    candidates.get(item.provider).push(item);
  }

  return ["codex", "claude"].flatMap((provider) => {
    const matches = candidates.get(provider);
    return matches.length === 1 ? matches : [];
  });
}

export const layoutForProviderCount = (count) => count === 0 ? "empty" : count === 1 ? "single" : "dual";

export function createLayoutSynchronizer(setLayout, commit) {
  let committedCount = -1;
  let desiredCount = -1;
  let pending = null;

  const start = () => {
    const attempt = desiredCount;
    let succeeded = false;
    pending = Promise.resolve()
      .then(() => setLayout(layoutForProviderCount(attempt)))
      .then(() => {
        committedCount = attempt;
        commit(attempt);
        succeeded = true;
      })
      .finally(() => {
        pending = null;
        if (succeeded && desiredCount !== committedCount) {
          void start().catch(() => {});
        }
      });
    return pending;
  };

  return {
    sync(count) {
      desiredCount = count;
      if (pending) return pending;
      if (desiredCount === committedCount) return Promise.resolve();
      return start();
    }
  };
}

export function createLatestOnlyRefresh(request, commit) {
  let pending = false;
  let refreshQueued = false;

  const run = async () => {
    if (pending) {
      refreshQueued = true;
      return;
    }

    pending = true;
    try {
      const view = await request();
      if (!refreshQueued) commit(view);
    } finally {
      pending = false;
      if (refreshQueued) {
        refreshQueued = false;
        return run();
      }
    }
  };

  return {run};
}
