import {
  formatCountdown, meterText, meterTone, providerWindowEntries, remainingPercent
} from "./ui-model.js";

const providerName = (provider) => provider === "codex" ? "CODEX" : "CLAUDE";

const createElement = (tagName, className, text) => {
  const element = document.createElement(tagName);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
};

const resetText = (resetsAt, nowMs) => `RESET ${formatCountdown(resetsAt, nowMs)}`;

function observationText(observedAt, nowMs) {
  const seconds = Math.floor(nowMs / 1000 - observedAt);
  if (!Number.isFinite(seconds) || seconds < 0) return "OBSERVATION TIME UNAVAILABLE";
  const [value, unit] = seconds < 60 ? [seconds, "S"]
    : seconds < 3600 ? [Math.floor(seconds / 60), "M"]
    : seconds < 86400 ? [Math.floor(seconds / 3600), "H"]
    : [Math.floor(seconds / 86400), "D"];
  return `OBSERVED ${value}${unit} AGO`;
}

function createWindowRow(provider, label, window, nowMs) {
  const remaining = remainingPercent(window.used_percent);
  const row = createElement("section", "window-row");
  const meter = createElement("span", `meter meter--${provider} meter--${meterTone(remaining)}`, meterText(remaining));
  meter.setAttribute("role", "progressbar");
  meter.setAttribute("aria-label", `${label} remaining`);
  meter.setAttribute("aria-valuemin", "0");
  meter.setAttribute("aria-valuemax", "100");
  meter.setAttribute("aria-valuenow", String(remaining));

  const reset = createElement("span", "reset", resetText(window.resets_at, nowMs));
  reset.dataset.resetAt = String(window.resets_at);

  row.append(
    createElement("span", "window-label", label),
    meter,
    createElement("span", "window-percent", `${remaining}% LEFT`),
    reset
  );
  return row;
}

function createProviderCard(provider, nowMs) {
  const card = createElement("article", `provider-card provider--${provider.provider}`);
  card.append(createElement("h2", "provider-heading", providerName(provider.provider)));
  for (const [label, window] of providerWindowEntries(provider)) {
    card.append(createWindowRow(provider.provider, label, window, nowMs));
  }
  const observation = createElement("p", "observation-age", observationText(provider.observed_at, nowMs));
  observation.dataset.observedAt = String(provider.observed_at);
  card.append(observation);
  return card;
}

export function renderProviders(container, providers, nowMs) {
  if (providers.length === 0) {
    container.replaceChildren(createElement("p", "empty-state", "NO CURRENT LIMIT DATA"));
    return;
  }
  container.replaceChildren(...providers.map((provider) => createProviderCard(provider, nowMs)));
}

export function updateCountdowns(container, nowMs) {
  for (const reset of container.querySelectorAll("[data-reset-at]")) {
    reset.textContent = resetText(Number(reset.dataset.resetAt), nowMs);
  }
  for (const observation of container.querySelectorAll("[data-observed-at]")) {
    observation.textContent = observationText(Number(observation.dataset.observedAt), nowMs);
  }
}
