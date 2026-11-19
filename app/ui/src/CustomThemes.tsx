// Custom themes (Settings › Appearance › Custom themes): make a palette of your own, give each
// colour its pick, and switch to it like any built-in. Creating starts from the palette on
// screen — its record is seeded from the computed palette (theme.ts points data-theme at the
// base with no overrides yet, so the computed style *is* the seed) — and every edit saves and
// repaints the whole app live. Colours ride on the base palette as inline custom properties,
// so the transparency, selection and rail formulas in base.css keep working untouched. Styles:
// settings.css (.ct-*).

import { useState } from "react";
import {
  ROLES, THEMES, activeCustomTheme, currentConcreteTheme, customThemeRef, loadCustomThemes,
  makeCustomThemeId, normalizeThemeHex, removeCustomTheme, sanitizeCustomTheme, themeMode,
  upsertCustomTheme, type CustomTheme, type ThemeId, type ThemeRole,
} from "./theme";
import { ConfirmButton } from "./Confirm";
import { Grp, Row } from "./settingsUi";

function themeLabel(base: CustomTheme["base"]): string {
  return THEMES.find((t) => t.id === base)?.label ?? base;
}

/** "Custom 1", "Custom 2", … — the first number not already taken by a saved theme. */
function nextName(customs: CustomTheme[]): string {
  let n = customs.length + 1;
  while (customs.some((c) => c.name === `Custom ${n}`)) n += 1;
  return `Custom ${n}`;
}

/** A duplicate of an existing record under a fresh id (the New-theme seed for a custom that is
 *  already on screen, and the Duplicate button). */
function cloneOf(src: CustomTheme, name: string): CustomTheme {
  return {
    id: makeCustomThemeId(), name, base: src.base, accent: src.accent, baseAccent: src.baseAccent,
    colors: { ...src.colors }, baseColors: { ...src.baseColors }, createdAt: Date.now(),
  };
}

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

  /** Save a patch and — when this theme is the one on screen — repaint the app live. */
  const patch = (rec: CustomTheme, p: Partial<CustomTheme>) => {
    upsertCustomTheme({ ...rec, ...p });
    onListChange(loadCustomThemes());
    const ref = customThemeRef(rec.id);
    if (activeId === ref) onActivate(ref);
  };

  const activate = (rec: CustomTheme) => onActivate(customThemeRef(rec.id));

  const create = () => {
    const onScreen = activeCustomTheme();
    if (onScreen) {
      // The palette on screen is already a custom one: duplicate it wholesale.
      const rec = cloneOf(onScreen, nextName(customs));
      upsertCustomTheme(rec);
      onListChange(loadCustomThemes());
      activate(rec);
      setEditing(rec.id);
      return;
    }
    const base = currentConcreteTheme() ?? "light";
    // The accent must be read BEFORE the record first activates: activation applies the record's
    // own accent through the accent mechanism, so a post-switch read would only see the seed
    // placeholder. --acc is the accent mechanism's inline property, independent of data-theme.
    const accentNow =
      normalizeThemeHex(getComputedStyle(document.documentElement).getPropertyValue("--acc").trim()) ?? "#ffc533";
    // Seed the record, paint the bare base (data-theme = base, no overrides yet), then snapshot
    // the computed palette as both colours and base colours.
    let rec: CustomTheme = {
      id: makeCustomThemeId(), name: nextName(customs), base, accent: accentNow, baseAccent: accentNow,
      colors: {}, baseColors: {}, createdAt: Date.now(),
    };
    upsertCustomTheme(rec);
    onListChange(loadCustomThemes());
    activate(rec);
    const cs = getComputedStyle(document.documentElement);
    const dark = themeMode(base) === "dark";
    const colors: Record<string, string> = {};
    for (const r of ROLES) {
      if (r.darkOnly && !dark) continue;
      const hex = normalizeThemeHex(cs.getPropertyValue(r.token).trim());
      if (hex) colors[r.token] = hex;
    }
    rec = { ...rec, colors, baseColors: { ...colors } };
    upsertCustomTheme(rec);
    onListChange(loadCustomThemes());
    activate(rec);
    setEditing(rec.id);
  };

  const duplicate = (src: CustomTheme) => {
    const rec = cloneOf(src, nextName(customs));
    upsertCustomTheme(rec);
    onListChange(loadCustomThemes());
    activate(rec);
    setEditing(rec.id);
  };

  const remove = (rec: CustomTheme) => {
    removeCustomTheme(rec.id);
    onListChange(loadCustomThemes());
    if (activeId === customThemeRef(rec.id)) onActivate(rec.base); // land on the palette it started from
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
    try {
      parsed = JSON.parse(raw);
    } catch {
      setImportErr("That text isn't JSON — use a theme's Export copy.");
      return;
    }
    const rec = sanitizeCustomTheme(parsed);
    if (!rec) {
      setImportErr("That text isn't a saved winseek theme.");
      return;
    }
    const fresh: CustomTheme = { ...rec, id: makeCustomThemeId() }; // never overwrite on import
    upsertCustomTheme(fresh);
    onListChange(loadCustomThemes());
    setImporting(false);
    setImportText("");
    setImportErr(null);
    activate(fresh);
    setEditing(fresh.id); // open it in the editor — an import is the start of a tweak
  };

  const editingRec = editing ? customs.find((c) => c.id === editing) ?? null : null;

  return (
    <Grp id="custom-themes" title="Custom themes"
         note="Your palettes sit in the Theme row above and follow every window.">
      <Row id="new-theme" title="Make your own"
           desc="Start from the palette on screen, then give each colour its pick.">
        <span className="ct-new">
          <button className="gbtn" onClick={create}>New theme</button>
          <button className="gbtn" onClick={() => { setImporting((v) => !v); setImportErr(null); }}>Import</button>
        </span>
      </Row>
      {importing && (
        <div className="ct-import">
          <textarea className="ct-paste" rows={4} spellCheck={false} value={importText}
                    placeholder="Paste an exported theme JSON here…" aria-label="Paste an exported theme"
                    onChange={(e) => { setImportText(e.target.value); setImportErr(null); }} />
          <div className="ct-foot">
            <button className="gbtn" disabled={!importText.trim()} onClick={importNow}>Import</button>
            {importErr && <span className="ct-err" role="alert">{importErr}</span>}
          </div>
        </div>
      )}
      {editingRec ? (
        <ThemeEditor rec={editingRec} onPatch={(p) => patch(editingRec, p)} onClose={() => setEditing(null)}
                     onDuplicate={() => duplicate(editingRec)} onDelete={() => remove(editingRec)}
                     onExport={() => exportTheme(editingRec)} copied={copied === editingRec.id} />
      ) : customs.length === 0 ? (
        <p className="ct-empty">
          No custom themes yet. “New theme” saves the palette you're using as your own — canvas,
          cards, text, code colours, accent — and switches to it right away.
        </p>
      ) : (
        customs.map((rec) => {
          const on = activeId === customThemeRef(rec.id);
          return (
            <div key={rec.id} className={"ct-row" + (on ? " on" : "")}>
              <button type="button" className="ct-open" onClick={() => activate(rec)} aria-pressed={on}
                      title={on ? `${rec.name} is on` : `Switch to ${rec.name}`}>
                <span className="ct-strip" aria-hidden>
                  {["--bg-0", "--bg2-0", "--bg3-0"].map((tok) => (
                    <i key={tok} style={rec.colors[tok] ? { background: rec.colors[tok] } : undefined} />
                  ))}
                  <i className="ct-acc-dot" style={{ background: rec.accent }} />
                </span>
                <span className="ct-name">{rec.name}</span>
                {on && <span className="ct-live">active</span>}
              </button>
              <span className="ct-acts">
                <button type="button" className="gbtn" onClick={() => setEditing(rec.id)}>Edit</button>
                <button type="button" className="gbtn" onClick={() => duplicate(rec)}>Duplicate</button>
                <ConfirmButton label="Delete" confirmLabel={`Delete “${rec.name}”?`} onConfirm={() => remove(rec)} />
              </span>
            </div>
          );
        })
      )}
    </Grp>
  );
}

/** One colour cell: swatch (native colour picker), label, hex field, per-role reset. */
function RoleCell({ role, rec, onSet }: { role: ThemeRole; rec: CustomTheme; onSet: (token: string, hex: string | null) => void }) {
  const hex = rec.colors[role.token];
  const baseHex = rec.baseColors[role.token];
  const cur = hex ?? baseHex ?? "#888888";
  return (
    <div className="ct-role">
      <label className="ct-sw" style={{ background: cur }} title={`${role.label}: pick a colour`}>
        <input type="color" value={/^#[0-9a-f]{6}$/.test(cur) ? cur : "#888888"}
               aria-label={`${role.label} colour`} onChange={(e) => onSet(role.token, e.target.value)} />
      </label>
      <span className="ct-lab">{role.label}</span>
      <div className="ct-line">
        <HexField label={`${role.label} hex`} value={hex ?? ""} placeholder={baseHex ?? ""}
                  onCommit={(h) => onSet(role.token, h)} />
        <button type="button" className="ct-reset" disabled={!hex || hex === baseHex}
                title={baseHex ? `Reset ${role.label.toLowerCase()} to ${baseHex}` : `Reset ${role.label.toLowerCase()}`}
                onClick={() => onSet(role.token, null)}>↺</button>
      </div>
    </div>
  );
}

/** The hex text field next to a swatch: commits as soon as what's typed is a valid colour,
 *  holds the half-typed text otherwise, and drops it (back to the committed value) on blur. */
function HexField({ label, value, placeholder, onCommit }: {
  label: string; value: string; placeholder?: string; onCommit: (hex: string) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = (raw: string) => {
    const norm = normalizeThemeHex(raw);
    if (norm) {
      setDraft(null);
      onCommit(norm);
      return true;
    }
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

/** The inline editor card: name, every colour role grouped, export/duplicate/delete. The app
 *  behind stays visible and repaints on each pick. */
function ThemeEditor({ rec, onPatch, onClose, onDuplicate, onDelete, onExport, copied }: {
  rec: CustomTheme;
  onPatch: (p: Partial<CustomTheme>) => void;
  onClose: () => void;
  onDuplicate: () => void;
  onDelete: () => void;
  onExport: () => void;
  copied: boolean;
}) {
  const dark = themeMode(rec.base) === "dark";
  const surfaces = ROLES.filter((r) => r.group === "surfaces" && (!r.darkOnly || dark));
  const text = ROLES.filter((r) => r.group === "text");
  const code = ROLES.filter((r) => r.group === "code");
  const set = (token: string, hex: string | null) => {
    const colors = { ...rec.colors };
    if (hex) colors[token] = hex;
    else delete colors[token]; // reset: inherit the base palette again
    onPatch({ colors });
  };
  return (
    <div className="ct-editor">
      <div className="ct-head">
        <input className="ct-name" value={rec.name} maxLength={40} aria-label="Theme name"
               placeholder="Name your theme" onChange={(e) => onPatch({ name: e.target.value })} />
        <span className="ct-from">Started from {themeLabel(rec.base)}</span>
      </div>
      <div className="ct-role ct-acc">
        <label className="ct-sw" style={{ background: rec.accent }} title="Accent: pick a colour">
          <input type="color" value={rec.accent} aria-label="Accent colour"
                 onChange={(e) => onPatch({ accent: e.target.value })} />
        </label>
        <span className="ct-lab">Accent</span>
        <div className="ct-line">
          <HexField label="Accent hex" value={rec.accent} placeholder={rec.baseAccent}
                    onCommit={(h) => onPatch({ accent: h })} />
          <button type="button" className="ct-reset" disabled={rec.accent === rec.baseAccent}
                  title={`Reset the accent to ${rec.baseAccent}`}
                  onClick={() => onPatch({ accent: rec.baseAccent })}>↺</button>
        </div>
      </div>
      <div className="ct-sec">
        <h4>Surfaces</h4>
        <div className="ct-grid">{surfaces.map((r) => <RoleCell key={r.token} role={r} rec={rec} onSet={set} />)}</div>
      </div>
      <div className="ct-sec">
        <h4>Text</h4>
        <div className="ct-grid">{text.map((r) => <RoleCell key={r.token} role={r} rec={rec} onSet={set} />)}</div>
      </div>
      <details className="ct-code">
        <summary>Code colours</summary>
        <div className="ct-grid">{code.map((r) => <RoleCell key={r.token} role={r} rec={rec} onSet={set} />)}</div>
      </details>
      <div className="ct-foot">
        <button className="gbtn" onClick={onDuplicate}>Duplicate</button>
        <button className="gbtn" onClick={onExport}>{copied ? "Copied ✓" : "Export"}</button>
        <ConfirmButton label="Delete" confirmLabel={`Delete “${rec.name}”?`} onConfirm={onDelete} />
        <span className="ct-flex" aria-hidden />
        <button className="gbtn" onClick={onClose}>Done</button>
      </div>
    </div>
  );
}
