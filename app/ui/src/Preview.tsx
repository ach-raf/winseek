import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox, ParsedDoc, PreviewInfo } from "./types";
import { FileIcon } from "./FileIcon";
import { Snippet } from "./Semantic";
import { formatDate, formatSize } from "./format";
import { boxesForView, boxRect, clampPage, clampView, crumbParts, extOf, fitScale, formatClock, kindLabel, highlight, MAX_SCALE, pdfRenderWidth, previewTerms, seekTarget, tableLabel, toggleActual, zoomAt, type View } from "./previewLogic";
import { combos, hint } from "./keymap";
import { tooltip } from "./Shortcuts";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

/** What the popup's global key handler may drive while the preview is open (PRD B6). */
export interface PreviewApi {
  /** +1 next PDF page, -1 previous. */
  page: (d: number) => void;
  /** Zoom about the centre (e.g. 1.25 in, 0.8 out). */
  zoom: (factor: number) => void;
  /** Fit; again -> 100 %. */
  fit: () => void;
  /** Pan by fractions of the stage (Alt+Shift+arrows). */
  pan: (dx: number, dy: number) => void;
  /** The Extract text / parse action. */
  extract: () => void;
  /** Copy the extracted text. */
  copyText: () => void;
}

export interface PreviewProps {
  backend: Backend;
  hit: Hit;
  /** Free-text part of the search; its words are highlighted in text files. */
  query: string;
  /** Text found inside the file by the semantic search (`[match]` marks). */
  snippet?: string | null;
  /** Seconds into a video / audio file to start at (a hit that carries a timestamp). */
  start?: number;
  /** PDF page (1-based) to open on: the page the search hit is on. */
  page?: number | null;
  /** Where the query words sit on the image / on that PDF page (fractions 0-1); drawn over the picture. */
  boxes?: HitBox[] | null;
  onOpen: () => void;
  onReveal: () => void;
  onCopyPath: () => void;
  /** The popup registers keys into this ref while the preview is open. */
  apiRef?: { current: PreviewApi | null };
}

/**
 * Space preview. Loads `preview_info` for the hit; a newer hit supersedes an older one (the older answer is
 * dropped and its media elements are unmounted, which aborts their downloads). The previous content stays,
 * dimmed, until the new one arrives so arrowing through results does not flash.
 */
export function Preview({ backend, hit, query, snippet, start, page, boxes, onOpen, onReveal, onCopyPath, apiRef }: PreviewProps) {
  // Children (PDF view, image stage, parse bar) contribute their piece of the keyboard API.
  const apiParts = useRef<Partial<PreviewApi>>({});
  const registerApi = useCallback((patch: Partial<PreviewApi>) => { apiParts.current = { ...apiParts.current, ...patch }; }, []);
  useEffect(() => {
    if (!apiRef) return;
    apiRef.current = {
      page: (d) => apiParts.current.page?.(d),
      zoom: (f) => apiParts.current.zoom?.(f),
      fit: () => apiParts.current.fit?.(),
      pan: (dx, dy) => apiParts.current.pan?.(dx, dy),
      extract: () => apiParts.current.extract?.(),
      copyText: () => apiParts.current.copyText?.(),
    };
    return () => { if (apiRef) apiRef.current = null; };
  }, [apiRef]);
  const [info, setInfo] = useState<PreviewInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // The PDF page being shown (the actions read that page) and whether the optional document parser is installed.
  const [pdfPage, setPdfPage] = useState<number | null>(null);
  const [parser, setParser] = useState<boolean | null>(null);

  useEffect(() => {
    let live = true;
    backend.parserPresent().then((p) => { if (live) setParser(p); }, () => { if (live) setParser(false); });
    return () => { live = false; };
  }, [backend, hit.path]);

  useEffect(() => {
    let live = true;
    setLoading(true);
    setPdfPage(null);
    backend.previewInfo(hit.path).then(
      (i) => { if (live) { setInfo(i); setErr(null); setLoading(false); } },
      (e) => { if (live) { setInfo(null); setErr(errText(e)); setLoading(false); } },
    );
    return () => { live = false; };
  }, [backend, hit.path]);

  const shown = info && info.path === hit.path ? info : null;
  const stale = loading && !!info;
  // While the next file's info is on its way, the previous facts stay (dimmed): the column below
  // the stage keeps its height, so moving through a list never bounces the layout around.
  const lastShown = useRef<PreviewInfo | null>(null);
  if (shown) lastShown.current = shown;
  const metaInfo = shown ?? (stale ? lastShown.current : null);
  const label = kindLabel(shown?.kind ?? null, shown?.ext ?? extOf(hit.name), hit.is_dir);
  const crumbs = crumbParts(hit.path, hit.is_dir);
  const keep = (e: React.MouseEvent) => e.preventDefault(); // keep focus in the search box

  return (
    <aside className="pv" aria-label={`Preview of ${hit.name}`}>
      <div className="pv-col">
        <div className={"pbox" + (stale ? " stale" : "")}>
          {err ? (
            <div className="pv-msg" role="alert">{err}</div>
          ) : info ? (
            <Body key={info.path + "#" + (page ?? 0) + "@" + (start ?? 0)} backend={backend} info={info} query={query} start={start} page={page ?? null}
                  boxes={boxes ?? null} onPage={setPdfPage} registerApi={registerApi} />
          ) : (
            <div className="pv-msg"><span className="spin" aria-label="Loading" /></div>
          )}
        </div>
        <div className="pv-title">
          <h2 title={hit.name}>{hit.name}</h2>
          <div className="kind">{label}</div>
        </div>
        <div className="crumbs" title={hit.path}>
          {crumbs.map((p, i) => i === crumbs.length - 1
            ? <b key={i}>{p}</b>
            : <span key={i} className="cr"><span>{p}</span><i>›</i></span>)}
        </div>
        {snippet && (
          <div className="whyb"><b>Why it&rsquo;s here</b><Snippet text={snippet} /></div>
        )}
        {metaInfo && <Meta info={metaInfo} folderSize={hit.is_dir ? hit.size : null} dim={stale} />}
        {metaInfo && parser !== null && (metaInfo.kind === "image" || (metaInfo.kind === "pdf" && metaInfo.pdf?.engine && metaInfo.pdf.pages !== null)) && (
          <ParseBar backend={backend} path={metaInfo.path} available={parser} dim={stale} registerApi={registerApi}
                    page={metaInfo.kind === "pdf" ? (pdfPage ?? clampPage(page ?? 1, metaInfo.pdf?.pages ?? null)) : null} />
        )}
        <div className="btns">
          <button className="btn pri" tabIndex={-1} onMouseDown={keep} onClick={onOpen} title={tooltip("preview.open")}>Open <kbd>↵</kbd></button>
          <button className="btn" tabIndex={-1} onMouseDown={keep} onClick={onReveal} title={tooltip("preview.reveal")}>Show in folder</button>
          <button className="btn" tabIndex={-1} onMouseDown={keep} onClick={onCopyPath} title={tooltip("preview.copyPath")}>Copy path</button>
        </div>
      </div>
    </aside>
  );
}

/** Compact facts, two per row, so the pane fits without a scrollbar. Noise is left out: the drive
 *  letter is already the first crumb, and "EXIF: None" / "GPS: No" say nothing worth a row. */
function Meta({ info, folderSize, dim }: { info: PreviewInfo; folderSize?: number | null; dim?: boolean }) {
  const cells: [string, string][] = [];
  if (info.mtime !== null) cells.push(["Modified", formatDate(info.mtime)]);
  if (info.size !== null && info.kind !== "folder") cells.push(["Size", formatSize(info.size)]);
  // A folder's size comes with the search row (the index adds up everything inside it).
  if (info.kind === "folder" && folderSize != null) cells.push(["Contains", formatSize(folderSize)]);
  if (info.ctime !== null) cells.push(["Created", formatDate(info.ctime)]);
  const im = info.image;
  if (im?.width && im.height) cells.push(["Dimensions", `${im.width} × ${im.height}`]);
  if (info.kind === "text" && info.text) cells.push(["Encoding", info.text.encoding]);
  if (info.kind === "pdf" && info.pdf?.pages) cells.push(["Pages", String(info.pdf.pages)]);
  const ex = im?.exif;
  if (ex?.date) cells.push(["Taken", ex.date]);
  if (ex?.camera) cells.push(["Camera", ex.camera]);
  return (
    <dl className={"meta" + (dim ? " stale" : "")}>
      {cells.map(([k, v], i) => (
        <div key={k} className={i === cells.length - 1 && cells.length % 2 === 1 ? "wide" : undefined}>
          <dt>{k}</dt><dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

function Body({ backend, info, query, start, page, boxes, onPage, registerApi }: {
  backend: Backend; info: PreviewInfo; query: string; start?: number; page: number | null; boxes: HitBox[] | null; onPage: (n: number) => void;
  registerApi: (patch: Partial<PreviewApi>) => void;
}) {
  switch (info.kind) {
    case "pdf": return <PdfView backend={backend} info={info} query={query} page={page} boxes={boxes} onPage={onPage} registerApi={registerApi} />;
    case "image": return <ImageView info={info} boxes={boxes} registerApi={registerApi} />;
    case "video": case "audio": return <MediaView info={info} start={start} />;
    // A document (docx / xlsx / pptx / ODF / RTF) reads exactly like a text file.
    case "text": case "document": return <TextView info={info} query={query} />;
    default: return <OtherView info={info} />;
  }
}

// ---- images: fit, wheel zoom, drag pan, double-click 1:1 ------------------------------------------------------

function ImageView({ info, boxes, registerApi }: { info: PreviewInfo; boxes: HitBox[] | null; registerApi: (patch: Partial<PreviewApi>) => void }) {
  const meta = info.image;
  const [useThumb, setUseThumb] = useState(!!meta?.thumb);
  const [dead, setDead] = useState(false);
  const src = useThumb ? info.thumb : info.src;
  const drawn = useMemo(() => boxesForView(boxes, null, null), [boxes]);
  if (!src || dead) return <OtherView info={info} note="This image cannot be shown here." />;
  // Fit (contain) always: the whole picture is visible on open, and wheel / drag / the zoom
  // bar take it from there. (Cover cropped the picture and locked pan at the crop scale.)
  return (
    <ImageStage
      key={src} src={src} name={info.name} boxes={drawn} registerApi={registerApi}
      dims={!useThumb && meta?.width && meta.height ? { w: meta.width, h: meta.height } : null}
      note={useThumb ? "Shell thumbnail" : null}
      onError={() => { if (!useThumb && info.thumb) setUseThumb(true); else setDead(true); }}
    />
  );
}

function ImageStage({ src, name, dims, note, boxes, onError, registerApi }: {
  src: string; name: string; dims: { w: number; h: number } | null; note: string | null; boxes: HitBox[]; onError: () => void;
  registerApi?: (patch: Partial<PreviewApi>) => void;
}) {
  const stage = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ w: 0, h: 0 });
  const [nat, setNat] = useState(dims);
  const [view, setView] = useState<View>({ s: 1, tx: 0, ty: 0 });
  const [fitted, setFitted] = useState(true);
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number } | null>(null);

  const fit = nat ? fitScale(box.w, box.h, nat.w, nat.h) : 1;
  const cur: View = fitted ? { s: fit, tx: 0, ty: 0 } : view;
  const zoomed = nat !== null && cur.s > fit + 1e-6;
  // The wheel listener is native (React's is passive and could not stop the list from scrolling).
  const latest = useRef({ cur, fit, box, nat });
  latest.current = { cur, fit, box, nat };

  const apply = (v: View, f: number) => {
    if (v.s <= f + 1e-6) { setFitted(true); setView({ s: f, tx: 0, ty: 0 }); } else { setFitted(false); setView(v); }
  };

  useLayoutEffect(() => {
    const el = stage.current;
    if (!el) return;
    const measure = () => setBox({ w: el.clientWidth, h: el.clientHeight });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, []);

  // Keyboard zoom / pan (Ctrl+= / Ctrl+- / Ctrl+0 / Alt+Shift+arrows) reuse the wheel maths:
  // zoom about the stage centre (px = py = 0), pan by 10 % of the stage, clamped like a drag.
  useEffect(() => {
    if (!registerApi) return;
    registerApi({
      zoom: (factor) => {
        const { cur, fit, box, nat } = latest.current;
        if (!nat) return;
        apply(zoomAt(cur, factor, 0, 0, fit, box.w, box.h, nat.w, nat.h), fit);
      },
      fit: () => {
        const { cur, fit, nat } = latest.current;
        if (!nat) return;
        apply(toggleActual(cur, fit), fit);
      },
      pan: (dx, dy) => {
        const { cur, fit, box, nat } = latest.current;
        if (!nat || cur.s <= fit + 1e-6) return; // nothing to pan while the whole image is visible
        apply(clampView({ s: cur.s, tx: cur.tx + dx * box.w * 0.1, ty: cur.ty + dy * box.h * 0.1 }, box.w, box.h, nat.w, nat.h), fit);
      },
    });
    return () => registerApi({ zoom: undefined, fit: undefined, pan: undefined });
  }, [registerApi]);

  useEffect(() => {
    const el = stage.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const { cur, fit, box, nat } = latest.current;
      if (!nat) return;
      const r = el.getBoundingClientRect();
      const px = e.clientX - r.left - r.width / 2, py = e.clientY - r.top - r.height / 2;
      const dy = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaY;
      apply(zoomAt(cur, Math.exp(-dy * 0.0015), px, py, fit, box.w, box.h, nat.w, nat.h), fit);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const onDown = (e: React.PointerEvent) => {
    if (e.button !== 0 || !nat) return;
    e.preventDefault(); // keep focus in the search box
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, y: e.clientY, tx: cur.tx, ty: cur.ty };
    setDragging(true);
  };
  const onMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || !nat) return;
    const { cur, fit, box } = latest.current;
    if (cur.s <= fit + 1e-6) return; // nothing to pan while the whole image is visible
    apply(clampView({ s: cur.s, tx: d.tx + e.clientX - d.x, ty: d.ty + e.clientY - d.y }, box.w, box.h, nat.w, nat.h), fit);
  };
  const onUp = () => { drag.current = null; setDragging(false); };
  const onDouble = () => { if (nat) apply(toggleActual(cur, fit), fit); };
  const zoomStep = (factor: number) => {
    const { cur, fit, box, nat } = latest.current;
    if (!nat) return;
    apply(zoomAt(cur, factor, 0, 0, fit, box.w, box.h, nat.w, nat.h), fit);
  };
  const keep = (e: React.MouseEvent) => e.preventDefault(); // keep focus in the search box

  const style: React.CSSProperties = nat
    ? {
        width: nat.w, height: nat.h, marginLeft: -nat.w / 2, marginTop: -nat.h / 2,
        transform: `translate(${cur.tx}px, ${cur.ty}px) scale(${cur.s})`,
      }
    : { visibility: "hidden" };

  return (
    <div className={"pv-stage" + (dragging ? " dragging" : "") + (!fitted ? " zoomed" : "")} ref={stage}
         onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp} onPointerCancel={onUp} onDoubleClick={onDouble}>
      {/* The boxes sit in the same transformed layer as the picture, so they follow zoom and pan. */}
      <div className="pv-img" style={style}>
        <img src={src} alt={name} draggable={false} decoding="async" style={{ imageRendering: cur.s >= 3 ? "pixelated" : "auto" }}
             onLoad={(e) => {
               const i = e.currentTarget;
               setNat({ w: i.naturalWidth || 800, h: i.naturalHeight || 600 });
             }}
             onError={onError} />
        {boxes.map((b, i) => <div key={i} className="pv-box" style={boxRect(b)} aria-hidden />)}
      </div>
      {/* The zoom bar: wheel and drag work too, but these say so without a manual.
          Pointer events stop at the bar so a press does not start a stage drag. */}
      {nat && (
        <div className="pv-zoombar" onPointerDown={(e) => e.stopPropagation()}>
          <button className="pv-zb" tabIndex={-1} onMouseDown={keep} disabled={cur.s <= fit + 1e-6}
                  onClick={() => zoomStep(0.8)} aria-label="Zoom out"
                  title={`Zoom out (scroll wheel, ${combos("preview.zoomOut")[0] ?? "Ctrl+-"})`}>−</button>
          <span className="pv-zp" aria-live="off">{Math.round(cur.s * 100)}%</span>
          <button className="pv-zb" tabIndex={-1} onMouseDown={keep} disabled={cur.s >= MAX_SCALE - 1e-6}
                  onClick={() => zoomStep(1.25)} aria-label="Zoom in"
                  title={`Zoom in (scroll wheel, ${combos("preview.zoomIn")[0] ?? "Ctrl+="})`}>+</button>
          {fit < 1 - 1e-6 && (
            <button className="pv-zb pv-zfit" tabIndex={-1} onMouseDown={keep} onClick={() => apply(toggleActual(cur, fit), fit)}
                    title={zoomed ? "Back to fit (double-click, Ctrl+0)" : "Actual size (double-click)"}>
              {zoomed ? "Fit" : "1:1"}
            </button>
          )}
          {note && <span className="pv-zn">{note}</span>}
        </div>
      )}
    </div>
  );
}

// ---- PDF: the page rendered by pdfium, prev / next ---------------------------------------------------------

function PdfView({ backend, info, query, page, boxes, onPage, registerApi }: {
  backend: Backend; info: PreviewInfo; query: string; page: number | null; boxes: HitBox[] | null; onPage: (n: number) => void;
  registerApi: (patch: Partial<PreviewApi>) => void;
}) {
  const pages = info.pdf?.pages ?? null;
  const [n, setN] = useState(() => clampPage(page ?? 1, pages));
  useEffect(() => { onPage(n); }, [n, onPage]);
  useEffect(() => {
    registerApi({ page: (d) => setN((cur) => clampPage(cur + d, pages)) });
    return () => registerApi({ page: undefined });
  }, [registerApi, pages]);
  // The highlight belongs to the page the hit is on; other pages are drawn clean. Stored boxes
  // (OCR text, parser output) cover scanned pages; a text-layer PDF stores none, so the pane
  // asks pdfium for the query words' boxes live.
  const stored = useMemo(() => boxesForView(boxes, page, n), [boxes, page, n]);
  const [fetched, setFetched] = useState<HitBox[] | null>(null);
  const wantBoxes = page !== null && n === page && stored.length === 0 && previewTerms(query).length > 0;
  useEffect(() => {
    if (!wantBoxes) {
      setFetched(null);
      return;
    }
    let live = true;
    backend.previewPdfBoxes(info.path, n, query).then(
      (b) => { if (live) setFetched(b); },
      () => { if (live) setFetched(null); },
    );
    return () => { live = false; };
  }, [backend, info.path, n, wantBoxes, query]);
  const drawn = wantBoxes && fetched && fetched.length > 0 ? fetched : stored;
  const [url, setUrl] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    let made: string | null = null;
    setLoading(true);
    const width = pdfRenderWidth(box.current?.clientWidth ?? 600, window.devicePixelRatio || 1);
    backend.previewPdfPage(info.path, n, width).then(
      (b) => {
        if (!live) return;
        made = URL.createObjectURL(b);
        setUrl(made); setErr(null); setLoading(false);
      },
      (e) => { if (live) { setErr(errText(e)); setLoading(false); } },
    );
    return () => { live = false; };
  }, [backend, info.path, n]);

  // The object URL of a page is released when the next one has replaced it (or the view goes away).
  const prev = useRef<string | null>(null);
  useEffect(() => {
    if (prev.current && prev.current !== url) URL.revokeObjectURL(prev.current);
    prev.current = url;
  }, [url]);
  useEffect(() => () => { if (prev.current) URL.revokeObjectURL(prev.current); }, []);

  if (!info.pdf?.engine) return <OtherView info={info} note="The PDF reader is still being downloaded. Use Open for now." />;
  if (pages === null) return <OtherView info={info} note="This PDF cannot be previewed (password protected or damaged). Use Open." />;

  const go = (d: number) => setN((cur) => clampPage(cur + d, pages));
  const keep = (e: React.MouseEvent) => e.preventDefault(); // keep focus in the search box
  return (
    <div className="pv-pdf">
      <div className="pv-pdfbar">
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={n <= 1} onClick={() => go(-1)} aria-label="Previous page" title={`Previous page (${combos("preview.page")[1] ?? ""})`}>‹</button>
        <span className="pv-pageno" aria-live="polite">Page {n} of {pages}</span>
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={n >= pages} onClick={() => go(1)} aria-label="Next page" title={`Next page (${hint("preview.page")})`}>›</button>
      </div>
      <div className={"pv-pdfpage" + (loading ? " stale" : "")} ref={box}>
        {err ? (
          <div className="pv-msg" role="alert">{err}</div>
        ) : url ? (
          <ImageStage key={url} src={url} name={`${info.name}, page ${n}`} dims={null} note={null} boxes={drawn} onError={() => setErr("This page cannot be shown.")} registerApi={registerApi} />
        ) : (
          <div className="pv-msg"><span className="spin" aria-label="Loading" /></div>
        )}
      </div>
    </div>
  );
}

// ---- on-demand document parser: extract text, copy a table, copy as Markdown ----------------------------------

function ParseBar({ backend, path, page, available, dim, registerApi }: {
  backend: Backend; path: string; page: number | null; available: boolean; dim?: boolean; registerApi: (patch: Partial<PreviewApi>) => void;
}) {
  const [doc, setDoc] = useState<ParsedDoc | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; bad: boolean } | null>(null);
  const [panel, setPanel] = useState(false);
  const [pick, setPick] = useState(false);
  const key = `${path}#${page ?? 0}`;
  const run = useRef(0);
  // Another file or page: forget the result and ignore an answer that is still on its way.
  useEffect(() => {
    run.current++;
    setDoc(null); setBusy(false); setMsg(null); setPanel(false); setPick(false);
  }, [key]);
  useEffect(() => {
    if (!msg || msg.bad) return;
    const t = window.setTimeout(() => setMsg(null), 2500);
    return () => window.clearTimeout(t);
  }, [msg]);

  if (!available) {
    return (
      <div className={"pv-parse" + (dim ? " stale" : "")}>
        <div className="pv-parse-bar">
          <button className="btn quiet pv-btn" tabIndex={-1} disabled title="The document parser is an optional download">Install the document parser in Setup</button>
        </div>
      </div>
    );
  }

  const load = async (): Promise<ParsedDoc | null> => {
    if (doc) return doc;
    const my = ++run.current;
    setBusy(true);
    setMsg(null);
    try {
      const d = await backend.parseDocument(path, page ?? undefined);
      if (my !== run.current) return null;
      setDoc(d);
      return d;
    } catch (e) {
      if (my === run.current) setMsg({ text: errText(e), bad: true });
      return null;
    } finally {
      if (my === run.current) setBusy(false);
    }
  };
  const copy = async (text: string, html: string | null, what: string) => {
    try { await backend.copyTextHtml(text, html); setMsg({ text: `${what} copied`, bad: false }); }
    catch (e) { setMsg({ text: errText(e), bad: true }); }
  };
  const extract = async () => { const d = await load(); if (d) { setPick(false); setPanel(true); } };
  // Ctrl+E runs the extract action; Ctrl+Shift+X copies what the panel shows (PRD B6).
  // Re-registered every render: the closures must see the current doc / copy helpers.
  useEffect(() => {
    registerApi({ extract: () => void extract(), copyText: () => { if (doc) void copy(doc.markdown, null, "Text"); } });
    return () => registerApi({ extract: undefined, copyText: undefined });
  });
  const copyTable = async () => {
    const d = await load();
    if (!d) return;
    setPanel(false);
    if (d.tables.length === 0) setMsg({ text: "No table found here", bad: false });
    else if (d.tables.length === 1) await copy(d.tables[0].tsv, d.tables[0].html, "Table");
    else setPick(true);
  };
  const copyMarkdown = async () => { const d = await load(); if (d) await copy(d.markdown, null, "Markdown"); };

  const keep = (e: React.MouseEvent) => e.preventDefault(); // keep focus in the search box
  const btn = (label: string, action: () => Promise<void>, title: string) => (
    <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={busy} onClick={() => void action()} title={title}>{label}</button>
  );
  return (
    <div className={"pv-parse" + (dim ? " stale" : "")}>
      <div className="pv-parse-bar">
        {btn("Extract text", extract, `Read the text on this page with the document parser (${hint("preview.extract")})`)}
        {btn("Copy table", copyTable, "Copy the table on this page, ready to paste into a spreadsheet")}
        {btn("Copy as Markdown", copyMarkdown, "Copy the whole page as Markdown")}
        {busy && <span className="pv-parse-state"><span className="spin" aria-hidden /> Reading the page…</span>}
        {msg && <span className={"pv-parse-state" + (msg.bad ? " bad" : "")} role={msg.bad ? "alert" : "status"}>{msg.text}</span>}
      </div>
      {pick && doc && (
        <div className="pv-pick" role="menu" aria-label="Tables on this page">
          {doc.tables.map((t, i) => (
            <button key={i} className="btn quiet pv-btn" role="menuitem" tabIndex={-1} onMouseDown={keep}
                    onClick={() => { setPick(false); void copy(t.tsv, t.html, `Table ${i + 1}`); }}>{tableLabel(i, t.tsv)}</button>
          ))}
        </div>
      )}
      {panel && doc && (
        <div className="pv-parse-panel">
          <div className="pv-parse-head">
            <b>Extracted text</b>
            <span className="muted">{doc.elements_count} block{doc.elements_count === 1 ? "" : "s"}</span>
            <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={!doc.markdown} onClick={() => void copy(doc.markdown, null, "Text")}>Copy</button>
            <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} onClick={() => setPanel(false)} aria-label="Close the extracted text">Close</button>
          </div>
          <pre>{doc.markdown || "No text found on this page."}</pre>
        </div>
      )}
    </div>
  );
}

// ---- video / audio ------------------------------------------------------------------------------------

function MediaView({ info, start }: { info: PreviewInfo; start?: number }) {
  const ref = useRef<HTMLMediaElement>(null);
  const [failed, setFailed] = useState(false);
  const [playing, setPlaying] = useState(false);

  const url = info.src;
  useEffect(() => {
    const el = ref.current;
    if (!el || !url) return;
    if (el.getAttribute("src") !== url) el.src = url; // re-arm after StrictMode's simulated unmount
    return () => {
      // Unmounting (selection moved on) must stop the download as well as the sound.
      el.pause();
      el.removeAttribute("src");
      el.load();
    };
  }, [url]);

  if (!info.src || failed) return <OtherView info={info} note="This format cannot be played here. Use Open." />;
  const common = {
    src: info.src, controls: true, preload: "metadata" as const,
    onError: () => setFailed(true),
    // The hit's moment: seek there as soon as the clip knows its length (the matching window or shot starts here).
    onLoadedMetadata: (e: React.SyntheticEvent<HTMLMediaElement>) => {
      const t = seekTarget(start, e.currentTarget.duration);
      if (t !== null) e.currentTarget.currentTime = t;
    },
  };
  const anchor = seekTarget(start, Infinity);
  /** The "Match at 1:23" button: back to the hit's moment, and play from there. */
  const jump = () => {
    const el = ref.current;
    const t = seekTarget(start, el?.duration ?? NaN);
    if (!el || t === null) return;
    el.currentTime = t;
    void el.play().catch(() => {});
  };
  const at = anchor !== null && (
    <button className="btn quiet pv-at" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={jump} title="Jump to the moment that matched">
      Match at {formatClock(anchor)}
    </button>
  );
  return info.kind === "video" ? (
    <div className="pv-media">
      <video ref={ref as React.RefObject<HTMLVideoElement>} {...common} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} />
      {!playing && (
        <span className="playb" aria-hidden>
          <svg viewBox="0 0 24 24" fill="currentColor"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l11-6.5a1 1 0 0 0 0-1.72l-11-6.5A1 1 0 0 0 8 5.5Z" /></svg>
        </span>
      )}
      {at}
    </div>
  ) : (
    <div className="pv-media audio">
      <div className="wave" aria-hidden>{WAVE.map((h, j) => <i key={j} style={{ height: `${h}%` }} />)}</div>
      <audio ref={ref as React.RefObject<HTMLAudioElement>} {...common} />
      {at}
    </div>
  );
}

const WAVE = Array.from({ length: 52 }, (_, j) => 16 + Math.abs(Math.sin(j * 0.7) * 52 + Math.sin(j * 2.3) * 28));

// ---- text / code --------------------------------------------------------------------------------------

function TextView({ info, query }: { info: PreviewInfo; query: string }) {
  const box = useRef<HTMLDivElement>(null);
  const t = info.text;
  const { segments, hits } = useMemo(() => highlight(t?.content ?? "", previewTerms(query)), [t, query]);

  // Scroll to the first hit (a third of the way down, so some context above it stays visible).
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const m = el.querySelector("mark");
    el.scrollTop = m ? Math.max(0, (m as HTMLElement).offsetTop - el.clientHeight / 3) : 0;
  }, [segments]);

  if (!t) return <OtherView info={info} />;
  return (
    <div className="pv-text" ref={box}>
      {hits > 0 && <div className="pv-hits">{hits >= 2000 ? "2000+" : hits} match{hits === 1 ? "" : "es"}</div>}
      <pre>{segments.map((s, i) => (s.hit ? <mark key={i}>{s.text}</mark> : s.text))}</pre>
      {t.truncated && <div className="pv-trunc">Showing the beginning of the file</div>}
    </div>
  );
}

// ---- everything else: big icon / thumbnail + facts --------------------------------------------------------

function OtherView({ info, note }: { info: PreviewInfo; note?: string }) {
  const [broken, setBroken] = useState(false);
  return (
    <div className="pv-other">
      <div className="pv-bigicon">
        {info.thumb && !broken ? (
          <img src={info.thumb} alt="" draggable={false} onError={() => setBroken(true)} />
        ) : (
          <FileIcon name={info.name} isDir={info.kind === "folder"} src={null} />
        )}
      </div>
      {note && <div className="pv-note">{note}</div>}
    </div>
  );
}
