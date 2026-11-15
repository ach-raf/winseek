import { test } from "node:test";
import assert from "node:assert/strict";
import { CONFIRM_MAX_ITEMS, recycleAsk, removeFolderAsk } from "./confirmLogic.ts";

test("recycleAsk: one file names it, shows the path and the way back", () => {
  const a = recycleAsk(["D:\\Photos\\cat.png"]);
  assert.equal(a.title, "Move “cat.png” to the Recycle Bin?");
  assert.equal(a.body, "D:\\Photos\\cat.png");
  assert.equal(a.verb, "Recycle");
  assert.equal(a.items, undefined);
  assert.equal(a.more, undefined);
  assert.match(a.note!, /Recycle Bin/);
});

test("recycleAsk: several files list the first few and count the rest", () => {
  const paths = Array.from({ length: 7 }, (_, i) => `C:\\tmp\\file${i}.txt`);
  const a = recycleAsk(paths);
  assert.equal(a.title, "Move 7 files to the Recycle Bin?");
  assert.equal(a.body, undefined);
  assert.deepEqual(a.items, ["file0.txt", "file1.txt", "file2.txt", "file3.txt", "file4.txt"]);
  assert.equal(a.more, 2);
});

test("recycleAsk: exactly the cap shows all names and no 'more' line", () => {
  const paths = Array.from({ length: CONFIRM_MAX_ITEMS }, (_, i) => `C:\\tmp\\f${i}.txt`);
  const a = recycleAsk(paths);
  assert.equal(a.items!.length, CONFIRM_MAX_ITEMS);
  assert.equal(a.more, undefined);
});

test("recycleAsk: one more than the cap pushes exactly one into the 'more' count", () => {
  const paths = Array.from({ length: CONFIRM_MAX_ITEMS + 1 }, (_, i) => `C:\\tmp\\f${i}.txt`);
  const a = recycleAsk(paths);
  assert.equal(a.items!.length, CONFIRM_MAX_ITEMS);
  assert.equal(a.more, 1);
});

test("removeFolderAsk: names the folder, warns it forgets, blames nothing on disk", () => {
  const a = removeFolderAsk("D:\\Photos");
  assert.equal(a.title, "Remove D:\\Photos from the index?");
  assert.equal(a.verb, "Remove");
  assert.match(a.body!, /forgets the files already indexed/);
  assert.match(a.note!, /not touched/);
});
