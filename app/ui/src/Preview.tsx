import { useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { Highlight } from "prism-react-renderer";
import { renderAsync } from "docx-preview";
import "./prismLangs";
import type { Backend, Hit, HitBox, ParsedDoc, PreviewInfo } from "./types";
import { FileIcon } from "./FileIcon";
import { Snippet } from "./Semantic";
import { errText, formatDate, formatSize } from "./format";
import { makeMatcher, pathKey } from "./fsDelta";
import { useFsDeltaListener } from "./useFsDelta";
import { boxesForView, boxRect, clampPage, clampView, codeLanguage, crumbParts, extOf, fenceLanguage, fitScale, isMarkdown, isWordDoc, kindLabel, highlight, markTextNodes, matchRanges, MAX_SCALE, pdfRenderWidth, prettyJson, previewSelectionText, previewTerms, tableLabel, toggleActual, zoomAt, type MdNode, type View } from "./previewLogic";
import { combos, hint } from "./keymap";
import { tooltip } from "./Shortcuts";
import { MediaPlayer } from "./MediaPlayer";


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
  /** "NN% match" of a meaning hit: the same number as its row badge. */
  matchPct?: number | null;
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
export function Preview({ backend, hit, query, snippet, start, page, boxes, matchPct, onOpen, onReveal, onCopyPath, apiRef }: PreviewProps) {
  // Children (PDF view, image stage, parse bar) contribute their piece of the keyboard API.
  const apiParts = useRef<Partial<PreviewApi>>({});
  const registerApi = useCallback((patch: Partial<PreviewApi>) => { apiParts.current = { ...apiParts.current, ...patch }; }, []);
  // The parent's api ref is filled with forwarding closures over the children's registered parts;
  // useImperativeHandle is the sanctioned way to write a parent-provided ref (null ref = no-op),
  // and it clears the ref on unmount exactly like the manual cleanup did.
  useImperativeHandle(apiRef ?? null, () => ({
    page: (d) => apiParts.current.page?.(d),
    zoom: (f) => apiParts.current.zoom?.(f),
    fit: () => apiParts.current.fit?.(),
    pan: (dx, dy) => apiParts.current.pan?.(dx, dy),
    extract: () => apiParts.current.extract?.(),
    copyText: () => apiParts.current.copyText?.(),
  }));
  const [info, setInfo] = useState<PreviewInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // The PDF page being shown (the actions read that page) and whether the optional document parser is installed.
  const [pdfPage, setPdfPage] = useState<number | null>(null);
  const [parser, setParser] = useState<boolean | null>(null);
  // Right-click over selected text in the pane -> the copy menu (see CopyMenu).
  const [copyMenu, setCopyMenu] = useState<{ x: number; y: number; text: string } | null>(null);
  const pvRef = useRef<HTMLElement>(null);
  const copyText = useCallback((t: string) => backend.copyTextHtml(t, null), [backend]);
  const onContext = (e: React.MouseEvent) => {
    const text = previewSelectionText(document.getSelection(), pvRef.current);
    if (!text) return; // nothing selected: the global suppressor keeps right-click quiet, as before
    e.preventDefault();
    e.stopPropagation();
    setCopyMenu({ x: e.clientX, y: e.clientY, text });
  };

  useEffect(() => {
    let live = true;
    backend.parserPresent().then((p) => { if (live) setParser(p); }, () => { if (live) setParser(false); });
    return () => { live = false; };
  }, [backend, hit.path]);

  // The file under the pane changed on disk (an fs-delta `modified`: `rev` re-reads it and its bytes) or went away
  // (`gone`: said plainly, until the parent moves the selection). A rename reaches us as a new `hit.path`.
  const [rev, setRev] = useState(0);
  const [gone, setGone] = useState(false);
  const loadedPath = useRef<string | null>(null);
  const pathRef = useRef(hit.path);
  useEffect(() => { pathRef.current = hit.path; setGone(false); }, [hit.path]);
  useFsDeltaListener((d) => {
    const m = makeMatcher(d);
    if (m.idle) return;
    if (m.isGone(pathRef.current)) { setGone(true); return; }
    const k = pathKey(pathRef.current);
    if (d.modified.some((p) => pathKey(p) === k)) setRev((r) => r + 1);
  });

  useEffect(() => {
    let live = true;
    // A re-read of the same file keeps the stage on screen (no spinner flash); a new file shows the loader as before.
    if (loadedPath.current !== hit.path) setLoading(true);
    loadedPath.current = hit.path;
    setPdfPage(null);
    backend.previewInfo(hit.path).then(
      (i) => { if (live) { setInfo(i); setErr(null); setLoading(false); } },
      (e) => { if (live) { setInfo(null); setErr(errText(e)); setLoading(false); } },
    );
    return () => { live = false; };
  }, [backend, hit.path, rev]);

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
    <aside className="pv" aria-label={`Preview of ${hit.name}`} ref={pvRef} onContextMenu={onContext}>
      <div className="pv-col">
        <div className={"pbox" + (stale ? " stale" : "")}>
          {gone ? (
            <div className="pv-msg" role="status">This file was moved or deleted.</div>
          ) : err ? (
            <div className="pv-msg" role="alert">{err}</div>
          ) : info ? (
            <Body key={info.path + "#" + (info.mtime ?? 0) + "#" + (page ?? 0) + "@" + (start ?? 0)} backend={backend} info={info} query={query} start={start} page={page ?? null}
                  boxes={boxes ?? null} onPage={setPdfPage} registerApi={registerApi} />
          ) : (
            <div className="pv-msg"><span className="spin" aria-label="Loading" /></div>
          )}
        </div>
        <div className="pv-title">
          <h2 title={hit.name} dir="auto">{hit.name}</h2>
          <div className="kind">{label}</div>
          {matchPct != null && (
            <span className="pv-pct" title="How closely this file matches what you typed">{matchPct}% match{page ? ` · page ${page}` : ""}</span>
          )}
        </div>
        <Crumbs key={hit.path} parts={crumbs} path={hit.path} />
        {snippet && (
          <div className="whyb"><b>Why it&rsquo;s here</b><Snippet text={snippet} /></div>
        )}
        {metaInfo && <Meta info={metaInfo} folderSize={hit.is_dir ? hit.size : null} dim={stale} />}
        {metaInfo && parser !== null && (metaInfo.kind === "image" || (metaInfo.kind === "pdf" && metaInfo.pdf?.engine && metaInfo.pdf.pages !== null)) && (
          <ParseBar backend={backend} path={metaInfo.path} available={parser} dim={stale} registerApi={registerApi} query={query}
                    page={metaInfo.kind === "pdf" ? (pdfPage ?? clampPage(page ?? 1, metaInfo.pdf?.pages ?? null)) : null} />
        )}
        <div className="btns">
          <button className="btn pri" tabIndex={-1} onMouseDown={keep} onClick={onOpen} title={tooltip("preview.open")}>Open <kbd>↵</kbd></button>
          <button className="btn" tabIndex={-1} onMouseDown={keep} onClick={onReveal} title={tooltip("preview.reveal")}>Show in folder</button>
          <button className="btn" tabIndex={-1} onMouseDown={keep} onClick={onCopyPath} title={tooltip("preview.copyPath")}>Copy path</button>
        </div>
      </div>
      {copyMenu && <CopyMenu x={copyMenu.x} y={copyMenu.y} text={copyMenu.text} copy={copyText} onClose={() => setCopyMenu(null)} />}
    </aside>
  );
}

/** Where the file lives, on ONE row. A short path shows every folder; a long one keeps the drive,
 *  a "…+N" chip and the last two folders (the part that says "where"), and the chip opens the full
 *  path wrapped in place — nothing is ever cut off for good. */
function Crumbs({ parts, path }: { parts: string[]; path: string }) {
  const [open, setOpen] = useState(false);
  const long = parts.length > 4;
  const item = (p: string, i: number, last: boolean) => last
    ? <b key={i}>{p}</b>
    : <span key={i} className="cr"><span>{p}</span><i>›</i></span>;
  let body: React.ReactNode;
  if (!long || open) {
    body = parts.map((p, i) => item(p, i, i === parts.length - 1));
  } else {
    const hidden = parts.length - 3;
    body = [
      item(parts[0]!, 0, false),
      <span key="more" className="cr">
        <button type="button" className="cr-more" tabIndex={-1} onMouseDown={(e) => e.preventDefault()}
                onClick={() => setOpen(true)} title={"Show the full path\n" + path}>…&thinsp;+{hidden}</button><i>›</i>
      </span>,
      item(parts[parts.length - 2]!, parts.length - 2, false),
      item(parts[parts.length - 1]!, parts.length - 1, true),
    ];
  }
  return (
    <div className={"crumbs" + (open ? " open" : "") + (long && !open ? " collapsed" : "")} title={path}>
      {body}
      {long && open && (
        <button type="button" className="cr-less" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => setOpen(false)}>Collapse</button>
      )}
    </div>
  );
}

/** Compact facts as one wrapping strip (label dim, value bold), so the stage keeps the height. Noise is left out: the drive
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
      {cells.map(([k, v]) => (
        <div key={k}>
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
    case "text": return <TextView info={info} query={query} />;
    // A document renders as a real Word file when it can (.docx family); everything else
    // (xlsx / pptx / ODF / RTF / legacy .doc) reads like the text file it was indexed as.
    case "document": return <DocView info={info} query={query} />;
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
        <div className="pv-zoombar ov" onPointerDown={(e) => e.stopPropagation()}>
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

function ParseBar({ backend, path, page, available, dim, registerApi, query }: {
  backend: Backend; path: string; page: number | null; available: boolean; dim?: boolean; registerApi: (patch: Partial<PreviewApi>) => void; query: string;
}) {
  const [doc, setDoc] = useState<ParsedDoc | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; bad: boolean } | null>(null);
  const [panel, setPanel] = useState(false);
  const [pick, setPick] = useState(false);
  // The extracted markdown reads formatted by default; "Plain" shows the raw markdown source.
  const [plain, setPlain] = useState(false);
  const terms = useMemo(() => previewTerms(query), [query]);
  const key = `${path}#${page ?? 0}`;
  const run = useRef(0);
  // Another file or page: forget the result and ignore an answer that is still on its way.
  useEffect(() => {
    run.current++;
    setDoc(null); setBusy(false); setMsg(null); setPanel(false); setPick(false); setPlain(false);
  }, [key]);
  useEffect(() => {
    if (!msg || msg.bad) return;
    const t = window.setTimeout(() => setMsg(null), 2500);
    return () => window.clearTimeout(t);
  }, [msg]);
  // Ctrl+E runs the extract action; Ctrl+Shift+X copies what the panel shows (PRD B6).
  // Re-registered every render: the closures must see the current doc / copy helpers.
  // Registered before the early return below: hook order must not depend on `available`
  // (the parser can finish downloading while this bar is on screen). Until then the
  // shortcuts stay unbound, exactly as when the bar took the early return.
  useEffect(() => {
    registerApi(available
      ? { extract: () => void extract(), copyText: () => { if (doc) void copy(doc.markdown, null, "Text"); } }
      : { extract: undefined, copyText: undefined });
    return () => registerApi({ extract: undefined, copyText: undefined });
  });

  if (!available) {
    return (
      <div className={"pv-parse" + (dim ? " stale" : "")}>
        <div className="pv-parse-bar">
          <button className="btn quiet pv-btn" tabIndex={-1} disabled title="The document parser is an optional download">Install the document parser in Setup</button>
        </div>
      </div>
    );
  }

  const load = async (force = false): Promise<ParsedDoc | null> => {
    if (doc && !force) return doc;
    const my = ++run.current;
    setBusy(true);
    setMsg(null);
    try {
      const d = await backend.parseDocument(path, page ?? undefined, force);
      if (my !== run.current) return null;
      setDoc(d);
      if (force) setMsg({ text: "Re-extracted", bad: false });
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
  const reextract = async () => { const d = await load(true); if (d) { setPick(false); setPanel(true); } };
  const copyTable = async () => {
    const d = await load();
    if (!d) return;
    setPanel(false);
    if (d.tables.length === 0) setMsg({ text: "No table found here", bad: false });
    else if (d.tables.length === 1) await copy(d.tables[0]!.tsv, d.tables[0]!.html, "Table");
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
        {btn("Re-extract", reextract, "Read the page again, ignoring the saved extraction")}
        {busy && <span className="pv-parse-state"><span className="spin" aria-hidden /> Reading the page…</span>}
        {msg && <span className={"pv-parse-state" + (msg.bad ? " bad" : "")} role={msg.bad ? "alert" : "status"}>{msg.text}</span>}
        {doc?.cached && !busy && !msg && (
          <span className="pv-parse-state" title="Answered from the saved extraction — Re-extract reads the page again">from cache</span>
        )}
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
            <div className="dseg" role="group" aria-label="How the extracted text is shown">
              <button className={plain ? "" : "on"} tabIndex={-1} aria-pressed={!plain} onMouseDown={keep} onClick={() => setPlain(false)}>Rendered</button>
              <button className={plain ? "on" : ""} tabIndex={-1} aria-pressed={plain} onMouseDown={keep} onClick={() => setPlain(true)}>Plain</button>
            </div>
            <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={!doc.markdown} onClick={() => void copy(doc.markdown, null, "Text")}>Copy</button>
            <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} onClick={() => setPanel(false)} aria-label="Close the extracted text">Close</button>
          </div>
          {plain ? (
            <pre>{doc.markdown || "No text found on this page."}</pre>
          ) : (
            <div className="pv-parse-md">
              {doc.markdown
                ? <MdView content={doc.markdown} terms={terms} />
                : <div className="pv-md">No text found on this page.</div>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ---- video / audio ------------------------------------------------------------------------------------

function MediaView({ info, start }: { info: PreviewInfo; start?: number }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [info.src]);
  if (!info.src || failed) return <OtherView info={info} note="This format cannot be played here. Use Open." />;
  return <MediaPlayer kind={info.kind === "video" ? "video" : "audio"} src={info.src} name={info.name} start={start} onError={() => setFailed(true)} />;
}

// ---- text / code: formatted Markdown or Prism-highlighted source ---------------------------------------

/** Which way a Markdown file is shown: formatted ("Preview") or highlighted source ("Source"). */
type TextViewMode = "preview" | "source";

/** One Range over the plain view's single text node — or null when the offsets cannot be
 *  honored. Guards the node type and clamps to the text length: a wrong node (a rich view
 *  reached the fallback without a <mark>, so its offsets point into raw content, not this
 *  node) must degrade to "no aim" instead of throwing a Range error ("There is no child at
 *  offset N") that takes the whole app down through the error boundary. */
function plainRange(node: Node, start: number, end: number): Range | null {
  if (node.nodeType !== Node.TEXT_NODE) return null;
  const len = node.textContent?.length ?? 0;
  const s = Math.max(0, Math.min(start, len));
  const e = Math.max(s, Math.min(end, len));
  if (s >= e) return null;
  const r = document.createRange();
  r.setStart(node, s);
  r.setEnd(node, e);
  return r;
}

function TextView({ info, query }: { info: PreviewInfo; query: string }) {
  const box = useRef<HTMLDivElement>(null);
  const t = info.text;
  const md = isMarkdown(extOf(info.name));
  const lang = codeLanguage(extOf(info.name));
  const terms = useMemo(() => previewTerms(query), [query]);
  // JSON is re-indented for reading; anything unparseable (or a bare scalar) shows raw.
  const content = useMemo(
    () => (t && lang === "json" ? prettyJson(t.content) ?? t.content : t?.content ?? ""),
    [t, lang],
  );
  // The plain text view paints its hits over its single text node (see the registry effect);
  // the rich views still split their own nodes on the same matcher via highlight().
  const { ranges, hits } = useMemo(() => matchRanges(content, terms), [content, terms]);
  const [mode, setMode] = useState<TextViewMode>("preview");

  // Scroll to the first hit (a third of the way down, so some context above it stays visible).
  // Rich views own real <mark> elements; the plain view aims at the first range of its own
  // text node (pre.pv-plain). A rich view without a mark (a term split across Prism tokens or
  // markdown syntax) has no node its raw-content offsets could aim at — it keeps the scroll
  // position instead of throwing a Range error that would take the whole app down.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const m = el.querySelector("mark");
    if (m) { el.scrollTop = Math.max(0, (m as HTMLElement).offsetTop - el.clientHeight / 3); return; }
    const node = el.querySelector("pre.pv-plain")?.firstChild;
    const r = node && ranges.length > 0 ? plainRange(node, ranges[0]![0], ranges[0]![1]) : null;
    if (!r) { el.scrollTop = 0; return; }
    el.scrollTop = Math.max(0, r.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop - el.clientHeight / 3);
  }, [ranges, mode]);

  // Register the plain view's hits in the document highlight registry while its <pre> is on
  // screen; a term change swaps ranges without re-rendering a single node. Only the plain view
  // registers (rich views paint real <mark> elements), and every range goes through plainRange
  // so an unexpected node can never throw here.
  useEffect(() => {
    const registry = (CSS as { highlights?: Map<string, unknown> }).highlights;
    if (!registry) return; // older engine: hits stay unpainted, the count chip still reports them
    const pre = box.current?.querySelector("pre.pv-plain");
    const node = pre?.firstChild;
    const parts = node && ranges.length > 0
      ? ranges.flatMap(([s, e]) => { const r = plainRange(node, s, e); return r ? [r] : []; })
      : [];
    if (parts.length === 0) { registry.delete("pv-hits"); return; }
    registry.set("pv-hits", new (window as unknown as { Highlight: new (...r: Range[]) => unknown }).Highlight(...parts));
    return () => { registry.delete("pv-hits"); };
  }, [ranges, mode]);

  if (!t) return <OtherView info={info} />;
  const keep = (e: React.MouseEvent) => e.preventDefault(); // keep focus in the search box
  return (
    <div className="pv-text" ref={box}>
      {(md || hits > 0) && (
        <div className="pv-textbar">
          {md && (
            <div className="dseg" role="group" aria-label="How the Markdown file is shown">
              <button className={mode === "preview" ? "on" : ""} tabIndex={-1} aria-pressed={mode === "preview"}
                      onMouseDown={keep} onClick={() => setMode("preview")}>Preview</button>
              <button className={mode === "source" ? "on" : ""} tabIndex={-1} aria-pressed={mode === "source"}
                      onMouseDown={keep} onClick={() => setMode("source")}>Source</button>
            </div>
          )}
          {hits > 0 && <span className="pv-hits">{hits >= 2000 ? "2000+" : hits} match{hits === 1 ? "" : "es"}</span>}
        </div>
      )}
      {md && mode === "preview" ? (
        <MdView content={t.content} terms={terms} />
      ) : lang ? (
        <CodeBlock content={content} lang={lang} terms={terms} numbers />
      ) : (
        <pre className="pv-plain">{content}</pre>
      )}
      {t.truncated && <div className="pv-trunc">Showing the beginning of the file</div>}
    </div>
  );
}

const REMARK_GFM = [remarkGfm];

/** Formatted Markdown (GFM: tables, task lists, strikethrough, autolinks). Query terms are
 *  marked inside the rendered text — the same segments the source views use, so one highlighter
 *  decides what a hit is everywhere. */
function MdView({ content, terms }: { content: string; terms: string[] }) {
  // A unified "plugin" is an attacher that RETURNS the tree transformer (the attacher itself runs
  // once at use-time without a tree). Memoised so a plain re-render never re-parses.
  const rehypeMarks = useMemo(() => [() => (tree: unknown) => { markTextNodes(tree as MdNode, terms); }], [terms]);
  const components = useMemo<Components>(() => ({
    // <pre> unwraps: fenced code renders its own block through `code` below.
    pre: ({ children }) => <>{children}</>,
    code: (props) => <MdCode {...props} terms={terms} />,
  }), [terms]);
  return (
    <div className="pv-md">
      <Markdown remarkPlugins={REMARK_GFM} rehypePlugins={rehypeMarks} components={components}>{content}</Markdown>
    </div>
  );
}

/** Fenced code inside rendered Markdown: highlighted through the fence's info string. Inline
 *  code stays styled text; a fence with an unknown language renders as a plain block. */
function MdCode({ className, children, terms }: { className?: string; children?: React.ReactNode; terms?: string[] }) {
  const info = /language-([\w#+.-]+)/.exec(className ?? "")?.[1] ?? null;
  const lang = info ? fenceLanguage(info) : null;
  if (!info) return <code>{children}</code>;
  if (!lang) return <pre className="pv-mdblock"><code>{children}</code></pre>;
  return <CodeBlock content={String(children).replace(/\n$/, "")} lang={lang} terms={terms} numbers={false} />;
}

/** Prism-highlighted source, one row per line. Query terms are marked inside the tokens (a term
 *  split by the tokenizer across two tokens loses its mark — the match count stays complete).
 *  Line numbers: a sticky CSS counter, never selected, never copied — whole-file views only. */
function CodeBlock({ content, lang, terms, numbers }: { content: string; lang: string; terms?: string[]; numbers: boolean }) {
  return (
    <Highlight code={content} language={lang}>
      {({ tokens }) => (
        <pre className={"pv-code " + (numbers ? "num" : "bare")} dir="ltr">
          {tokens.map((line, i) => (
            <code className="line" key={i}>
              {line.map((token, k) => {
                const type = token.types.find((x) => x !== "plain") ?? "";
                return <span className={type ? `tok-${type}` : undefined} key={k}>{termMarks(token.content, terms)}</span>;
              })}
            </code>
          ))}
        </pre>
      )}
    </Highlight>
  );
}

/** Query-term marks inside one Prism token (the same highlight() as everywhere else). */
function termMarks(text: string, terms: string[] | undefined): React.ReactNode {
  if (!terms || terms.length === 0) return text;
  const { segments } = highlight(text, terms);
  if (!segments.some((s) => s.hit)) return text;
  return segments.map((s, i) => (s.hit ? <mark key={i}>{s.text}</mark> : s.text));
}

// ---- Word documents -----------------------------------------------------------------------------------

/** `wsfile:` answers without a Range in one piece up to this size (FULL_MAX in preview.rs):
 *  bigger files arrive short and no ZIP reader can open them. */
const DOCX_ONE_PIECE_MAX = 24 * 1024 * 1024;

/** A document preview: the .docx family renders with real Word layout (docx-preview over the
 *  streamed bytes); anything else (xlsx / pptx / ODF / RTF / legacy .doc) reads like the text
 *  file it was indexed as. */
function DocView({ info, query }: { info: PreviewInfo; query: string }) {
  const word = isWordDoc(extOf(info.name)) && info.src !== null;
  return word ? <DocxView info={info} query={query} /> : <TextView info={info} query={query} />;
}

/** Word layout for one .docx: fetch the bytes, hand them to the renderer, keep the extracted
 *  text as the fallback when the bytes are out of reach (protocol refusal, or a file bigger
 *  than the protocol's one-piece cap) or the file is not a docx after all. */
function DocxView({ info, query }: { info: PreviewInfo; query: string }) {
  const host = useRef<HTMLDivElement>(null);
  const [dead, setDead] = useState(false);
  useEffect(() => {
    const made = host.current;
    if (!made || !info.src) return;
    let live = true;
    setDead(false);
    void (async () => {
      try {
        const res = await fetch(info.src!);
        if (!res.ok) throw new Error(`bytes ${res.status}`);
        const buf = await res.arrayBuffer();
        // Only files above the one-piece cap can arrive short; smaller ones always come whole.
        if (info.size !== null && info.size > DOCX_ONE_PIECE_MAX && buf.byteLength < info.size) {
          throw new Error("bytes truncated");
        }
        if (!live) return;
        made.replaceChildren();
        await renderAsync(buf, made, undefined, { inWrapper: true });
      } catch (e) {
        if (live) { setDead(true); console.info("[preview] docx bytes unusable, showing the extracted text", e); }
      }
    })();
    return () => { live = false; };
  }, [info.src, info.size]);
  if (dead) return <TextView info={info} query={query} />;
  return (
    <div className="pv-docx">
      <div className="pv-docx-pages" ref={host} />
    </div>
  );
}

// ---- copy the selected preview text -------------------------------------------------------------------

/** Right-click over selected text in the pane: Copy / Select all. The app suppresses the native
 *  webview menu, so without this a selection would have no way out — and focus usually sits in
 *  the search box, which is why Copy goes through the backend clipboard, not a native copy. */
function CopyMenu({ x, y, text, copy, onClose }: {
  x: number; y: number; text: string; copy: (t: string) => Promise<void>; onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [msg, setMsg] = useState<"copied" | "failed" | null>(null);
  useEffect(() => {
    const press = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) onClose(); };
    // Capture phase, and Escape must not also close Quick Look behind the menu.
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); }
    };
    window.addEventListener("mousedown", press, true);
    window.addEventListener("keydown", key, true);
    return () => { window.removeEventListener("mousedown", press, true); window.removeEventListener("keydown", key, true); };
  }, [onClose]);
  useEffect(() => {
    if (msg !== "copied") return;
    const t = window.setTimeout(onClose, 700);
    return () => window.clearTimeout(t);
  }, [msg, onClose]);
  const selectAll = () => {
    const pane = ref.current?.closest(".pv");
    const root = pane?.querySelector(".pbox") ?? pane;
    if (!root) return;
    const r = document.createRange();
    r.selectNodeContents(root);
    const s = window.getSelection();
    s?.removeAllRanges();
    s?.addRange(r);
    onClose();
  };
  return (
    <div className="pv-pick pv-copymenu" role="menu" aria-label="Selected text" style={{ left: Math.min(x, window.innerWidth - 150), top: Math.min(y, window.innerHeight - 100) }} ref={ref}>
      <button className="btn quiet pv-btn" role="menuitem" tabIndex={-1}
              onClick={() => { void copy(text).then(() => setMsg("copied"), () => setMsg("failed")); }}>
        {msg === "copied" ? "Copied ✓" : msg === "failed" ? "Copy failed" : "Copy"}
      </button>
      <button className="btn quiet pv-btn" role="menuitem" tabIndex={-1} onClick={selectAll}>Select all</button>
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
