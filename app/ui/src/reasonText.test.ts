import test from "node:test";
import assert from "node:assert/strict";
import { fileName, isReasonCode, reasonHint, reasonSentence } from "./reasonText.ts";

test("the sentences read like a person wrote them, with the count", () => {
  assert.equal(reasonSentence("damaged_image", 31), "31 pictures are damaged or not really pictures");
  assert.equal(reasonSentence("damaged_image", 1), "1 picture is damaged or not really a picture");
  assert.equal(reasonSentence("damaged_video", 1), "1 video is incomplete or damaged");
  assert.equal(reasonSentence("damaged_archive", 6), "6 Office or zip files are damaged");
  assert.equal(reasonSentence("damaged_pdf", 1), "1 PDF is incomplete or damaged");
  assert.equal(reasonSentence("damaged_image", 1234), "1,234 pictures are damaged or not really pictures");
});

test("old free text or unknown codes never reach the screen", () => {
  const raw = [
    "decode: The image format could not be determined",
    "ffmpeg: [in#0 @ 00000222cd15a800] Error opening input: Invalid data found",
    "pdf: PdfiumLibraryInternalError(FormatError)",
    "",
  ];
  for (const r of raw) {
    assert.equal(isReasonCode(r), false);
    assert.equal(reasonSentence(r, 2), "2 files could not be read");
    assert.equal(reasonHint(r), null);
  }
});

test("a hint only where the person can do something", () => {
  assert.match(reasonHint("password_protected") ?? "", /password/);
  assert.match(reasonHint("legacy_office") ?? "", /\.docx/);
  assert.equal(reasonHint("damaged_video"), null);
  assert.equal(reasonHint("too_large"), null);
  assert.equal(reasonHint("empty_file"), null);
});

test("every sentence names its count once and no code shows through", () => {
  for (const code of ["damaged_image", "not_an_image", "empty_file", "temp_file", "still_writing", "cloud_only", "too_large", "unreadable", "password_protected"]) {
    for (const n of [1, 2]) {
      const s = reasonSentence(code, n);
      assert.equal(s.split(String(n)).length - 1, 1, s);
      assert.ok(!s.includes("_") && !s.includes("{"), s);
    }
  }
});

test("file names for the example list", () => {
  assert.equal(fileName("C:\\a\\b\\clip.mp4"), "clip.mp4");
  assert.equal(fileName("/x/y.png"), "y.png");
  assert.equal(fileName("plain"), "plain");
});
