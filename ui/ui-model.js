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
    const shortMissing = shortWindow === undefined || shortWindow === null;
    const weeklyMissing = weeklyWindow === undefined || weeklyWindow === null;
    if (shortMissing && weeklyMissing) continue;
    if (item.provider === "claude" && (shortMissing || weeklyMissing)) continue;
    if (!shortMissing && !isValidWindow(shortWindow, 300, nowSeconds)) continue;
    if (!weeklyMissing && !isValidWindow(weeklyWindow, 10080, nowSeconds)) continue;
    candidates.get(item.provider).push(item);
  }

  return ["codex", "claude"].flatMap((provider) => {
    const matches = candidates.get(provider);
    return matches.length === 1 ? matches : [];
  });
}

function isValidWindow(window, expectedDuration, nowSeconds) {
  return isRecord(window) &&
    window.duration_minutes === expectedDuration &&
    isFiniteNumber(window.resets_at) &&
    window.resets_at > nowSeconds &&
    isFiniteNumber(window.used_percent) &&
    window.used_percent >= 0 && window.used_percent <= 100;
}

export function providerWindowEntries(provider) {
  const entries = [];
  if (provider?.short_window != null) entries.push(["5H", provider.short_window]);
  if (provider?.weekly_window != null) entries.push(["7D", provider.weekly_window]);
  return entries;
}

export function measuredWidgetHeight(body) {
  if (!body || typeof body.getBoundingClientRect !== "function") return null;
  const rectHeight = body.getBoundingClientRect().height;
  if (!isFiniteNumber(rectHeight) || rectHeight < 0) return null;
  return Math.ceil(rectHeight) + 1;
}

export function createHeightSynchronizer(setHeight, commit) {
  let committedHeight = -1;
  let desiredHeight = -1;
  let pending = null;

  const start = () => {
    const attempt = desiredHeight;
    let succeeded = false;
    pending = Promise.resolve()
      .then(() => setHeight(attempt))
      .then(() => {
        committedHeight = attempt;
        commit(attempt);
        succeeded = true;
      })
      .finally(() => {
        pending = null;
        if (succeeded && desiredHeight !== committedHeight) {
          void start().catch(() => {});
        }
      });
    return pending;
  };

  return {
    sync(height) {
      desiredHeight = height;
      if (pending) return pending;
      if (desiredHeight === committedHeight) return Promise.resolve();
      return start();
    }
  };
}

export function createLatestOnlyRefresh(request, commit) {
  let pending = null;
  let refreshQueued = false;

  const run = () => {
    if (pending) {
      refreshQueued = true;
      return pending;
    }

    let resolveCompletion;
    let rejectCompletion;
    pending = new Promise((resolve, reject) => {
      resolveCompletion = resolve;
      rejectCompletion = reject;
    });
    const completion = pending;
    void (async () => {
      try {
        do {
          refreshQueued = false;
          try {
            const view = await request();
            if (!refreshQueued) commit(view);
          } catch (error) {
            if (!refreshQueued) throw error;
          }
        } while (refreshQueued);
        resolveCompletion();
      } catch (error) {
        rejectCompletion(error);
      } finally {
        pending = null;
      }
    })();
    return completion;
  };

  return {run};
}
