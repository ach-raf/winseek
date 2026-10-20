import assert from "node:assert/strict";
import { test } from "node:test";
import { calc } from "./calc.ts";

test("precedence and parentheses", () => {
  assert.equal(calc("2+2*3"), "8");
  assert.equal(calc("(2+2)*3"), "12");
  assert.equal(calc("10-4/2"), "8");
  assert.equal(calc("2*3+4*5"), "26");
});

test("power is right-assoc: 2^3^2 = 512", () => {
  assert.equal(calc("2^3^2"), "512");
  assert.equal(calc("2^10"), "1024");
});

test("unary minus and modulo", () => {
  assert.equal(calc("-2^2"), "-4");
  assert.equal(calc("3--2"), "5");
  assert.equal(calc("10 % 3"), "1");
});

test("percent of: 20% of 80 = 16", () => {
  assert.equal(calc("20% of 80"), "16");
  assert.equal(calc("20 % of 80"), "16");
  assert.equal(calc("50% of 200"), "100");
});

test("divide by zero and junk return null", () => {
  assert.equal(calc("5/0"), null);
  assert.equal(calc("10 % 0"), null);
  assert.equal(calc("hello"), null);
  assert.equal(calc("2 + x"), null);
  assert.equal(calc(""), null);
  assert.equal(calc("   "), null);
  assert.equal(calc("2+"), null);
  assert.equal(calc("(2+3"), null);
  assert.equal(calc("2 3"), null); // trailing junk
  assert.equal(calc("0.1+0.2"), "0.3"); // binary noise rounded off
  assert.equal(calc("(15/4)^2"), String(parseFloat(Math.pow(15 / 4, 2).toPrecision(12))));
});
