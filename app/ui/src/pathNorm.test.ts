// Run: npm test   (node's built-in runner; Node 22.6+ strips the types)
import { test } from "node:test";
import assert from "node:assert/strict";
import { extractPath, normalizeWinPath, pathQueryFromPaste } from "./pathNorm.ts";

test("extractPath finds the JSON-escaped path value", () => {
  // the clipboard holds literal doubled backslashes inside quotes, plus a trailing comma
  const line = '"path": "E:\\\\vs_studio_projects\\\\src\\\\BackgroundTasks.winmd",';
  assert.equal(extractPath(line), "E:\\\\vs_studio_projects\\\\src\\\\BackgroundTasks.winmd");
});

test("pathQueryFromPaste un-escapes the JSON paste into a searchable path", () => {
  const line = '"path": "E:\\\\vs_studio_projects\\\\src\\\\BackgroundTasks.winmd",';
  assert.equal(pathQueryFromPaste(line), "E:\\vs_studio_projects\\src\\BackgroundTasks.winmd");
});

test("pathQueryFromPaste pulls the path out of an ffmpeg line", () => {
  const cmd = 'ffmpeg -i "C:\\video\\my clip.mp4" -c copy out.mp4';
  // the path has spaces, so the query wraps it in quotes to stay one term
  assert.equal(pathQueryFromPaste(cmd), '"C:\\video\\my clip.mp4"');
});

test("pathQueryFromPaste decodes file:/// URLs including %20", () => {
  assert.equal(pathQueryFromPaste("file:///E:/my%20vids/a%20b.mp4"), '"E:\\my vids\\a b.mp4"');
});

test("pathQueryFromPaste keeps an already-clean path untouched and unquoted", () => {
  assert.equal(pathQueryFromPaste("C:\\Users\\Alice\\photo.jpg"), "C:\\Users\\Alice\\photo.jpg");
  assert.equal(pathQueryFromPaste("C:\\Users\\"), "C:\\Users\\");
});

test("normalizeWinPath handles scheme forms and glued quotes", () => {
  assert.equal(normalizeWinPath("file://C:/Users/Alice/photo.jpg"), "C:\\Users\\Alice\\photo.jpg");
  assert.equal(normalizeWinPath("'C:\\video\\clip.mp4'"), "C:\\video\\clip.mp4");
  // a stray % that is not a valid escape stays literal
  assert.equal(normalizeWinPath("C:/a%2zb.mp4"), "C:\\a%2zb.mp4");
});

test("extractPath picks the longest candidate and stops at copy junk", () => {
  const text = "wrote D:\\tmp\\x.log, then E:\\long\\folder\\final.mp4 done";
  assert.equal(extractPath(text), "E:\\long\\folder\\final.mp4");
});

test("extractPath treats a whole unquoted drive-path line as one path", () => {
  assert.equal(extractPath("C:\\my vids\\a b.mp4"), "C:\\my vids\\a b.mp4");
});

test("UNC paths normalize to a searchable substring", () => {
  assert.equal(pathQueryFromPaste("\\\\NAS\\share\\file.txt"), "\\NAS\\share\\file.txt");
});

test("plain text and query syntax leave the paste alone", () => {
  assert.equal(extractPath("see the notes for details"), null);
  assert.equal(pathQueryFromPaste("see the notes for details"), null);
  // wildcards / negation / operators: the paste already IS a query
  assert.equal(pathQueryFromPaste("!users\\ *.txt"), null);
  assert.equal(pathQueryFromPaste("C:\\Users\\ *.txt"), null);
  assert.equal(pathQueryFromPaste("report ext:pdf"), null);
  assert.equal(pathQueryFromPaste("   "), null);
});
