import { getWidgetView, hideWidget, refresh, setWidgetLayout } from "./bridge.js";
import { renderProviders, updateCountdowns } from "./render.js";
import { layoutForProviderCount, visibleProviders } from "./ui-model.js";

const providersElement = document.querySelector("#providers");
const hideButton = document.querySelector("#hide-widget");
let currentView = {providers: []};
let visibleCount = -1;

function currentProviders() {
  return visibleProviders(currentView, Date.now() / 1000);
}

function renderCurrentView(forceRender = false) {
  const nowMs = Date.now();
  const providers = currentProviders();
  const nextCount = providers.length;

  if (forceRender || nextCount !== visibleCount) {
    renderProviders(providersElement, providers, nowMs);
  } else {
    updateCountdowns(providersElement, nowMs);
  }

  if (nextCount !== visibleCount) {
    visibleCount = nextCount;
    void setWidgetLayout(layoutForProviderCount(nextCount)).catch(() => {});
  }
}

async function loadView(viewRequest) {
  try {
    currentView = await viewRequest();
    renderCurrentView(true);
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

void loadView(getWidgetView);
setInterval(() => void loadView(refresh), 5_000);
setInterval(() => renderCurrentView(), 1_000);
