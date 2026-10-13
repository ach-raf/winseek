import { test } from "node:test";
import assert from "node:assert/strict";
import { clampPage, clampView, fitScale, highlight, pdfRenderWidth, previewTerms, toggleActual, zoomAt } from "./previewLogic.ts";

test("clampPage keeps a PDF page inside the document", () => {
  assert.equal(clampPage(3, 10), 3);
  assert.equal(clampPage(0, 10), 1);
  assert.equal(clampPage(-4, 10), 1);
  assert.equal(clampPage(11, 10), 10);
  assert.equal(clampPage(2.9, 10), 2);
  assert.equal(clampPage(5, null), 5);
  assert.equal(clampPage(0, null), 1);
  assert.equal(clampPage(Number.NaN, 7), 1);
  assert.equal(clampPage(9, 0), 9, "an unknown (zero) count does not lock the page to 1");
});

test("pdfRenderWidth rounds up to 100 px and stays within 400..1600", () => {
  assert.equal(pdfRenderWidth(560, 1), 600);
  assert.equal(pdfRenderWidth(560, 1.5), 900);
  assert.equal(pdfRenderWidth(100, 1), 400);
  assert.equal(pdfRenderWidth(0, 1), 400);
  assert.equal(pdfRenderWidth(2000, 2), 1600);
  assert.equal(pdfRenderWidth(500, 0.5), 500, "dpr below 1 is treated as 1");
});

test("previewTerms drops operators, negations, wildcards", () => {
  assert.deepEqual(previewTerms('invoice "due date" !draft ext:pdf *.log Invoice a'), ["invoice", "due", "date", ".log"]);
  assert.deepEqual(previewTerms("  "), []);
});

test("highlight marks case-insensitive matches and keeps the text intact", () => {
  const { segments, hits } = highlight("Invoice due. invoice DUE", ["invoice", "due"]);
  assert.equal(hits, 4);
  assert.equal(segments.map((s) => s.text).join(""), "Invoice due. invoice DUE");
  assert.deepEqual(segments.filter((s) => s.hit).map((s) => s.text), ["Invoice", "due", "invoice", "DUE"]);
  assert.deepEqual(highlight("a.b", ["a.b"]).segments, [{ text: "a.b", hit: true }]); // regex chars are escaped
  assert.equal(highlight("x".repeat(10), ["x"], 3).hits, 3);
  assert.deepEqual(highlight("text", []).segments, [{ text: "text", hit: false }]);
});

test("fit never enlarges, handles empty sizes", () => {
  assert.equal(fitScale(400, 300, 800, 600), 0.5);
  assert.equal(fitScale(400, 300, 100, 50), 1);
  assert.equal(fitScale(400, 300, 0, 0), 1);
});

test("zoomAt keeps the point under the cursor fixed and respects limits", () => {
  const [cw, ch, iw, ih] = [400, 300, 800, 600];
  const fit = fitScale(cw, ch, iw, ih); // 0.5
  const v0 = { s: fit, tx: 0, ty: 0 };
  // can't zoom out below fit
  assert.equal(zoomAt(v0, 0.5, 0, 0, fit, cw, ch, iw, ih).s, fit);
  // zoom in at the stage centre: stays centred
  const c = zoomAt(v0, 2, 0, 0, fit, cw, ch, iw, ih);
  assert.deepEqual([c.s, c.tx, c.ty], [1, 0, 0]);
  // zoom 2x about (100, 50): image point under the cursor stays under it
  const z = zoomAt({ s: 1, tx: 0, ty: 0 }, 1.5, 100, 50, fit, cw, ch, iw, ih);
  assert.equal(z.s, 1.5);
  assert.ok(Math.abs((100 - z.tx) / z.s - 100) < 1e-9 && Math.abs((50 - z.ty) / z.s - 50) < 1e-9);
  assert.equal(zoomAt(v0, 1e9, 0, 0, fit, cw, ch, iw, ih).s, 32);
});

test("clampView keeps a larger image covering the stage, centres a smaller one", () => {
  assert.deepEqual(clampView({ s: 1, tx: 999, ty: -999 }, 400, 300, 800, 600), { s: 1, tx: 200, ty: -150 });
  assert.deepEqual(clampView({ s: 0.5, tx: 50, ty: 50 }, 400, 300, 800, 600), { s: 0.5, tx: 0, ty: 0 });
});

test("double click toggles fit and 1:1", () => {
  assert.deepEqual(toggleActual({ s: 0.5, tx: 3, ty: 4 }, 0.5), { s: 1, tx: 0, ty: 0 });
  assert.deepEqual(toggleActual({ s: 1, tx: 3, ty: 4 }, 0.5), { s: 0.5, tx: 0, ty: 0 });
  assert.deepEqual(toggleActual({ s: 1, tx: 0, ty: 0 }, 1), { s: 1, tx: 0, ty: 0 });
});
