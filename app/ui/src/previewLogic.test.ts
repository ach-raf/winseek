import { test } from "node:test";
import assert from "node:assert/strict";
import { boxesForView, boxRect, clampPage, clampView, cleanBoxes, fitScale, highlight, pdfRenderWidth, previewTerms, tableLabel, toggleActual, zoomAt } from "./previewLogic.ts";

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

test("cleanBoxes drops unusable boxes and clamps the rest into the image", () => {
  assert.deepEqual(cleanBoxes(null), []);
  assert.deepEqual(cleanBoxes(undefined), []);
  assert.deepEqual(cleanBoxes([{ x: 0.1, y: 0.2, w: 0.3, h: 0.1 }]), [{ x: 0.1, y: 0.2, w: 0.3, h: 0.1 }]);
  assert.deepEqual(cleanBoxes([{ x: Number.NaN, y: 0, w: 0.1, h: 0.1 }, { x: 0, y: 0, w: Infinity, h: 0.1 }]), []);
  assert.deepEqual(cleanBoxes([{ x: 0.5, y: 0.5, w: 0, h: 0.2 }, { x: 0.5, y: 0.5, w: 0.2, h: -1 }]), [], "empty boxes");
  assert.deepEqual(cleanBoxes([{ x: 0.9, y: 0.95, w: 0.5, h: 0.5 }]), [{ x: 0.9, y: 0.95, w: 0.1, h: 0.05 }], "cut at the edge");
  assert.deepEqual(cleanBoxes([{ x: -0.2, y: 0.1, w: 0.5, h: 0.1 }]), [{ x: 0, y: 0.1, w: 0.3, h: 0.1 }], "negative start");
  assert.deepEqual(cleanBoxes([{ x: 0.1, y: 0.1, w: 0.0001, h: 0.0001 }]), [{ x: 0.1, y: 0.1, w: 0.004, h: 0.004 }], "tiny boxes stay visible");
  assert.equal(cleanBoxes([{ x: 5, y: 5, w: 1, h: 1 }]).length, 0, "entirely outside");
});

test("boxRect gives padded percentages that stay inside the image", () => {
  assert.deepEqual(boxRect({ x: 0.1, y: 0.2, w: 0.5, h: 0.1 }, 0), { left: "10%", top: "20%", width: "50%", height: "10%" });
  assert.deepEqual(boxRect({ x: 0.1, y: 0.2, w: 0.5, h: 0.1 }, 0.01), { left: "9%", top: "19%", width: "52%", height: "12%" });
  assert.deepEqual(boxRect({ x: 0, y: 0, w: 1, h: 1 }, 0.05), { left: "0%", top: "0%", width: "100%", height: "100%" });
  assert.deepEqual(boxRect({ x: 0.9, y: 0.9, w: 0.1, h: 0.1 }, 0.02), { left: "88%", top: "88%", width: "12%", height: "12%" });
});

test("boxes show on images always and on a PDF page only for the hit's page", () => {
  const b = [{ x: 0.1, y: 0.1, w: 0.2, h: 0.1 }];
  assert.equal(boxesForView(b, null, null).length, 1, "an image has no page");
  assert.equal(boxesForView(b, 3, null).length, 1);
  assert.equal(boxesForView(b, 3, 3).length, 1, "the hit's page");
  assert.equal(boxesForView(b, 3, 4).length, 0, "another page");
  assert.equal(boxesForView(b, null, 2).length, 0, "no page in the hit");
  assert.equal(boxesForView(b, undefined, 1).length, 0);
  assert.equal(boxesForView(null, 3, 3).length, 0);
});

test("tableLabel counts rows and columns of the tab-separated text", () => {
  assert.equal(tableLabel(0, "a\tb\tc\n1\t2\t3\n4\t5\t6"), "Table 1: 3 rows × 3 columns");
  assert.equal(tableLabel(1, "only"), "Table 2: 1 row × 1 column");
  assert.equal(tableLabel(2, "a\tb\n1"), "Table 3: 2 rows × 2 columns", "the widest row counts");
  assert.equal(tableLabel(0, ""), "Table 1");
});

test("double click toggles fit and 1:1", () => {
  assert.deepEqual(toggleActual({ s: 0.5, tx: 3, ty: 4 }, 0.5), { s: 1, tx: 0, ty: 0 });
  assert.deepEqual(toggleActual({ s: 1, tx: 3, ty: 4 }, 0.5), { s: 0.5, tx: 0, ty: 0 });
  assert.deepEqual(toggleActual({ s: 1, tx: 0, ty: 0 }, 1), { s: 1, tx: 0, ty: 0 });
});

test("keyboard zoom about the centre keeps the centre point fixed", async () => {
  const { clampView, fitScale, zoomAt } = await import("./previewLogic.ts");
  const [cw, ch, iw, ih] = [400, 300, 800, 600];
  const fit = fitScale(cw, ch, iw, ih);
  const v0 = clampView({ s: 1, tx: 150, ty: 40 }, cw, ch, iw, ih);
  const z = zoomAt(v0, 1.25, 0, 0, fit, cw, ch, iw, ih);
  // the IMAGE point under the stage centre (iw/2 - tx/s) is the same before and after
  const pt = (v: { s: number; tx: number }) => iw / 2 - v.tx / v.s;
  assert.ok(Math.abs(pt(v0) - pt(z)) < 1e-9, `${pt(v0)} vs ${pt(z)}`);
  assert.equal(z.s, 1.25);
});

test("pan step is a fraction of the stage and clamps like a drag", async () => {
  const { clampView, fitScale } = await import("./previewLogic.ts");
  const [cw, ch, iw, ih] = [400, 300, 800, 600];
  const s = 2; // zoomed in; limits are +/- (800*2 - 400)/2 = 600 and +/- (600*2 - 300)/2 = 450
  const pan = (tx: number, ty: number, dx: number, dy: number) =>
    clampView({ s, tx: tx + dx * cw * 0.1, ty: ty + dy * ch * 0.1 }, cw, ch, iw, ih);
  // +10 % right / -10 % up from an interior view: moves, no clamp
  assert.deepEqual(pan(100, 100, 1, -1), { s, tx: 140, ty: 70 });
  // from the right edge, another +10 % stays clamped
  assert.deepEqual(pan(600, 0, 1, 0), { s, tx: 600, ty: 0 });
  // zoomed out to fit: nothing to pan, recentred
  const fitted = clampView({ s: fitScale(cw, ch, iw, ih), tx: 500, ty: 500 }, cw, ch, iw, ih);
  assert.deepEqual([fitted.tx, fitted.ty], [0, 0]);
});
