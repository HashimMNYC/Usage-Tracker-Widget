import { getWidgetView, hideWidget, onRefreshCompleted, setWidgetHeight } from "./bridge.js";
import { renderProviders, updateCountdowns } from "./render.js";
import {
  createHeightSynchronizer, createLatestOnlyRefresh, measuredWidgetHeight, visibleProviders
} from "./ui-model.js";

const providersElement = document.querySelector("#providers");
const hideButton = document.querySelector("#hide-widget");
const refreshStatus = document.querySelector("#refresh-status");
let currentView = {providers: []};
let renderedStructure = "";
let measurementFrame = null;
let bodyObserver = null;
const heightSynchronizer = createHeightSynchronizer(setWidgetHeight, () => {});

function currentProviders() {
  return visibleProviders(currentView, Date.now() / 1000);
}

function renderCurrentView(forceRender = false) {
  const nowMs = Date.now();
  const providers = currentProviders();
  const nextStructure = providers.map((provider) => [
    provider.provider,
    provider.short_window == null ? "" : "5h",
    provider.weekly_window == null ? "" : "7d"
  ].join(":")).join("|");

  if (forceRender || nextStructure !== renderedStructure) {
    renderProviders(providersElement, providers, nowMs);
    renderedStructure = nextStructure;
  } else {
    updateCountdowns(providersElement, nowMs);
  }
  scheduleHeightSync();
}

function scheduleHeightSync() {
  if (measurementFrame !== null) return;
  measurementFrame = requestAnimationFrame(() => {
    measurementFrame = null;
    const height = measuredWidgetHeight(document.body);
    if (height !== null) void heightSynchronizer.sync(height).catch(() => {});
  });
}

function applyView(view) {
  currentView = view;
  renderCurrentView(true);
}

const viewGate = createLatestOnlyRefresh(getWidgetView, applyView);

function showRefreshStatus(succeeded) {
  refreshStatus.textContent = succeeded ? "LOCAL DATA CHECKED" : "REFRESH FAILED";
  refreshStatus.dataset.failed = String(!succeeded);
  scheduleHeightSync();
}

function handleRefreshCompleted(succeeded) {
  if (typeof succeeded !== "boolean") return;
  showRefreshStatus(succeeded);
  if (succeeded) void viewGate.run().catch(() => showRefreshStatus(false));
}

async function initialize() {
  try {
    await onRefreshCompleted(handleRefreshCompleted);
  } catch {
    // Periodic reads remain available if the event listener cannot be installed.
  }
  try {
    await viewGate.run();
  } catch {
    renderCurrentView();
  }
}

function hide() {
  void hideWidget().catch(() => {});
}

hideButton.addEventListener("pointerdown", (event) => event.stopPropagation());
hideButton.addEventListener("pointerup", (event) => event.stopPropagation());
hideButton.addEventListener("click", hide);
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") hide();
});

if ("ResizeObserver" in window) {
  bodyObserver = new ResizeObserver(scheduleHeightSync);
  bodyObserver.observe(document.body);
}
window.addEventListener("resize", scheduleHeightSync);

void initialize();
setInterval(() => void viewGate.run().catch(() => {}), 5_000);
setInterval(() => renderCurrentView(), 1_000);
