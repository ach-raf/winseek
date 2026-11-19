// Custom themes (Settings › Appearance › Custom themes): a shelf of your own palettes, a shelf of
// ready-made ones to start from, and an editor with a live miniature of the app — click anything
// in the miniature to jump to its colour. Every edit saves and repaints the whole app live.
// Colours ride on the base palette as inline custom properties, so the transparency, selection
// and rail formulas in base.css keep working untouched. Styles: settings.css (.ct-*).

import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import {
  ROLES, activeCustomTheme, currentConcreteTheme, customThemeRef, loadCustomThemes,
  makeCustomThemeId, normalizeThemeHex, removeCustomTheme, sanitizeCustomTheme,
  upsertCustomTheme, type ConcreteTheme, type CustomTheme, type ThemeId, type ThemeRole,
} from "./theme";
import { inkFor } from "./accent";
import { builtinPalettes } from "./builtinPalettes";
import { PRESETS } from "./themePresets";
import { ConfirmButton } from "./Confirm";
import { Grp } from "./settingsUi";

const DARK_BASES: ConcreteTheme[] = ["dark", "mocha", "dracula", "tokyo"];
const isDarkBase = (b: ConcreteTheme) => DARK_BASES.includes(b);

/** A palette as the shelf and the loader see it: a built-in, a ready-made one or a saved theme. */
type Source = { id: string; name: string; base: ConcreteTheme; accent: string; colors: Record<string, string>; tag?: string };

function uniqueName(want: string, customs: CustomTheme[]): string {
  if (!customs.some((c) => c.name === want)) return want;
  let n = 2;
  while (customs.some((c) => c.name === `${want} ${n}`)) n += 1;
  return `${want} ${n}`;
}

/** Roles a palette of this base can carry (rail cards exist on dark bases only). */
function rolesFor(base: ConcreteTheme): ThemeRole[] {
  const dark = isDarkBase(base);
  return ROLES.filter((r) => !r.darkOnly || dark);
}

function recordFrom(src: Source, name: string): CustomTheme {
  const colors: Record<string, string> = {};
  for (const r of rolesFor(src.base)) { const v = src.colors[r.token]; if (v) colors[r.token] = v; }
  return {
    id: makeCustomThemeId(), name, base: src.base, accent: src.accent, baseAccent: src.accent,
    colors, baseColors: { ...colors }, createdAt: Date.now(),
  };
}

function savedSource(c: CustomTheme): Source {
  return { id: `saved-${c.id}`, name: c.name, base: c.base, accent: c.accent, colors: { ...c.baseColors, ...c.colors }, tag: "Yours" };
}

// ---------------------------------------------------------------------------------------------
// The miniature
// ---------------------------------------------------------------------------------------------

type Look = { colors: Record<string, string>; accent: string; base: ConcreteTheme };

/** A tiny app window painted from a palette alone (inline vars, independent of the live theme).
 *  `onPick` makes its parts clickable: each carries the role it paints. */
function Miniature({ look, onPick, compact }: { look: Look; onPick?: (token: string) => void; compact?: boolean }) {
  const dark = isDarkBase(look.base);
  const c = (t: string, fb: string) => look.colors[t] ?? fb;
  const bg = c("--bg-0", dark ? "#1b1c21" : "#e8eaee");
  const cards = c("--bg2-0", dark ? "#151619" : "#f5f6f8");
  const ctl = c("--bg3-0", dark ? "#282a31" : "#d8dbe2");
  const fg = c("--fg", dark ? "#f3f4f6" : "#1a1c22");
  const style = {
    "--m-bg": bg, "--m-cards": cards, "--m-ctl": ctl, "--m-line": c("--line", ctl),
    "--m-rail": c("--side-0", bg), "--m-railc": c("--side-card-0", cards), "--m-paper": c("--paper", "#f3f3f3"),
    "--m-fg": fg, "--m-fg2": c("--fg2", fg), "--m-fg3": c("--fg3", fg),
    "--m-acc": look.accent, "--m-ink": inkFor(look.accent),
    "--m-kw": c("--tok-keyword", fg), "--m-str": c("--tok-string", fg), "--m-num": c("--tok-number", fg),
    "--m-fn": c("--tok-function", fg), "--m-cls": c("--tok-class", fg),
    "--m-del": c("--tok-deleted", fg), "--m-ins": c("--tok-inserted", fg),
  } as CSSProperties;
  const part = (token: string, label: string) =>
    onPick ? { "data-role": token, role: "button", tabIndex: 0, title: label, onClick: (e: React.MouseEvent) => { e.stopPropagation(); onPick(token); },
      onKeyDown: (e: React.KeyboardEvent) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); onPick(token); } } } : {};
  return (
    <div className={"mini" + (compact ? " compact" : "")} style={style} aria-hidden={onPick ? undefined : true} {...part("--bg-0", "Canvas")}>
      <div className="mini-rail" {...part("--side-0", "Sidebar rail")}>
        <i className="mini-dot" />
        <i className="mini-bar on" /><i className="mini-bar" /><i className="mini-bar" />
        {dark && <div className="mini-railc" {...part("--side-card-0", "Rail cards")}><i /><i /></div>}
      </div>
      <div className="mini-main">
        <div className="mini-card" {...part("--bg2-0", "Cards")}>
          <b className="mini-h" {...part("--fg", "Text")}>{compact ? "" : "Invoice 2026.pdf"}</b>
          <span className="mini-t2" {...part("--fg2", "Secondary text")}>{compact ? "" : "Documents · 2.4 MB"}</span>
          <span className="mini-t3" {...part("--fg3", "Muted text")}>{compact ? "" : "Modified yesterday"}</span>
          <div className="mini-sel" {...part("accent", "Accent")}><i className="mini-acc" />{compact ? "" : "Selected result"}</div>
          <div className="mini-row">
            <span className="mini-btn" {...part("accent", "Accent")}>{compact ? "" : "Open"}</span>
            <span className="mini-chip" {...part("--bg3-0", "Controls")}>{compact ? "" : "Filter"}</span>
          </div>
          <div className="mini-hr" {...part("--line", "Hairlines")} />
        </div>
        {!compact && (
          <div className="mini-code" {...part("--bg2-0", "Cards")}>
            <code><s {...part("--tok-keyword", "Keywords")} className="k">fn</s> <s {...part("--tok-function", "Functions")} className="f">search</s>(<s {...part("--tok-class", "Classes")} className="c">Query</s>) {"{"}</code>
            <code>&nbsp;&nbsp;<s {...part("--tok-string", "Strings")} className="s">"name"</s>, <s {...part("--tok-number", "Numbers")} className="n">42</s></code>
            <code><s {...part("--tok-deleted", "Deleted lines")} className="d">− old line</s></code>
            <code><s {...part("--tok-inserted", "Inserted lines")} className="i">+ new line</s></code>
          </div>
        )}
        <div className="mini-paper" {...part("--paper", "Paper sheets")} />
      </div>
    </div>
  );
}

/** A shelf card: miniature, name, a one-line hint, and what a click does. */
function ShelfCard({ name, tag, look, on, onClick, hint, extra }: {
  name: string; tag?: string; look: Look; on?: boolean; onClick: () => void; hint: string; extra?: React.ReactNode;
}) {
  return (
    <div className={"ct-card" + (on ? " on" : "")}>
      <button type="button" className="ct-face" onClick={onClick} title={hint} aria-pressed={on === undefined ? undefined : on}>
        <Miniature look={look} compact />
        <span className="ct-cap">
          <span className="ct-cname">{name}</span>
          {on ? <span className="ct-live">active</span> : tag ? <span className="ct-tag">{tag}</span> : null}
        </span>
      </button>
      {extra}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The block
// ---------------------------------------------------------------------------------------------

export function CustomThemesBlock({ customs, activeId, onActivate, onListChange, onError }: {
  customs: CustomTheme[];
  /** The theme setting this window shows right now (the Theme row's value). */
  activeId: ThemeId;
  onActivate: (id: ThemeId) => void;
  onListChange: (list: CustomTheme[]) => void;
  onError: (e: unknown) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [importText, setImportText] = useState("");
  const [importErr, setImportErr] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [shelf, setShelf] = useState<"dark" | "light">(() => {
    const b = activeCustomTheme()?.base ?? currentConcreteTheme() ?? "light";
    return isDarkBase(b) ? "dark" : "light";
  });

  const sources = useMemo<Source[]>(() => [...builtinPalettes().map((p) => ({ ...p, tag: "Built-in" })), ...PRESETS.map((p) => ({ ...p, tag: "Preset" }))], []);

  const refresh = () => onListChange(loadCustomThemes());
  const activate = (rec: CustomTheme) => onActivate(customThemeRef(rec.id));

  /** Save a patch and — when this theme is the one on screen — repaint the app live. */
  const patch = (rec: CustomTheme, p: Partial<CustomTheme>) => {
    upsertCustomTheme({ ...rec, ...p });
    refresh();
    const ref = customThemeRef(rec.id);
    if (activeId === ref) onActivate(ref);
  };

  const startFrom = (src: Source) => {
    const rec = recordFrom(src, uniqueName(src.name, customs));
    upsertCustomTheme(rec);
    refresh();
    activate(rec);
    setEditing(rec.id);
  };

  /** "New theme": start from the palette that's on screen right now. */
  const create = () => {
    const onScreen = activeCustomTheme();
    if (onScreen) return startFrom({ ...savedSource(onScreen), name: onScreen.name });
    const base = currentConcreteTheme() ?? "light";
    const accentNow = normalizeThemeHex(getComputedStyle(document.documentElement).getPropertyValue("--acc").trim());
    const known = sources.find((s) => s.base === base && s.id.startsWith("builtin-"));
    if (known) startFrom({ ...known, name: "My theme", accent: accentNow ?? known.accent });
  };

  const duplicate = (src: CustomTheme) => startFrom({ ...savedSource(src), name: src.name });

  const remove = (rec: CustomTheme) => {
    removeCustomTheme(rec.id);
    refresh();
    if (activeId === customThemeRef(rec.id)) onActivate(rec.base);
    if (editing === rec.id) setEditing(null);
  };

  const exportTheme = (rec: CustomTheme) => {
    navigator.clipboard?.writeText(JSON.stringify(rec, null, 2))
      .then(() => {
        setCopied(rec.id);
        window.setTimeout(() => setCopied((c) => (c === rec.id ? null : c)), 1500);
      })
      .catch(onError);
  };

  const importNow = () => {
    const raw = importText.trim();
    if (!raw) return;
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { setImportErr("That text isn't JSON — use a theme's Export copy."); return; }
    const rec = sanitizeCustomTheme(parsed);
    if (!rec) { setImportErr("That text isn't a saved winseek theme."); return; }
    const fresh: CustomTheme = { ...rec, id: makeCustomThemeId(), name: uniqueName(rec.name, customs) };
    upsertCustomTheme(fresh);
    refresh();
    setImporting(false); setImportText(""); setImportErr(null);
    activate(fresh);
    setEditing(fresh.id);
  };

  const editingRec = editing ? customs.find((c) => c.id === editing) ?? null : null;
  const shown = sources.filter((s) => (shelf === "dark") === isDarkBase(s.base));

  return (
    <Grp id="custom-themes" title="Custom themes"
         note="Your palettes sit in the Theme row above and follow every window.">
      {editingRec ? (
        <ThemeEditor key={editingRec.id} rec={editingRec} customs={customs} sources={sources}
                     onPatch={(p) => patch(editingRec, p)} onClose={() => setEditing(null)}
                     onDuplicate={() => duplicate(editingRec)} onDelete={() => remove(editingRec)}
                     onExport={() => exportTheme(editingRec)} copied={copied === editingRec.id} />
      ) : (
        <div className="ct-wrap">
          <div className="ct-top">
            <div>
              <h4 className="ct-h">Your themes</h4>
              <p className="ct-sub">Pick one to use it, or open it to tweak every colour.</p>
            </div>
            <span className="ct-new">
              <button className="gbtn" onClick={() => { setImporting((v) => !v); setImportErr(null); }}>Import</button>
              <button className="gbtn pri" onClick={create}>+ New theme</button>
            </span>
          </div>
          {importing && (
            <div className="ct-import">
              <textarea className="ct-paste" rows={4} spellCheck={false} value={importText}
                        placeholder="Paste an exported theme JSON here…" aria-label="Paste an exported theme"
                        onChange={(e) => { setImportText(e.target.value); setImportErr(null); }} />
              <div className="ct-foot">
                <button className="gbtn" disabled={!importText.trim()} onClick={importNow}>Import theme</button>
                {importErr && <span className="ct-err" role="alert">{importErr}</span>}
              </div>
            </div>
          )}
          {customs.length === 0 ? (
            <div className="ct-empty">
              <b>Nothing here yet.</b>
              <span>Start from a ready-made palette below, or press “New theme” to copy the one you're using.</span>
            </div>
          ) : (
            <div className="ct-shelf">
              {customs.map((rec) => {
                const on = activeId === customThemeRef(rec.id);
                return (
                  <ShelfCard key={rec.id} name={rec.name} look={{ colors: { ...rec.baseColors, ...rec.colors }, accent: rec.accent, base: rec.base }}
                             on={on} onClick={() => activate(rec)} hint={on ? `${rec.name} is on` : `Use ${rec.name}`}
                             extra={
                               <span className="ct-cacts">
                                 <button type="button" onClick={() => setEditing(rec.id)}>Edit</button>
                                 <button type="button" onClick={() => duplicate(rec)}>Copy</button>
                                 <ConfirmButton label="Delete" confirmLabel="Sure?" onConfirm={() => remove(rec)} />
                               </span>
                             } />
                );
              })}
            </div>
          )}

          <div className="ct-top ct-top2">
            <div>
              <h4 className="ct-h">Start from a palette</h4>
              <p className="ct-sub">Click one to make your own copy and edit it.</p>
            </div>
            <div className="seg" role="radiogroup" aria-label="Palette mode">
              {(["dark", "light"] as const).map((m) => (
                <button key={m} role="radio" aria-checked={shelf === m} className={shelf === m ? "on" : ""} onClick={() => setShelf(m)}>
                  {m === "dark" ? "Dark" : "Light"}
                </button>
              ))}
            </div>
          </div>
          <div className="ct-shelf">
            {shown.map((s) => (
              <ShelfCard key={s.id} name={s.name} tag={s.tag} look={s} onClick={() => startFrom(s)} hint={`Make an editable copy of ${s.name}`} />
            ))}
          </div>
        </div>
      )}
    </Grp>
  );
}

// ---------------------------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------------------------

function RoleRow({ role, rec, onSet, hl }: { role: ThemeRole; rec: CustomTheme; onSet: (token: string, hex: string | null) => void; hl: boolean }) {
  const hex = rec.colors[role.token];
  const baseHex = rec.baseColors[role.token];
  const cur = hex ?? baseHex ?? "#888888";
  const changed = !!hex && hex !== baseHex;
  return (
    <div className={"ct-role" + (hl ? " hl" : "")} data-token={role.token}>
      <label className="ct-sw" style={{ background: cur }} title={`${role.label}: pick a colour`}>
        <input type="color" value={/^#[0-9a-f]{6}$/.test(cur) ? cur : "#888888"}
               aria-label={`${role.label} colour`} onChange={(e) => onSet(role.token, e.target.value)} />
      </label>
      <span className="ct-lab">{role.label}</span>
      <HexField label={`${role.label} hex`} value={hex ?? ""} placeholder={baseHex ?? ""} onCommit={(h) => onSet(role.token, h)} />
      <button type="button" className="ct-reset" disabled={!changed}
              title={baseHex ? `Reset to ${baseHex}` : "Reset"} aria-label={`Reset ${role.label.toLowerCase()}`}
              onClick={() => onSet(role.token, null)}>↺</button>
    </div>
  );
}

/** The hex field next to a swatch: commits as soon as what's typed is a valid colour, holds the
 *  half-typed text otherwise, and drops it (back to the committed value) on blur. */
function HexField({ label, value, placeholder, onCommit }: {
  label: string; value: string; placeholder?: string; onCommit: (hex: string) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = (raw: string) => {
    const norm = normalizeThemeHex(raw);
    if (norm) { setDraft(null); onCommit(norm); return true; }
    return false;
  };
  return (
    <input className="ct-hex" aria-label={label} spellCheck={false} maxLength={7}
           value={draft ?? value} placeholder={placeholder} title={label}
           onChange={(e) => { if (!commit(e.target.value)) setDraft(e.target.value); }}
           onBlur={() => setDraft(null)}
           onKeyDown={(e) => { if (e.key === "Enter" && !commit(e.currentTarget.value)) setDraft(null); }} />
  );
}

function ThemeEditor({ rec, customs, sources, onPatch, onClose, onDuplicate, onDelete, onExport, copied }: {
  rec: CustomTheme;
  customs: CustomTheme[];
  sources: Source[];
  onPatch: (p: Partial<CustomTheme>) => void;
  onClose: () => void;
  onDuplicate: () => void;
  onDelete: () => void;
  onExport: () => void;
  copied: boolean;
}) {
  const dark = isDarkBase(rec.base);
  const roles = rolesFor(rec.base);
  const [hl, setHl] = useState<string | null>(null);
  const [loader, setLoader] = useState(false);
  const [loaded, setLoaded] = useState<{ name: string; before: Partial<CustomTheme> } | null>(null);
  const [shelf, setShelf] = useState<"dark" | "light">(dark ? "dark" : "light");
  const listRef = useRef<HTMLDivElement>(null);
  const hlTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(hlTimer.current), []);

  const set = (token: string, hex: string | null) => {
    const colors = { ...rec.colors };
    if (hex) colors[token] = hex; else delete colors[token];
    onPatch({ colors });
  };

  const focusRole = (token: string) => {
    setHl(token);
    window.clearTimeout(hlTimer.current);
    hlTimer.current = window.setTimeout(() => setHl(null), 1800);
    const el = listRef.current?.querySelector<HTMLElement>(`[data-token="${token}"]`);
    el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    el?.querySelector<HTMLInputElement>(".ct-hex")?.focus({ preventScroll: true });
  };
  const focusAccent = () => focusRole("accent");

  const modified = rec.accent !== rec.baseAccent || roles.some((r) => rec.colors[r.token] && rec.colors[r.token] !== rec.baseColors[r.token]);

  const loadPalette = (s: Source) => {
    setLoaded({ name: s.name, before: { base: rec.base, accent: rec.accent, baseAccent: rec.baseAccent, colors: rec.colors, baseColors: rec.baseColors } });
    const colors: Record<string, string> = {};
    for (const r of rolesFor(s.base)) { const v = s.colors[r.token]; if (v) colors[r.token] = v; }
    onPatch({ base: s.base, accent: s.accent, baseAccent: s.accent, colors, baseColors: { ...colors } });
    setLoader(false);
  };

  const group = (g: ThemeRole["group"]) => roles.filter((r) => r.group === g);
  const cells = (list: ThemeRole[]) => list.map((r) => <RoleRow key={r.token} role={r} rec={rec} onSet={set} hl={hl === r.token} />);
  const look: Look = { colors: { ...rec.baseColors, ...rec.colors }, accent: rec.accent, base: rec.base };
  const others = sources.concat(customs.filter((c) => c.id !== rec.id).map(savedSource)).filter((s) => (shelf === "dark") === isDarkBase(s.base));

  return (
    <div className="ct-editor">
      <div className="ct-ehead">
        <button type="button" className="ct-back" onClick={onClose}>‹ All themes</button>
        <input className="ct-nameinput" value={rec.name} maxLength={40} aria-label="Theme name"
               placeholder="Name your theme" onChange={(e) => onPatch({ name: e.target.value })} />
        <button className="gbtn pri" onClick={onClose}>Done</button>
      </div>

      <div className="ct-cols">
        <div className="ct-left" ref={listRef}>
          <div className="ct-loadbar">
            <button type="button" className="gbtn" aria-expanded={loader} onClick={() => setLoader((v) => !v)}>
              {loader ? "Close palettes" : "Load a palette…"}
            </button>
            <button type="button" className="gbtn" disabled={!modified}
                    onClick={() => onPatch({ colors: { ...rec.baseColors }, accent: rec.baseAccent })}>Reset all</button>
            {loaded && (
              <span className="ct-loaded">
                Loaded {loaded.name} ·{" "}
                <button type="button" className="ct-link" onClick={() => { onPatch(loaded.before); setLoaded(null); }}>Undo</button>
              </span>
            )}
          </div>
          {loader && (
            <div className="ct-loader">
              <div className="seg" role="radiogroup" aria-label="Palette mode">
                {(["dark", "light"] as const).map((m) => (
                  <button key={m} role="radio" aria-checked={shelf === m} className={shelf === m ? "on" : ""} onClick={() => setShelf(m)}>
                    {m === "dark" ? "Dark" : "Light"}
                  </button>
                ))}
              </div>
              <div className="ct-chips">
                {others.map((s) => (
                  <button key={s.id} type="button" className="ct-chip" onClick={() => loadPalette(s)} title={`Replace every colour with ${s.name}`}>
                    <span className="ct-strip" aria-hidden>
                      {["--bg-0", "--bg2-0", "--bg3-0", "--fg"].map((t) => <i key={t} style={{ background: s.colors[t] ?? "transparent" }} />)}
                      <i style={{ background: s.accent }} />
                    </span>
                    <span>{s.name}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          <div className="ct-sec" data-token="accent">
            <h4>Accent</h4>
            <div className={"ct-role" + (hl === "accent" ? " hl" : "")}>
              <label className="ct-sw" style={{ background: rec.accent }} title="Accent: pick a colour">
                <input type="color" value={rec.accent} aria-label="Accent colour" onChange={(e) => onPatch({ accent: e.target.value })} />
              </label>
              <span className="ct-lab">Buttons, selection, highlights</span>
              <HexField label="Accent hex" value={rec.accent} placeholder={rec.baseAccent} onCommit={(h) => onPatch({ accent: h })} />
              <button type="button" className="ct-reset" disabled={rec.accent === rec.baseAccent} aria-label="Reset accent"
                      title={`Reset to ${rec.baseAccent}`} onClick={() => onPatch({ accent: rec.baseAccent })}>↺</button>
            </div>
          </div>
          <div className="ct-sec"><h4>Surfaces</h4>{cells(group("surfaces"))}</div>
          <div className="ct-sec"><h4>Text</h4>{cells(group("text"))}</div>
          <details className="ct-code" open>
            <summary>Code colours</summary>
            {cells(group("code"))}
          </details>
        </div>

        <aside className="ct-right" aria-label="Live preview">
          <Miniature look={look} onPick={(t) => focusRole(t)} />
          <p className="ct-hint" onClick={focusAccent}>Click anything in the preview to edit its colour.</p>
          <div className="ct-foot">
            <button className="gbtn" onClick={onDuplicate}>Duplicate</button>
            <button className="gbtn" onClick={onExport}>{copied ? "Copied ✓" : "Export"}</button>
            <ConfirmButton label="Delete" confirmLabel={`Delete “${rec.name}”?`} onConfirm={onDelete} />
          </div>
        </aside>
      </div>
    </div>
  );
}
