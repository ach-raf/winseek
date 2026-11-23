import { test } from "node:test";
import assert from "node:assert/strict";
import { CHUNK, MAX_CHUNKS, chunksToEvict, compress, wantedChunks } from "./sparseGrid.ts";

test("wanted chunks: what is within one viewport of the view, nearest first, inside the list", () => {
  // 40 positions on screen at 600,000 of a million: 599,960..600,080 is wanted, chunks 1199 and 1200.
  assert.deepEqual(wantedChunks(600_000, 600_040, 1_000_000), [1200, 1199]);
  // The top of the list asks for the first chunk only; the end never past the last chunk.
  assert.deepEqual(wantedChunks(0, 40, 1_000_000), [0]);
  assert.deepEqual(wantedChunks(999_980, 1_000_000, 1_000_000), [1999]);
  // A view across a chunk border wants both, the one holding its middle first.
  assert.deepEqual(wantedChunks(CHUNK - 10, CHUNK + 30, 1_000_000), [1, 0]);
  // Nothing on screen or nothing in the list: nothing to read.
  assert.deepEqual(wantedChunks(5, 5, 1_000_000), []);
  assert.deepEqual(wantedChunks(0, 40, 0), []);
  // A jump never wants what lies between: the chunks wanted are a handful, whatever the list's length.
  assert.ok(wantedChunks(10_000_000, 10_000_200, 20_000_000).length <= 3);
});

test("eviction: past the cap the chunks farthest from the view go first; wanted ones stay", () => {
  const held = Array.from({ length: MAX_CHUNKS + 3 }, (_, i) => i * 10); // 0, 10, ..., 420
  const at = 200 * CHUNK; // the view sits in chunk 200
  const out = chunksToEvict(held, at, new Set([200]));
  assert.equal(out.length, 3);
  assert.deepEqual(out, [420, 410, 400], "the farthest three, farthest first");
  assert.deepEqual(chunksToEvict(held.slice(0, MAX_CHUNKS), at, new Set()), [], "at the cap nothing goes");
  // A wanted chunk is never let go, even when it is the farthest.
  assert.ok(!chunksToEvict(held, at, new Set([420])).includes(420));
});

test("compressed height: 1:1 at both ends, by fraction between, continuous, never past the list", () => {
  const full = 40_000_000, viewH = 400, edge = 900, max = 30_000_000;
  assert.deepEqual(compress(500, 1000, viewH, edge, max), { y: 500, h: 1000 }, "a list that fits is drawn as it is");
  assert.equal(compress(0, full, viewH, edge, max).y, 0);
  assert.equal(compress(edge, full, viewH, edge, max).y, edge);
  assert.equal(compress(max - viewH, full, viewH, edge, max).y, full - viewH, "the end shows the last rows");
  const mid = compress((max - viewH) / 2, full, viewH, edge, max).y;
  assert.ok(Math.abs(mid - (full - viewH) / 2) < 1, "the middle of the scrollbar is the middle of the list");
  for (const r of [edge, max - viewH - edge]) {
    assert.ok(Math.abs(compress(r - 0.001, full, viewH, edge, max).y - compress(r + 0.001, full, viewH, edge, max).y) < 0.01, `continuous at ${r}`);
  }
  assert.equal(compress(1, full, viewH, edge, max).h, max);
});
