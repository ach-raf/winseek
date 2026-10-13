// Fake library data for the browser mock: ~400 images over three years (as coloured SVG tiles), exact
// duplicates, near-duplicate bursts and some very big files. Pure data: mock.ts adds it to its file
// database and implements the backend calls on top.

export type Theme = "photo" | "screenshot" | "receipt" | "whiteboard" | "meme" | "document";

export interface Tile {
  theme: Theme;
  hue: number;
  /** Small per-copy change (near-duplicates differ a little). */
  variant: number;
}

export interface LibFile {
  name: string;
  path: string;
  size: number;
  mtime: number;
  /** 1 = image, 3 = video, 0 = other (mock.ts "kind"). */
  kind: 1 | 3 | 0;
  w: number;
  h: number;
  tile: Tile | null;
  /** Files with the same key are byte-identical copies. */
  dupKey?: string;
  /** Files with the same key are near-duplicates (a burst). */
  nearKey?: string;
}

const PICS = "C:\\Users\\User\\Pictures";
const DAY = 86400000;

function mulberry32(a: number) {
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const THEME_WEIGHTS: [Theme, number][] = [["photo", 0.54], ["screenshot", 0.18], ["receipt", 0.09], ["whiteboard", 0.05], ["meme", 0.08], ["document", 0.06]];

export function buildLibrary(now: number): LibFile[] {
  const rnd = mulberry32(0x5eed1234);
  const pick = <T,>(a: T[]): T => a[Math.floor(rnd() * a.length)];
  const theme = (): Theme => {
    let r = rnd();
    for (const [t, w] of THEME_WEIGHTS) { if ((r -= w) < 0) return t; }
    return "photo";
  };
  const out: LibFile[] = [];
  let img = 100, shot = 0;

  const make = (mtime: number, th: Theme): LibFile => {
    const d = new Date(mtime);
    const year = d.getFullYear();
    const tile: Tile = { theme: th, hue: Math.floor(rnd() * 360), variant: 0 };
    let name: string, dir: string, w: number, h: number, size: number;
    switch (th) {
      case "screenshot": {
        shot++;
        const pad = (n: number) => String(n).padStart(2, "0");
        name = shot % 3 === 0
          ? `Screenshot ${year}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}.png`
          : `Screenshot (${shot}).png`;
        dir = `${PICS}\\Screenshots`;
        [w, h] = pick([[1920, 1080], [2560, 1440], [1366, 768]]);
        size = 250e3 + Math.floor(rnd() * 1.6e6);
        break;
      }
      case "meme":
        name = rnd() < 0.5 ? `FB_IMG_${1500000000000 + Math.floor(rnd() * 9e10)}.jpg` : `meme_${Math.floor(rnd() * 900 + 100)}.jpg`;
        dir = `C:\\Users\\User\\Downloads`;
        [w, h] = pick([[1080, 1080], [720, 900], [640, 640]]);
        size = 60e3 + Math.floor(rnd() * 400e3);
        break;
      case "receipt": case "document": case "whiteboard":
        img++;
        name = `IMG_${String(img).padStart(4, "0")}.jpg`;
        dir = `${PICS}\\${year}`;
        [w, h] = pick([[3000, 4000], [3024, 4032], [2448, 3264]]);
        size = 1.1e6 + Math.floor(rnd() * 2.6e6);
        break;
      default:
        img++;
        name = rnd() < 0.3 ? `PXL_${year}${String(d.getMonth() + 1).padStart(2, "0")}${String(d.getDate()).padStart(2, "0")}_${Math.floor(rnd() * 235959)}.jpg` : `IMG_${String(img).padStart(4, "0")}.jpg`;
        dir = `${PICS}\\${year}`;
        [w, h] = pick([[4032, 3024], [4032, 3024], [3024, 4032], [4000, 3000]]);
        size = 1.8e6 + Math.floor(rnd() * 4.2e6);
    }
    return { name, path: `${dir}\\${name}`, size: Math.round(size), mtime, kind: 1, w, h, tile };
  };

  // Three years of months, uneven on purpose (some empty, some busy).
  const nowD = new Date(now);
  for (let m = 0; m < 36; m++) {
    const first = new Date(nowD.getFullYear(), nowD.getMonth() - m, 1).getTime();
    const days = new Date(nowD.getFullYear(), nowD.getMonth() - m + 1, 0).getDate();
    const n = Math.floor(Math.pow(rnd(), 1.6) * 20) + (m % 6 === 0 ? 8 : 0) + (m === 17 || m === 18 ? 0 : 2);
    for (let i = 0; i < n; i++) {
      let t = first + Math.floor(rnd() * days) * DAY + Math.floor((8 + rnd() * 13) * 3600e3);
      if (t > now) t = now - Math.floor(rnd() * DAY * 3);
      out.push(make(t, theme()));
    }
  }

  // Exact duplicates: copies elsewhere with the same bytes (so the same size and tile).
  const dupSrc = out.filter((f) => f.tile!.theme !== "screenshot").slice(0, 120);
  const dupCount = 14;
  for (let g = 0; g < dupCount; g++) {
    const src = dupSrc[Math.floor(g * (dupSrc.length / dupCount))];
    src.dupKey = `d${g}`;
    const copies = 1 + (g % 3);
    for (let c = 0; c < copies; c++) {
      const base = src.name.replace(/\.[^.]+$/, ""), ext = src.name.slice(src.name.lastIndexOf("."));
      const [dir, nm] = c === 0 ? ["C:\\Users\\User\\Downloads", `${base} (1)${ext}`]
        : c === 1 ? ["C:\\Users\\User\\Desktop", `${base} - Copy${ext}`]
        : [`${PICS}\\Backup`, `${base}${ext}`];
      out.push({ ...src, name: nm, path: `${dir}\\${nm}`, mtime: Math.min(now, src.mtime + (c + 1) * 9 * DAY), dupKey: `d${g}`, nearKey: undefined });
    }
  }

  // Near-duplicate bursts: a few frames of the same scene seconds apart, some re-saved at lower resolution.
  const burstSrc = out.filter((f) => f.tile!.theme === "photo" && !f.dupKey);
  const bursts = 16;
  for (let g = 0; g < bursts; g++) {
    const src = burstSrc[Math.floor((g + 0.5) * (burstSrc.length / bursts))];
    src.nearKey = `n${g}`;
    const extra = 1 + (g % 4);
    for (let c = 1; c <= extra; c++) {
      const ext = src.name.slice(src.name.lastIndexOf("."));
      const name = src.name.replace(ext, `_BURST00${c}${ext}`);
      const small = c === 2;
      out.push({
        ...src, name, path: src.path.slice(0, src.path.lastIndexOf("\\") + 1) + name, mtime: src.mtime + c * 1800,
        w: small ? Math.round(src.w / 2) : src.w, h: small ? Math.round(src.h / 2) : src.h,
        size: Math.round(src.size * (small ? 0.32 : 0.9 + c * 0.05)),
        tile: { ...src.tile!, variant: c }, dupKey: undefined, nearKey: `n${g}`,
      });
    }
  }

  // A few huge forgotten files under the profile (not images).
  const bigs: [string, string, number][] = [
    ["C:\\Users\\User\\Videos", "Wedding raw footage.mp4", 14.2e9], ["C:\\Users\\User\\Videos", "Screen recording 2024-02-11.mkv", 9.6e9],
    ["C:\\Users\\User\\Downloads", "Win11_23H2_English_x64.iso", 6.4e9], ["C:\\Users\\User\\Downloads", "ubuntu-22.04.3-desktop-amd64.iso", 4.7e9],
    ["C:\\Users\\User\\Videos", "Holiday Lisbon 4K.mov", 3.9e9], ["C:\\Users\\User\\Documents\\VMs", "dev-box.vhdx", 3.3e9],
    ["C:\\Users\\User\\Downloads", "dataset_v3.zip", 2.1e9], ["C:\\Users\\User\\Desktop", "backup-2023-old-laptop.7z", 1.8e9],
    ["C:\\Users\\User\\Videos", "Lecture 12.mp4", 1.4e9], ["C:\\Users\\User\\Downloads", "installer_full.exe", 1.1e9],
  ];
  bigs.forEach(([dir, name, size], i) => {
    out.push({ name, path: `${dir}\\${name}`, size, mtime: now - (200 + i * 83) * DAY, kind: name.match(/\.(mp4|mkv|mov)$/) ? 3 : 0, w: 0, h: 0, tile: null });
  });
  for (let i = 0; i < 30; i++) {
    const nm = `render_${String(i).padStart(2, "0")}.mp4`;
    out.push({ name: nm, path: `C:\\Users\\User\\Videos\\Renders\\${nm}`, size: 300e6 + Math.floor(rnd() * 700e6), mtime: now - (30 + i * 29) * DAY, kind: 3, w: 0, h: 0, tile: null });
  }
  return out;
}

// ---- tiles --------------------------------------------------------------------------------------------

const cache = new Map<string, string>();

function svgFor(t: Tile, size: number): string {
  const h = t.hue, v = t.variant;
  const h2 = (h + 40) % 360;
  const o = v * 6; // shifts the foreground a little between near-duplicates
  const body = (() => {
    switch (t.theme) {
      case "receipt":
        return `<rect width="256" height="256" fill="hsl(${h} 12% 32%)"/>` +
          `<path d="M78 22h100v200l-10-8-10 8-10-8-10 8-10-8-10 8-10-8-10 8-10-8-10 8z" fill="#fbfbf6"/>` +
          [44, 60, 76, 92, 124, 140, 156].map((y, i) => `<rect x="94" y="${y}" width="${i % 3 === 0 ? 68 : 48}" height="5" rx="2" fill="#555"/>`).join("") +
          `<rect x="94" y="184" width="68" height="9" rx="2" fill="#111"/>`;
      case "whiteboard":
        return `<rect width="256" height="256" fill="hsl(${h} 10% 40%)"/><rect x="18" y="30" width="220" height="170" rx="6" fill="#f5f7f8" stroke="#aab" stroke-width="5"/>` +
          `<path d="M40 70c30-20 40 20 70 0s40 10 80-6M44 112h120M44 134h90M150 150l30 30 30-40" fill="none" stroke="#1d5fd6" stroke-width="4" stroke-linecap="round"/>` +
          `<circle cx="70" cy="165" r="16" fill="none" stroke="#c92a2a" stroke-width="4"/>`;
      case "meme":
        return `<rect width="256" height="256" fill="hsl(${h} 70% 48%)"/><circle cx="128" cy="132" r="62" fill="hsl(${h2} 80% 70%)"/>` +
          `<circle cx="108" cy="120" r="8" fill="#222"/><circle cx="150" cy="120" r="8" fill="#222"/><path d="M104 152q24 20 48 0" stroke="#222" stroke-width="6" fill="none" stroke-linecap="round"/>` +
          `<rect x="28" y="14" width="200" height="26" rx="4" fill="#fff" stroke="#000" stroke-width="3"/><rect x="40" y="216" width="176" height="26" rx="4" fill="#fff" stroke="#000" stroke-width="3"/>`;
      case "document":
        return `<rect width="256" height="256" fill="hsl(${h} 10% 36%)"/><rect x="50" y="18" width="156" height="220" rx="4" fill="#fff"/>` +
          [40, 56, 72, 88, 104, 120].map((y, i) => `<rect x="66" y="${y}" width="${i === 5 ? 70 : 124}" height="5" rx="2" fill="#9aa"/>`).join("") +
          `<path d="M70 200q18-28 30 0t30-6" fill="none" stroke="#1d3fa6" stroke-width="3"/><path d="M66 208h124" stroke="#333" stroke-width="2"/>` +
          `<rect x="150" y="172" width="44" height="16" rx="3" fill="#ffd54a"/>`;
      case "screenshot":
        return `<rect width="256" height="256" fill="hsl(${h} 30% 20%)"/><rect x="14" y="30" width="228" height="196" rx="6" fill="#f3f4f6"/>` +
          `<rect x="14" y="30" width="228" height="20" rx="6" fill="hsl(${h} 50% 55%)"/><rect x="14" y="50" width="52" height="176" fill="#e1e4e8"/>` +
          [66, 84, 102, 120].map((y) => `<rect x="78" y="${y}" width="${120 + ((y * 7) % 50)}" height="8" rx="3" fill="#b9bfc8"/>`).join("") +
          `<rect x="78" y="150" width="150" height="60" rx="4" fill="hsl(${h2} 45% 80%)"/>`;
      default:
        return `<defs><linearGradient id="s" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="hsl(${h} 70% 62%)"/><stop offset="1" stop-color="hsl(${h2} 80% 86%)"/></linearGradient></defs>` +
          `<rect width="256" height="256" fill="url(#s)"/><circle cx="${190 - o}" cy="${64 + o / 2}" r="22" fill="#fff6c8"/>` +
          `<path d="M0 190q60-${70 + o} 120-10t136-20v96H0z" fill="hsl(${h2} 35% 32%)"/><path d="M0 220q80-${46 + o / 2} 160-6t96-10v52H0z" fill="hsl(${h2} 40% 22%)"/>`;
    }
  })();
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 256 256">${body}</svg>`;
}

/** Data URL of a file's tile (cached). */
export function tileUrl(t: Tile, size = 256): string {
  const key = `${t.theme}|${t.hue}|${t.variant}|${size}`;
  let u = cache.get(key);
  if (!u) {
    u = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svgFor(t, size));
    cache.set(key, u);
  }
  return u;
}
