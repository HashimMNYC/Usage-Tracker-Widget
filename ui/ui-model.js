export const METER_CELLS = 10;

export const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

export const remainingPercent = (used) => Math.round(clamp(100 - used, 0, 100));

export function meterText(remaining) {
  const cells = clamp(Math.round(remaining / 10), 0, METER_CELLS);
  return `[${"█".repeat(cells)}${"░".repeat(METER_CELLS - cells)}]`;
}

export const meterTone = (remaining) => remaining < 10 ? "red" : remaining < 30 ? "amber" : "provider";

export function formatCountdown(resetsAtSeconds, nowMs) {
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
  const order = new Map([["codex", 0], ["claude", 1]]);
  return (view.providers ?? []).filter((item) =>
    order.has(item.provider) &&
    item.short_window?.duration_minutes === 300 &&
    item.weekly_window?.duration_minutes === 10080 &&
    item.short_window.resets_at > nowSeconds &&
    item.weekly_window.resets_at > nowSeconds &&
    Number.isFinite(item.short_window.used_percent) &&
    Number.isFinite(item.weekly_window.used_percent) &&
    item.short_window.used_percent >= 0 && item.short_window.used_percent <= 100 &&
    item.weekly_window.used_percent >= 0 && item.weekly_window.used_percent <= 100
  ).sort((a, b) => order.get(a.provider) - order.get(b.provider));
}

export const layoutForProviderCount = (count) => count === 0 ? "empty" : count === 1 ? "single" : "dual";
