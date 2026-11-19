import assert from "node:assert/strict";
import { test } from "node:test";
import { EMIT_MS, PIC_SEARCH_CAP, PIC_SEARCH_PAGE, readPictureNames, type NamesState } from "./pictureSearch.ts";
import { buildOverview } from "./overviewLogic.ts";
import { kindOf } from "./format.ts";
import type { Hit, SemanticHit } from "./types";

const isImage = (n: string) => kindOf(n, false) === "image";
const name = (i: number) => `cat ${String(i).padStart(6, "0")}.jpg`;
const hit = (i: number): Hit => ({ name: name(i), path: `C:\\Pictures\\${name(i)}`, is_dir: false, size: 1000 + i, mtime: 1_600_000_000_000 + i });

/** A name index with `total` pictures; `clock` advances `pageMs` per answered page. */
function fakeIndex(total: number, pageMs = 8) {
  const calls: { offset: number; limit: number }[] = [];
  const clock = { t: 0 };
  const backend = {
    search: async ({ offset, limit }: { offset: number; limit: number }) => {
      calls.push({ offset, limit });
      clock.t += pageMs;
      const hits: Hit[] = [];
      for (let i = offset; i < Math.min(total, offset + limit); i++) hits.push(hit(i));
      return { total, hits, elapsed_us: 0 };
    },
  };
  return { backend: backend as never, calls, clock };
}

test("names are read in pages of 400, capped at 20k, handed on once per burst", async () => {
  const f = fakeIndex(50_000);
  const acc = { hits: [] as Hit[], total: -1 };
  const emits: NamesState[] = [];
  await readPictureNames(f.backend, { query: "ws:cat", stream: "pics", acc, limit: 1e9, live: () => true, emit: (s) => emits.push(s), now: () => f.clock.t });
  assert.equal(acc.hits.length, PIC_SEARCH_CAP);
  assert.ok(f.calls.every((c) => c.limit <= PIC_SEARCH_PAGE));
  assert.equal(f.calls.length, PIC_SEARCH_CAP / PIC_SEARCH_PAGE);
  // 50 pages at 8 ms: the first, one every EMIT_MS, and the last — not 50
  assert.ok(emits.length <= 2 + Math.ceil((50 * 8) / EMIT_MS), `emits: ${emits.length}`);
  assert.equal(emits.at(-1)!.done, true);
  assert.equal(emits.at(-1)!.complete, true, "the cap counts as the end");
  assert.equal(emits.at(-1)!.total, 50_000, "the count still says how many there are");
});

test("a raised limit appends; nothing is re-read; a superseded read emits nothing", async () => {
  const f = fakeIndex(3000);
  const acc = { hits: [] as Hit[], total: -1 };
  await readPictureNames(f.backend, { query: "q", stream: "pics", acc, limit: 1200, live: () => true, emit: () => {} });
  assert.equal(acc.hits.length, 1200);
  await readPictureNames(f.backend, { query: "q", stream: "pics", acc, limit: 2400, live: () => true, emit: () => {} });
  assert.equal(acc.hits.length, 2400);
  assert.deepEqual(f.calls.map((c) => c.offset), [0, 400, 800, 1200, 1600, 2000]);
  let n = 0;
  await readPictureNames(f.backend, { query: "q", stream: "pics", acc, limit: 2400, live: () => true, emit: (s) => { n++; assert.equal(s.total, 3000); } });
  assert.equal(n, 1, "at the limit: one hand-off with the known total, no request");
  assert.equal(f.calls.length, 6);
  const dead = { hits: [] as Hit[], total: -1 };
  let got = 0;
  await readPictureNames(f.backend, { query: "q", stream: "pics", acc: dead, limit: 800, live: () => false, emit: () => got++ });
  assert.equal(got, 0);
});

test("cost on 20k pictures: one overview build per burst instead of one per page", async (t) => {
  const N = 20_000;
  const sem: SemanticHit[] = [];
  for (let i = 0; i < 1000; i++) sem.push({ path: `C:\\Pictures\\${name(i * 7)}`, score: 1 - i / 1000, rank: 9 - i / 200, source: "meaning", size: 1, mtime: 1 } as SemanticHit);
  const all = Array.from({ length: N }, (_, i) => hit(i));
  const build = (k: number) => buildOverview({
    names: [], nameTotal: 0, sem, text: "cat", isImage, picNames: all.slice(0, k), picTotal: N, picNameTotal: N, expand: "images",
  });
  const time = (sizes: number[]) => {
    const t0 = performance.now();
    for (const k of sizes) build(k);
    return performance.now() - t0;
  };
  build(400); // warm the JIT
  // per page (pages of 400, a build after each): 50 builds over a growing list
  const perPage = Array.from({ length: N / PIC_SEARCH_PAGE }, (_, p) => (p + 1) * PIC_SEARCH_PAGE);
  // the previous popup (pages of 5000, a build after each)
  const old = [5000, 10_000, 15_000, 20_000];
  // now: the sizes the reader actually hands on (8 ms per page)
  const f = fakeIndex(N);
  const acc = { hits: [] as Hit[], total: -1 };
  const burst: number[] = [];
  await readPictureNames(f.backend, { query: "ws:cat", stream: "pics", acc, limit: N, live: () => true, emit: () => burst.push(acc.hits.length), now: () => f.clock.t });
  const msPerPage = time(perPage), msOld = time(old), msBurst = time(burst);
  const line = `20k pictures + 1k meaning hits: per-400-page ${perPage.length} builds ${msPerPage.toFixed(1)} ms; old 5000-page ${old.length} builds ${msOld.toFixed(1)} ms (first paint after a 5000-row page); now ${burst.length} builds ${msBurst.toFixed(1)} ms (first paint after 400 rows)`;
  t.diagnostic(line);
  console.log(line);
  assert.ok(burst.length < perPage.length / 4, `builds: ${burst.length}`);
  assert.ok(msBurst < msPerPage, "a burst is cheaper than per-page rebuilding");
});
