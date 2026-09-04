import test from "node:test";
import assert from "node:assert/strict";

const NOW = 2_000_000_000;
const flushTasks = () => new Promise((resolve) => setImmediate(resolve));

function view(used = 61, observedAt = NOW - 12) {
  return {providers: [{
    provider: "codex",
    observed_at: observedAt,
    weekly_window: {duration_minutes: 10080, used_percent: used, resets_at: NOW + 86400}
  }]};
}

async function startApp(t, {read = async () => view(), listenFails = false} = {}) {
  const savedGlobals = new Map(["document", "window", "requestAnimationFrame", "setInterval"]
    .map((name) => [name, globalThis[name]]));
  const originalNow = Date.now;
  t.after(() => {
    Date.now = originalNow;
    for (const [name, value] of savedGlobals) {
      if (value === undefined) delete globalThis[name];
      else globalThis[name] = value;
    }
  });

  class Element {
    constructor() {
      this.dataset = {};
      this.children = [];
      this.attributes = {};
      this.textContent = "";
    }
    addEventListener() {}
    setAttribute(name, value) { this.attributes[name] = value; }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    getBoundingClientRect() { return {height: 180}; }
    querySelectorAll(selector) {
      const key = {"[data-reset-at]": "resetAt", "[data-observed-at]": "observedAt"}[selector];
      return this.children.flatMap((child) => [
        ...(key && key in child.dataset ? [child] : []), ...child.querySelectorAll(selector)
      ]);
    }
  }

  const providers = new Element();
  const status = new Element();
  const intervals = new Map();
  const listeners = new Map();
  let clock = NOW * 1000;
  Date.now = () => clock;
  globalThis.document = {
    body: new Element(),
    querySelector(selector) {
      return {"#providers": providers, "#refresh-status": status, "#hide-widget": new Element()}[selector];
    },
    createElement() { return new Element(); },
    addEventListener() {}
  };
  globalThis.window = {
    __TAURI__: {
      core: {invoke(command) {
        if (command === "get_widget_view") return read();
        return Promise.resolve();
      }},
      event: {async listen(name, callback) {
        if (listenFails) throw new Error("event listener unavailable");
        listeners.set(name, callback);
        return () => listeners.delete(name);
      }}
    },
    addEventListener() {}
  };
  globalThis.requestAnimationFrame = (callback) => { queueMicrotask(callback); return 1; };
  globalThis.setInterval = (callback, delay) => { intervals.set(delay, callback); return delay; };

  await import(`../../ui/app.js?refresh-test=${Math.random()}`);
  await flushTasks();
  const renderedText = (element) => [element.textContent, ...element.children.map(renderedText)].join(" ");
  return {
    providers,
    status,
    text: () => renderedText(providers),
    async complete(success) {
      listeners.get("usage-refresh-completed")?.({payload: success});
      await flushTasks();
    },
    async tick(delay, advanceSeconds = 0) {
      clock += advanceSeconds * 1000;
      intervals.get(delay)();
      await flushTasks();
    }
  };
}

test("tray refresh renders changed usage immediately without a polling tick", async (t) => {
  let current = view();
  const app = await startApp(t, {read: async () => current});
  assert.match(app.text(), /39% LEFT/);
  current = view(62, NOW - 1);
  await app.complete(true);
  assert.match(app.text(), /38% LEFT/);
  assert.equal(app.status.textContent, "LOCAL DATA CHECKED");
});

test("refresh during initial loading discards the older in-flight view", async (t) => {
  const pending = [];
  const app = await startApp(t, {read: () => new Promise((resolve) => pending.push(resolve))});
  await app.complete(true);
  pending.shift()(view(20));
  await flushTasks();
  assert.doesNotMatch(app.text(), /80% LEFT/);
  assert.equal(pending.length, 1);
  pending.shift()(view(62));
  await flushTasks();
  assert.match(app.text(), /38% LEFT/);
});

test("refresh during polling discards the older in-flight view", async (t) => {
  const pending = [];
  let initial = true;
  const app = await startApp(t, {read: () => {
    if (initial) { initial = false; return Promise.resolve(view()); }
    return new Promise((resolve) => pending.push(resolve));
  }});
  await app.tick(5000);
  await app.complete(true);
  pending.shift()(view(20));
  await flushTasks();
  assert.doesNotMatch(app.text(), /80% LEFT/);
  assert.equal(pending.length, 1);
  pending.shift()(view(62));
  await flushTasks();
  assert.match(app.text(), /38% LEFT/);
});

test("a queued refresh read failure reports failure and preserves current usage", async (t) => {
  const pending = [];
  let initial = true;
  const app = await startApp(t, {read: () => {
    if (initial) { initial = false; return Promise.resolve(view()); }
    return new Promise((resolve, reject) => pending.push({resolve, reject}));
  }});
  const previous = app.text();
  await app.tick(5000);
  await app.complete(true);
  pending.shift().resolve(view(20));
  await flushTasks();
  pending.shift().reject(new Error("latest read failed"));
  await flushTasks();

  assert.equal(app.text(), previous);
  assert.equal(app.status.textContent, "REFRESH FAILED");
  assert.equal(app.status.dataset.failed, "true");
});

test("an unchanged successful scan preserves the observation time as its age increases", async (t) => {
  const app = await startApp(t);
  await app.complete(true);
  assert.match(app.text(), /39% LEFT/);
  assert.match(app.text(), /OBSERVED 12S AGO/);
  assert.equal(app.providers.querySelectorAll("[data-observed-at]")[0].dataset.observedAt, String(NOW - 12));
  await app.tick(1000, 1);
  assert.match(app.text(), /OBSERVED 13S AGO/);
  assert.equal(app.status.textContent, "LOCAL DATA CHECKED");
});

test("failed refresh preserves current usage and reports failure", async (t) => {
  const app = await startApp(t);
  const previous = app.text();
  await app.complete(false);
  assert.equal(app.text(), previous);
  assert.equal(app.status.textContent, "REFRESH FAILED");
});

test("listener failure leaves initial loading and periodic updates working", async (t) => {
  let current = view();
  const app = await startApp(t, {read: async () => current, listenFails: true});
  assert.match(app.text(), /39% LEFT/);
  current = view(62);
  await app.tick(5000);
  assert.match(app.text(), /38% LEFT/);
});
