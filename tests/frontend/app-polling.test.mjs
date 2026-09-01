import test from "node:test";
import assert from "node:assert/strict";

const flushTasks = () => new Promise((resolve) => setImmediate(resolve));

test("the five-second app poll reads the cached view without forcing a full refresh", async (t) => {
  const originalGlobals = new Map([
    ["document", globalThis.document],
    ["window", globalThis.window],
    ["requestAnimationFrame", globalThis.requestAnimationFrame],
    ["setInterval", globalThis.setInterval]
  ]);
  t.after(() => {
    for (const [name, value] of originalGlobals) {
      if (value === undefined) delete globalThis[name];
      else globalThis[name] = value;
    }
  });

  class FakeElement {
    constructor() {
      this.dataset = {};
      this.listeners = new Map();
    }

    addEventListener(type, listener) {
      this.listeners.set(type, listener);
    }

    replaceChildren() {}
    querySelectorAll() { return []; }
    getBoundingClientRect() { return {height: 100}; }
  }

  const providers = new FakeElement();
  const hideButton = new FakeElement();
  const body = new FakeElement();
  const intervals = [];
  const invokedCommands = [];

  globalThis.document = {
    body,
    querySelector(selector) {
      if (selector === "#providers") return providers;
      if (selector === "#hide-widget") return hideButton;
      return null;
    },
    createElement() { return new FakeElement(); },
    addEventListener() {}
  };
  globalThis.window = {
    __TAURI__: {
      core: {
        invoke(command) {
          invokedCommands.push(command);
          if (command === "get_widget_view" || command === "refresh") {
            return Promise.resolve({providers: []});
          }
          return Promise.resolve();
        }
      }
    },
    addEventListener() {}
  };
  globalThis.requestAnimationFrame = (callback) => {
    callback();
    return 1;
  };
  globalThis.setInterval = (callback, delay) => {
    intervals.push({callback, delay});
    return intervals.length;
  };

  await import(`../../ui/app.js?poll-test=${Date.now()}`);
  await flushTasks();
  await flushTasks();
  invokedCommands.length = 0;

  const viewPoll = intervals.find(({delay}) => delay === 5_000);
  assert.ok(viewPoll, "the app should register its five-second view poll");
  viewPoll.callback();
  await flushTasks();
  await flushTasks();

  assert.deepEqual(
    invokedCommands.filter((command) => command === "get_widget_view" || command === "refresh"),
    ["get_widget_view"]
  );
});
