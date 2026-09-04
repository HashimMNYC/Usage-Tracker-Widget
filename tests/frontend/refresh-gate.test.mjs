import test from "node:test";
import assert from "node:assert/strict";
import { createLatestOnlyRefresh } from "../../ui/ui-model.js";

test("coalesces an overlapping tick and commits only the newer refresh", async () => {
  const resolutions = [];
  const committed = [];
  let calls = 0;
  const gate = createLatestOnlyRefresh(
    () => {
      calls += 1;
      return new Promise((resolve) => resolutions.push(resolve));
    },
    (view) => committed.push(view)
  );

  const firstTick = gate.run();
  const secondTick = gate.run();
  assert.equal(calls, 1);
  assert.deepEqual(committed, []);

  resolutions.shift()({sequence: "stale"});
  await Promise.resolve();
  assert.equal(calls, 2);
  assert.deepEqual(committed, []);

  resolutions.shift()({sequence: "latest"});
  await Promise.all([firstTick, secondTick]);
  assert.deepEqual(committed, [{sequence: "latest"}]);
});

test("accepts a later refresh after a successful request", async () => {
  const committed = [];
  let calls = 0;
  const gate = createLatestOnlyRefresh(
    async () => ({sequence: ++calls}),
    (view) => committed.push(view)
  );

  await gate.run();
  await gate.run();

  assert.equal(calls, 2);
  assert.deepEqual(committed, [{sequence: 1}, {sequence: 2}]);
});

test("recovers for a later refresh after a failed request", async () => {
  const committed = [];
  let calls = 0;
  const gate = createLatestOnlyRefresh(
    async () => {
      calls += 1;
      if (calls === 1) throw new Error("refresh failed");
      return {sequence: calls};
    },
    (view) => committed.push(view)
  );

  await assert.rejects(gate.run(), /refresh failed/);
  await gate.run();

  assert.equal(calls, 2);
  assert.deepEqual(committed, [{sequence: 2}]);
});

test("queued callers wait for and receive the final read failure", async () => {
  const requests = [];
  const committed = [];
  const gate = createLatestOnlyRefresh(
    () => new Promise((resolve, reject) => requests.push({resolve, reject})),
    (view) => committed.push(view)
  );
  const first = gate.run();
  const queued = gate.run();
  let queuedSettled = false;
  void queued.then(() => { queuedSettled = true; }, () => { queuedSettled = true; });
  const outcomes = Promise.allSettled([first, queued]);
  await Promise.resolve();
  const settledBeforeCompletion = queuedSettled;

  requests.shift().resolve({sequence: "stale"});
  await Promise.resolve();
  const failure = new Error("latest read failed");
  requests.shift().reject(failure);
  const results = await outcomes;

  assert.equal(settledBeforeCompletion, false);
  assert.deepEqual(results, [
    {status: "rejected", reason: failure},
    {status: "rejected", reason: failure}
  ]);
  assert.deepEqual(committed, []);
});

test("a queued refresh retries after an older read fails", async () => {
  const requests = [];
  const committed = [];
  const gate = createLatestOnlyRefresh(
    () => new Promise((resolve, reject) => requests.push({resolve, reject})),
    (view) => committed.push(view)
  );
  const first = gate.run();
  const queued = gate.run();
  const outcomes = Promise.all([first, queued]);
  requests.shift().reject(new Error("older read failed"));
  await Promise.resolve();
  requests.shift().resolve({sequence: "latest"});
  await outcomes;

  assert.deepEqual(committed, [{sequence: "latest"}]);
});
