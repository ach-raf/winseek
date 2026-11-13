import assert from "node:assert/strict";
import { test } from "node:test";
import { ThumbQueue } from "./thumbQueue.ts";

/** A loader the test finishes by hand. */
function rig(maxActive = 2, maxWaiting = 100) {
  const started: string[] = [];
  const aborted: string[] = [];
  const ends = new Map<string, (ok: boolean) => void>();
  const q = new ThumbQueue((url, done) => {
    started.push(url);
    ends.set(url, done);
    return () => aborted.push(url);
  }, maxActive, maxWaiting);
  return { q, started, aborted, ends };
}

test("never more than maxActive in flight; the rest wait", () => {
  const r = rig(2);
  for (const u of ["a", "b", "c", "d"]) r.q.request(u, () => {});
  assert.equal(r.started.length, 2);
  assert.equal(r.q.pending, 4);
});

test("newest request goes first", () => {
  const r = rig(1);
  r.q.request("a", () => {});
  r.q.request("b", () => {});
  r.q.request("c", () => {});
  assert.deepEqual(r.started, ["a"]);
  r.ends.get("a")!(true);
  assert.deepEqual(r.started, ["a", "c"]);
  r.ends.get("c")!(true);
  assert.deepEqual(r.started, ["a", "c", "b"]);
});

test("cancelling a waiting request drops it", () => {
  const r = rig(1);
  r.q.request("a", () => {});
  const cancelB = r.q.request("b", () => {});
  cancelB();
  assert.equal(r.q.pending, 1);
  r.ends.get("a")!(true);
  assert.deepEqual(r.started, ["a"]);
});

test("an aborted in-flight request frees its slot", () => {
  const r = rig(1);
  const cancelA = r.q.request("a", () => {});
  r.q.request("b", () => {});
  cancelA();
  assert.deepEqual(r.aborted, ["a"]);
  assert.deepEqual(r.started, ["a", "b"]);
});

test("loaded urls resolve at once next time; failures are not remembered", () => {
  const r = rig(1);
  let got: boolean | null = null;
  r.q.request("a", () => {});
  r.ends.get("a")!(true);
  r.q.request("a", (ok) => { got = ok; });
  assert.equal(got, true);
  assert.equal(r.started.length, 1);
  r.q.request("bad", () => {});
  r.ends.get("bad")!(false);
  assert.equal(r.q.has("bad"), false);
});

test("the waiting list is bounded: the oldest are dropped", () => {
  const r = rig(1, 3);
  for (const u of ["a", "b", "c", "d", "e", "f"]) r.q.request(u, () => {});
  assert.equal(r.q.pending, 4); // a active + c, d... newest 3 waiting
  r.ends.get("a")!(true);
  assert.equal(r.started[1], "f");
});
