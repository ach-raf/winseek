import { test } from "node:test";
import assert from "node:assert/strict";
import { parseQuery } from "./filters.ts";
import { applyTerm, GUIDE, isActive } from "./guideLogic.ts";

const term = (id: string, text: string) => GUIDE.find((g) => g.id === id)!.terms.find((t) => t.text === text)!;

test("a date term appends, then replaces the previous date, then toggles off", () => {
  let q = "invoice";
  q = applyTerm(q, parseQuery(q), term("date", "today"));
  assert.equal(q, "invoice today");
  q = applyTerm(q, parseQuery(q), term("date", "last week"));
  assert.equal(q, "invoice last week");
  assert.ok(isActive(parseQuery(q), term("date", "last week")));
  q = applyTerm(q, parseQuery(q), term("date", "last week"));
  assert.equal(q, "invoice");
});

test("kind and size terms replace their own chip only", () => {
  let q = "report big";
  q = applyTerm(q, parseQuery(q), term("kind", "documents"));
  assert.equal(q, "report big documents");
  q = applyTerm(q, parseQuery(q), term("size", "huge"));
  assert.equal(q, "report documents huge");
});

test("syntax rows append for the user to finish", () => {
  assert.equal(applyTerm("", parseQuery(""), term("advanced", "ext:jpg;png")), "ext:jpg;png");
  assert.equal(applyTerm("cat ", parseQuery("cat "), term("advanced", "!draft")), "cat !draft");
});
