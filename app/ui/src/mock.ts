// Browser-preview backend (used when not running inside Tauri): mirrors the Rust
// MockSource shapes so `npm run dev` in a normal browser shows a working popup.
import type { Backend, Filter, Hit, Sort } from "./types";

const WORDS = ["invoice","report","budget","holiday","photo","screenshot","meeting","notes","draft","final","contract","resume","backup","project","design","summer","winter","family","tax","receipt","presentation","analysis","readme","config","archive","setup","installer","demo","logo","icon","paris","tokyo","berlin","wedding","birthday","lecture","thesis","model","dataset","export"];
const EXTS: [string, number][] = [["jpg",1],["png",1],["heic",1],["pdf",2],["docx",2],["xlsx",2],["txt",2],["md",2],["pptx",2],["mp4",3],["mkv",3],["mp3",4],["flac",4],["wav",4],["zip",0],["exe",0],["rs",0],["py",0],["json",0],["log",0]];
const ROOTS = ["C:\\Users\\User\\Documents","C:\\Users\\User\\Pictures","C:\\Users\\User\\Downloads","C:\\Users\\User\\Desktop","D:\\Projects\\winseek\\crates\\index\\src","D:\\Media\\Videos","D:\\Media\\Music","D:\\Archive\\2019\\Old Laptop Backup\\Documents"];
const SUBS = ["2024","2025","Work","Personal","Misc","Clients","Scans","Camera Roll"];

interface E { name: string; lower: string; path: string; dir: boolean; size: number; mtime: number; kind: number }

let seed = 0x9e3779b9;
const rnd = () => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) / 4294967296; };
const pick = <T,>(a: T[]): T => a[Math.floor(rnd() * a.length)];

const DB: E[] = (() => {
  const now = Date.now();
  const out: E[] = [];
  for (let i = 0; i < 30000; i++) {
    const dir = `${pick(ROOTS)}\\${pick(SUBS)}`;
    if (i % 40 === 0) {
      const name = `${pick(WORDS)} ${pick(SUBS)}`;
      out.push({ name, lower: name.toLowerCase(), path: `${dir}\\${name}`, dir: true, size: 0, mtime: now - rnd() * 3e10, kind: 5 });
      continue;
    }
    const [ext, kind] = pick(EXTS);
    const name = kind === 1 ? `IMG_${String(Math.floor(rnd() * 9999)).padStart(4, "0")}.${ext}` : `${pick(WORDS)}_${pick(WORDS)}_${Math.floor(rnd() * 100)}.${ext}`;
    const size = Math.floor(rnd() * 4e6 * (kind === 3 ? 300 : 1)) + 200;
    out.push({ name, lower: name.toLowerCase(), path: `${dir}\\${name}`, dir: false, size, mtime: now - Math.pow(rnd(), 3) * 3e10, kind });
  }
  return out;
})();

const toHit = (e: E): Hit => ({ name: e.name, path: e.path, is_dir: e.dir, size: e.dir ? null : e.size, mtime: Math.floor(e.mtime) });
const okFilter = (e: E, f: Filter) => f === "all" || (f === "images" && e.kind === 1) || (f === "docs" && e.kind === 2) || (f === "video" && e.kind === 3) || (f === "audio" && e.kind === 4) || (f === "folders" && e.dir);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const mockBackend: Backend = {
  async search({ query, offset, limit, sort, filter }) {
    const t0 = performance.now();
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    const m = DB.filter((e) => okFilter(e, filter) && terms.every((t) => e.lower.includes(t)));
    const s: Sort = sort === "rel" && terms.length === 0 ? "date" : sort;
    if (s === "name") m.sort((a, b) => a.lower.localeCompare(b.lower));
    else if (s === "size") m.sort((a, b) => b.size - a.size);
    else if (s === "date") m.sort((a, b) => b.mtime - a.mtime);
    else m.sort((a, b) => Number(b.lower.startsWith(terms[0])) - Number(a.lower.startsWith(terms[0])) || a.lower.length - b.lower.length);
    await sleep(8);
    return { total: m.length, hits: m.slice(offset, offset + limit).map(toHit), elapsed_us: Math.round((performance.now() - t0) * 1000) };
  },
  async status() { return [{ letter: "C", files: 12580243, folders: 1204511, state: "live" }]; },
  async recent(limit) { return [...DB].filter((e) => !e.dir).sort((a, b) => b.mtime - a.mtime).slice(0, limit).map(toHit); },
  async openPath(p) { console.info("[mock] open", p); },
  async revealPath(p) { console.info("[mock] reveal", p); },
  async copyPath(p) { try { await navigator.clipboard.writeText(p); } catch { /* ignore */ } console.info("[mock] copy", p); },
  async hide() { console.info("[mock] hide"); },
  async onShown() { return () => {}; },
};
