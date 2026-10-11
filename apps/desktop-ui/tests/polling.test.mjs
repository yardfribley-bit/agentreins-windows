import assert from "node:assert/strict";
import test from "node:test";
import { startPolling } from "../src/polling.ts";

const setup = (task, initiallyVisible = true) => {
  const timers = new Map();
  let nextId = 0;
  let visible = initiallyVisible;
  const errors = [];
  const polling = startPolling(task, {
    isVisible: () => visible,
    onError: error => errors.push(error),
    clock: {
      setTimeout: (callback, delay) => { const id = ++nextId; timers.set(id, { callback, delay }); return id; },
      clearTimeout: id => timers.delete(id),
    },
  });
  return {
    polling, timers, errors,
    visible: value => { visible = value; },
    tick: async () => {
      const [id, timer] = timers.entries().next().value;
      timers.delete(id);
      timer.callback();
      await Promise.resolve();
      await Promise.resolve();
    },
    delay: () => [...timers.values()][0]?.delay,
  };
};

test("slow refreshes never queue overlapping requests", async () => {
  let finish;
  let calls = 0;
  const fixture = setup(() => { calls++; return new Promise(resolve => { finish = resolve; }); });
  await fixture.tick();
  assert.equal(calls, 1);
  assert.equal(fixture.timers.size, 0);
  fixture.polling.wake();
  assert.equal(fixture.timers.size, 0);
  finish();
  await Promise.resolve();
  assert.equal(fixture.delay(), 5000);
  fixture.polling.stop();
});

test("hidden windows skip refresh work and resume when visible", async () => {
  let calls = 0;
  const fixture = setup(async () => { calls++; }, false);
  assert.equal(fixture.delay(), 30000);
  await fixture.tick();
  assert.equal(calls, 0);
  fixture.visible(true);
  fixture.polling.wake();
  assert.equal(fixture.delay(), 0);
  await fixture.tick();
  assert.equal(calls, 1);
  assert.equal(fixture.delay(), 5000);
  fixture.polling.stop();
});

test("stopping during a request prevents all future work", async () => {
  let finish;
  const fixture = setup(() => new Promise(resolve => { finish = resolve; }));
  await fixture.tick();
  fixture.polling.stop();
  finish();
  await Promise.resolve();
  fixture.polling.wake();
  assert.equal(fixture.timers.size, 0);
});

test("failure is reported and the next refresh stays scheduled", async () => {
  const error = new Error("offline");
  const fixture = setup(async () => { throw error; });
  await fixture.tick();
  assert.deepEqual(fixture.errors, [error]);
  assert.equal(fixture.delay(), 5000);
  fixture.polling.stop();
});

test("repeated visibility events leave only one scheduled refresh", () => {
  const fixture = setup(async () => {});
  fixture.polling.wake();
  fixture.polling.wake();
  assert.equal(fixture.timers.size, 1);
  fixture.visible(false);
  fixture.polling.wake();
  assert.equal(fixture.delay(), 30000);
  fixture.polling.stop();
  assert.equal(fixture.timers.size, 0);
});
