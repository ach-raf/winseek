// Pure calculator for the search box (PRD §6): "2+2*3", "(15/4)^2", "20% of 80" -> a result
// string; anything that is not pure arithmetic (letters other than "of", junk, division by
// zero) -> null so the query stays a normal search. No eval, no imports.

type Tok = { n: number } | { op: string };

/** Chars allowed besides digits: + - * / ^ % ( ) space and the word "of". */
function tokenize(text: string): Tok[] | null {
  const s = text.trim().toLowerCase();
  if (!s) return null;
  const toks: Tok[] = [];
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === " ") { i++; continue; }
    if (c >= "0" && c <= "9" || c === ".") {
      const m = /^\d*\.?\d+/.exec(s.slice(i));
      if (!m) return null; // a bare "." or "1.2.3"
      toks.push({ n: parseFloat(m[0]) });
      i += m[0].length;
      continue;
    }
    if ("+-*/^%()".includes(c)) { toks.push({ op: c }); i++; continue; }
    if (s.startsWith("of", i)) { toks.push({ op: "of" }); i += 2; continue; }
    return null; // any other letter: not a calculation
  }
  return toks.length ? toks : null;
}

export function calc(text: string): string | null {
  const toks = tokenize(text);
  if (!toks) return null;
  let pos = 0;
  const peek = () => (pos < toks.length ? toks[pos] : null);
  const takeOp = (...ops: string[]): string | null => {
    const t = peek();
    if (t && "op" in t && ops.includes(t.op)) { pos++; return t.op; }
    return null;
  };

  const atom = (): number | null => {
    if (takeOp("(")) {
      const v = expr();
      if (v === null || !takeOp(")")) return null;
      return v;
    }
    const t = peek();
    if (t && "n" in t) { pos++; return t.n; }
    return null;
  };
  // Right-assoc power: 2^3^2 = 2^(3^2) = 512. A unary minus binds looser than ^ (-2^2 = -4).
  const power = (): number | null => {
    const base = atom();
    if (base === null) return null;
    if (!takeOp("^")) return base;
    const exp = unary();
    if (exp === null) return null;
    return Math.pow(base, exp);
  };
  const unary = (): number | null => {
    if (takeOp("-")) {
      const v = unary();
      return v === null ? null : -v;
    }
    return power();
  };
  // "*" "/" and modulo at one level; "N% of M" (percent of) is recognised here too.
  const term = (): number | null => {
    let left = unary();
    if (left === null) return null;
    for (;;) {
      if (takeOp("*")) {
        const r = unary();
        if (r === null) return null;
        left = left * r;
      } else if (takeOp("/")) {
        const r = unary();
        if (r === null || r === 0) return null; // division by zero -> not a result
        left = left / r;
      } else if (takeOp("%")) {
        if (takeOp("of")) {
          const r = unary();
          if (r === null) return null;
          left = (left / 100) * r;
        } else {
          const r = unary();
          if (r === null || r === 0) return null;
          left = left % r;
        }
      } else return left;
    }
  };
  const expr = (): number | null => {
    let left = term();
    if (left === null) return null;
    for (;;) {
      const op = takeOp("+", "-");
      if (!op) return left;
      const r = term();
      if (r === null) return null;
      left = op === "+" ? left + r : left - r;
    }
  };

  const v = expr();
  if (v === null || pos !== toks.length || !Number.isFinite(v)) return null;
  // 1e10 as 10000000000, 8.1 as 8.1, 0.1+0.2 as 0.3: round off binary noise.
  return String(parseFloat(v.toPrecision(12)));
}
