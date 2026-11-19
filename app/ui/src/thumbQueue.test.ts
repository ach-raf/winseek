import assert from "node:assert/strict";
import { test } from "node:test";
import { ThumbQueue, bumpThumbs, iconUrlPath, requestThumb, subscribeThumbVer, thumbVerSubscribedPaths, thumbVersion, withThumbVer } from "./thumbQueue.ts";

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

test("a hold keeps new requests back until released; newest then goes first", () => {
  const r = rig(2);
  r.q.setHold(true);
  r.q.request("a", () => {});
  r.q.request("b", () => {});
  assert.equal(r.started.length, 0);
  r.q.setHold(false);
  assert.deepEqual(r.started, ["b", "a"]);
});

test("a hold does not stop requests already in flight, and a withdrawn waiting one never starts", () => {
  const r = rig(1);
  r.q.request("a", () => {});
  r.q.setHold(true);
  const cancelB = r.q.request("b", () => {});
  r.ends.get("a")!(true);
  assert.deepEqual(r.started, ["a"]);
  cancelB();
  r.q.setHold(false);
  assert.deepEqual(r.started, ["a"]);
});

test("a fast scroll holds, a slow one releases", () => {
  const r = rig(2);
  r.q.noteScroll(0, 1000);
  r.q.noteScroll(100, 1016); // 6 px/ms
  assert.equal(r.q.isHeld, true);
  r.q.noteScroll(104, 1032); // 0.25 px/ms
  assert.equal(r.q.isHeld, false);
  r.q.noteScroll(5000, 1048);
  r.q.setHold(false); // clears the idle timer's work for the test
});

test("the waiting list is bounded: the oldest are dropped", () => {
  const r = rig(1, 3);
  for (const u of ["a", "b", "c", "d", "e", "f"]) r.q.request(u, () => {});
  assert.equal(r.q.pending, 4); // a active + c, d... newest 3 waiting
  r.ends.get("a")!(true);
  assert.equal(r.started[1], "f");
});

test("forget withdraws waiting requests and cached urls of gone files", () => {
  const r = rig(1);
  r.q.request("http://wsicon.localhost/?p=C%3A%5Ckeep.jpg&s=256", () => {});
  r.ends.get("http://wsicon.localhost/?p=C%3A%5Ckeep.jpg&s=256")!(true);
  assert.ok(r.q.has("http://wsicon.localhost/?p=C%3A%5Ckeep.jpg&s=256"));
  r.q.request("u-busy", () => {});
  r.q.request("http://wsicon.localhost/?p=C%3A%5Cgone.jpg&s=256", () => {});
  r.q.forget((u) => iconUrlPath(u) === "C:\\gone.jpg" || iconUrlPath(u) === "C:\\keep.jpg");
  assert.equal(r.q.pending, 1, "only the in-flight load is left");
  assert.ok(!r.q.has("http://wsicon.localhost/?p=C%3A%5Ckeep.jpg&s=256"));
});

test("thumb re-reads: a changed file asks under a new url (r=); other urls stay plain", () => {
  assert.equal(thumbVersion("C:\\x.jpg"), 0);
  bumpThumbs(["c:/X.JPG"]);
  const v = thumbVersion("C:\\x.jpg");
  assert.ok(v > 0);
  assert.equal(withThumbVer("http://wsicon.localhost/?p=a&s=1&v=99", v), `http://wsicon.localhost/?p=a&s=1&v=99&r=${v}`);
  assert.equal(withThumbVer("data:image/png;base64,AA", v), "data:image/png;base64,AA");
  assert.equal(iconUrlPath("blob:abc"), null);
});

test("version subscriptions are per path: a bump wakes only that file's tiles", () => {
  let a = 0, b = 0;
  const offA = subscribeThumbVer("C:\\pics\\a.jpg", () => a++);
  const offB = subscribeThumbVer("C:\\pics\\b.jpg", () => b++);
  const offA2 = subscribeThumbVer("c:/PICS/A.JPG", () => a++); // same file, other spelling
  bumpThumbs(["C:\\pics\\a.jpg"]);
  assert.equal(a, 2);
  assert.equal(b, 0);
  bumpThumbs(["C:\\elsewhere\\z.jpg"]);
  assert.equal(a + b, 2, "a file nobody shows wakes nobody");
  offA(); offA2(); offB();
  assert.equal(thumbVerSubscribedPaths(), 0, "unsubscribing drops the path");
});

/** Two lanes the test finishes by hand: the placeholder (fast, never held) and the sharp queue. */
function lanes() {
  const sharp = rig(4);
  const low = rig(8);
  return { sharp, low };
}

test("tiers: the placeholder is asked first, the sharp picture only after it landed", () => {
  const { sharp, low } = lanes();
  const got: string[] = [];
  requestThumb("s1", "l1", () => got.push("low"), (ok) => got.push(ok ? "sharp" : "bad"), sharp.q, low.q);
  assert.deepEqual(low.started, ["l1"]);
  assert.deepEqual(sharp.started, [], "sharp waits for the placeholder");
  low.ends.get("l1")!(true);
  assert.deepEqual(sharp.started, ["s1"]);
  sharp.ends.get("s1")!(true);
  assert.deepEqual(got, ["low", "sharp"]);
});

test("tiers: a fling holds the sharp lane only; placeholders keep coming", () => {
  const { sharp, low } = lanes();
  sharp.q.setHold(true);
  for (const i of [1, 2, 3]) requestThumb(`s${i}`, `l${i}`, () => {}, () => {}, sharp.q, low.q);
  assert.equal(low.started.length, 3, "all placeholders start during the fling");
  for (const i of [1, 2, 3]) low.ends.get(`l${i}`)!(true);
  assert.equal(sharp.started.length, 0, "no sharp work while held");
  sharp.q.setHold(false);
  assert.deepEqual(sharp.started, ["s3", "s2", "s1"], "released: newest first");
});

test("tiers: a sharp picture already loaded skips the placeholder; cancel withdraws both", () => {
  const { sharp, low } = lanes();
  requestThumb("s1", null, () => {}, () => {}, sharp.q, low.q);
  sharp.ends.get("s1")!(true);
  let lowCalls = 0;
  requestThumb("s1", "l1", () => lowCalls++, () => {}, sharp.q, low.q);
  assert.equal(low.started.length, 0);
  assert.equal(lowCalls, 0);
  const cancel = requestThumb("s2", "l2", () => {}, () => {}, sharp.q, low.q);
  cancel();
  assert.deepEqual(low.aborted, ["l2"], "the in-flight placeholder is aborted");
  assert.equal(sharp.q.pending, 0, "and the sharp one never queued");
});

test("tiers: the placeholder lane is capped at 8 in flight, the sharp lane at 4", () => {
  const { sharp, low } = lanes();
  for (let i = 0; i < 12; i++) requestThumb(`s${i}`, `l${i}`, () => {}, () => {}, sharp.q, low.q);
  assert.equal(low.started.length, 8);
  for (let i = 0; i < 12; i++) low.ends.get(`l${i}`)?.(true);
  assert.equal(sharp.started.length, 4);
});