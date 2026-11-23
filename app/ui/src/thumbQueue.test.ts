import assert from "node:assert/strict";
import { test } from "node:test";
import { bumpThumbs, iconUrlPath, subscribeThumbVer, thumbVerSubscribedPaths, thumbVersion, withThumbVer } from "./thumbQueue.ts";

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
