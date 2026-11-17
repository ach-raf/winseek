import { test } from "node:test";
import assert from "node:assert/strict";
import { NO_HITS, runSemantic, semPlaceholder, semPlan, semView, type SemanticState } from "./semanticLogic.ts";
import { kindScope, parseQuery } from "./filters.ts";
import type { SemanticHit } from "./types";

const KINDS = ["all", "images", "docs", "video", "audio"] as const;
const ext = (k: string) => ({ all: "", images: "jpg", docs: "pdf", video: "mp4", audio: "mp3" })[k];

/** A fake backend with the real contract: a top-k is the best k over EVERYTHING; the full list is every relevant file. */
function fakeBackend() {
  const everything: { path: string; kind: string }[] = [];
  // 30 strong documents outrank the 5 relevant songs, as in the real index.
  for (let i = 0; i < 30; i++) everything.push({ path: `d${i}.pdf`, kind: "docs" });
  for (let i = 0; i < 5; i++) everything.push({ path: `s${i}.mp3`, kind: "audio" });
  const calls: [string, number, number | undefined][] = [];
  return {
    calls,
    semanticSearch: async (q: string, k: number, offset?: number) => {
      calls.push([q, k, offset]);
      const list = k > 200 ? everything : everything.slice(0, k);
      const page = list.slice(offset ?? 0, (offset ?? 0) + k);
      return { state: "ready", hits: page as unknown as SemanticHit[], total: list.length, more: k <= 200 && everything.length > k };
    },
  };
}

/** What the UI shows for (typed text, chips on/off, tab): the hook's request + the client-side kind filter. */
async function results(b: ReturnType<typeof fakeBackend>, text: string, off: string[], tab: (typeof KINDS)[number]) {
  const p = parseQuery(text, { off: new Set(off) });
  const eff = kindScope(p.kind, tab);
  const plan = semPlan(p.text.trim(), eff, false, false);
  let st: SemanticState = NO_HITS;
  await runSemantic(b, p.text.trim(), plan, () => true, (s) => (st = s));
  const shown = semView(st, p.text.trim(), plan);
  return shown.hits.map((h) => (h as unknown as { path: string; kind: string })).filter((h) => eff === "all" || h.kind === ({ images: "images", docs: "docs", video: "video", audio: "audio" } as Record<string, string>)[eff]).map((h) => h.path);
}

test("a kind scope reads the whole relevant list, not a top-k that other kinds fill", async () => {
  const b = fakeBackend();
  const songs = await results(b, "music about addiction", [], "all");
  assert.equal(songs.length, 5, "audio chip must show every relevant song");
  const plan = semPlan("x", "audio", false, false);
  assert.ok(plan.paged);
});

test("chip remove / re-add / tab switch cycle is order independent", async () => {
  const b = fakeBackend();
  const q = "music about addiction";
  const chipKeys = parseQuery(q, {}).chips.map((c) => c.key);
  const seqs: [string[], (typeof KINDS)[number]][] = [
    [[], "all"], [chipKeys, "all"], [[], "all"], [chipKeys, "audio"], [[], "audio"], [chipKeys, "all"], [[], "all"],
  ];
  const first = new Map<string, string[]>();
  for (const [off, tab] of seqs) {
    const r = await results(b, q, off, tab);
    const id = `${off.length}|${tab}`;
    if (first.has(id)) assert.deepEqual(r, first.get(id), `state ${id} differs from its first showing`);
    else first.set(id, r);
  }
  // with the chip, audio shows the 5 songs on every tab; without it, "music about addiction" on All is the same top-k each time
  assert.equal(first.get("0|all")!.length, 5);
  assert.equal(first.get("1|all")!.length, 24);
});

test("every tab gets a hit set that depends only on (query, tab)", async () => {
  for (const tab of KINDS) {
    const a = await results(fakeBackend(), "addiction", [], tab);
    const b2 = fakeBackend();
    await results(b2, "something else", [], "audio");
    const c = await results(b2, "addiction", [], tab);
    assert.deepEqual(a, c, tab);
  }
  void ext;
});

test("stale hits of another scope are never shown", () => {
  const st: SemanticState = { query: "addiction", key: semPlan("addiction", "all", false, false).key, hits: [{ path: "a" } as SemanticHit], more: false, loading: false };
  const next = semPlan("addiction", "audio", false, false);
  assert.equal(semView(st, "addiction", next).hits.length, 0);
  assert.equal(semView(st, "addiction", semPlan("addiction", "all", false, false)).hits.length, 1);
  // typing on: the previous query's hits of the same scope stay until the new ones arrive
  assert.equal(semView(st, "addictions", semPlan("addictions", "all", false, false)).hits.length, 1);
});

test("a superseded run emits nothing; a failed one answers empty for its own plan", async () => {
  const b = fakeBackend();
  const out: SemanticState[] = [];
  await runSemantic(b, "q1", semPlan("q1", "audio", false, false), () => false, (s) => out.push(s));
  assert.equal(out.length, 0);
  const bad = { semanticSearch: async () => { throw new Error("x"); } };
  const plan = semPlan("q1", "audio", false, false);
  await runSemantic(bad, "q1", plan, () => true, (s) => out.push(s));
  assert.equal(out[0]?.key, plan.key);
  assert.equal(out[0]?.hits.length, 0);
});

test("semPlaceholder: typing keeps the previous hits, a scope change never does", () => {
  const prevPlan = semPlan("addiction", "all", false, false);
  const prev: SemanticState = { query: "addiction", key: prevPlan.key, hits: [{ path: "a" } as SemanticHit], more: false, loading: false };
  const nextPlan = semPlan("addictions", "all", false, false);
  // typing on within the same paged-ness: the previous answer stays on screen while the new computes
  assert.equal(semPlaceholder(prev, prevPlan.key, "addictions", nextPlan), prev);
  // a scope change with the same text: pending, never another scope's hits
  const scopedPlan = semPlan("addiction", "audio", false, false);
  assert.equal(semPlaceholder(prev, prevPlan.key, "addiction", scopedPlan), undefined);
  // a different paged-ness is never held over (a top-k must not fill a full-list shape)
  const pagedPlan = semPlan("addictions", "all", false, true); // showAll: the full list shape
  assert.equal(semPlaceholder(prev, prevPlan.key, "addictions", pagedPlan), undefined);
  // nothing previous: no placeholder
  assert.equal(semPlaceholder(undefined, prevPlan.key, "addictions", nextPlan), undefined);
  assert.equal(semPlaceholder(prev, undefined, "addictions", nextPlan), undefined);
});
