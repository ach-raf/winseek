import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Backend, Hit, HitBox, ParsedDoc, PreviewInfo } from "./types";
import { FileIcon } from "./FileIcon";
import { Snippet } from "./Semantic";
import { formatDate, formatSize, splitDir } from "./format";
import { boxesForView, boxRect, clampPage, clampView, fitScale, highlight, pdfRenderWidth, previewTerms, tableLabel, toggleActual, zoomAt, type View } from "./previewLogic";

const errText = (e: unknown) => String(e instanceof Error ? e.message : e).replace(/^Error:\s*/, "");

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
}

/**
 * Space preview. Loads `preview_info` for the hit; a newer hit supersedes an older one (the older answer is
 * dropped and its media elements are unmounted, which aborts their downloads). The previous content stays,
 * dimmed, until the new one arrives so arrowing through results does not flash.
 */
export function Preview({ backend, hit, query, snippet, start, page, boxes, onOpen, onReveal, onCopyPath }: PreviewProps) {
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
  const { head, tail } = splitDir(hit.path);

  return (
    <aside className="pv" aria-label={`Preview of ${hit.name}`}>
      <header className="pv-head">
        <FileIcon name={hit.name} isDir={hit.is_dir} src={backend.iconUrl(hit.path, hit.is_dir, false)} />
        <div className="pv-title">
          <div className="pv-name" title={hit.name}>{hit.name}</div>
          <div className="pv-path" title={hit.path}><span className="head">{head}</span><span className="tail">{tail}</span></div>
        </div>
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onCopyPath} title="Copy the full path (Ctrl+Shift+C)">Copy path</button>
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onReveal} title="Show in Explorer (Ctrl+Enter)">Reveal</button>
        <button className="btn primary pv-btn" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={onOpen} title="Open (Enter)">Open</button>
      </header>
      {snippet && (
        <div className="pv-snip" title="Text found inside the file"><Snippet text={snippet} /></div>
      )}
      <div className={"pv-body" + (stale ? " stale" : "")}>
        {err ? (
          <div className="pv-msg" role="alert">{err}</div>
        ) : info ? (
          <Body key={info.path + "#" + (page ?? 0)} backend={backend} info={info} query={query} start={start} page={page ?? null}
                boxes={boxes ?? null} onPage={setPdfPage} />
        ) : (
          <div className="pv-msg"><span className="spin" aria-label="Loading" /></div>
        )}
      </div>
      {shown && parser !== null && (shown.kind === "image" || (shown.kind === "pdf" && shown.pdf?.engine && shown.pdf.pages !== null)) && (
        <ParseBar backend={backend} path={shown.path} available={parser}
                  page={shown.kind === "pdf" ? (pdfPage ?? clampPage(page ?? 1, shown.pdf?.pages ?? null)) : null} />
      )}
      {shown && <Facts info={shown} />}
    </aside>
  );
}

function Facts({ info }: { info: PreviewInfo }) {
  const parts: string[] = [];
  const im = info.image;
  if (im?.width && im.height) parts.push(`${im.width} × ${im.height}`);
  if (info.size !== null) parts.push(formatSize(info.size));
  if (info.mtime !== null) parts.push(`Modified ${formatDate(info.mtime)}`);
  if (info.kind === "text" && info.text) parts.push(info.text.encoding);
  const ex = im?.exif;
  return (
    <footer className="pv-facts">
      <div>{parts.join(" · ")}</div>
      {im && (
        <div className="exif">
          {ex ? (
            <>
              {ex.date && <span>{ex.date}</span>}
              {ex.camera && <span>{ex.camera}</span>}
              <span>GPS: {ex.gps ? "yes" : "no"}</span>
            </>
          ) : (
            <span>No EXIF data</span>
          )}
        </div>
      )}
    </footer>
  );
}

function Body({ backend, info, query, start, page, boxes, onPage }: {
  backend: Backend; info: PreviewInfo; query: string; start?: number; page: number | null; boxes: HitBox[] | null; onPage: (n: number) => void;
}) {
  switch (info.kind) {
    case "pdf": return <PdfView backend={backend} info={info} query={query} page={page} boxes={boxes} onPage={onPage} />;
    case "image": return <ImageView info={info} boxes={boxes} />;
    case "video": case "audio": return <MediaView info={info} start={start} />;
    // A document (docx / xlsx / pptx / ODF / RTF) reads exactly like a text file.
    case "text": case "document": return <TextView info={info} query={query} />;
    default: return <OtherView info={info} />;
  }
}

// ---- images: fit, wheel zoom, drag pan, double-click 1:1 ------------------------------------------------------

function ImageView({ info, boxes }: { info: PreviewInfo; boxes: HitBox[] | null }) {
  const meta = info.image;
  const [useThumb, setUseThumb] = useState(!!meta?.thumb);
  const [dead, setDead] = useState(false);
  const src = useThumb ? info.thumb : info.src;
  const drawn = useMemo(() => boxesForView(boxes, null, null), [boxes]);
  if (!src || dead) return <OtherView info={info} note="This image cannot be shown here." />;
  return (
    <ImageStage
      key={src} src={src} name={info.name} boxes={drawn}
      dims={!useThumb && meta?.width && meta.height ? { w: meta.width, h: meta.height } : null}
      note={useThumb ? "Shell thumbnail" : null}
      onError={() => { if (!useThumb && info.thumb) setUseThumb(true); else setDead(true); }}
    />
  );
}

function ImageStage({ src, name, dims, note, boxes, onError }: {
  src: string; name: string; dims: { w: number; h: number } | null; note: string | null; boxes: HitBox[]; onError: () => void;
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
      <div className="pv-zoom" aria-hidden>{Math.round(cur.s * 100)}%{note ? ` · ${note}` : ""}</div>
    </div>
  );
}

// ---- PDF: the page rendered by pdfium, prev / next ---------------------------------------------------------

function PdfView({ backend, info, query, page, boxes, onPage }: {
  backend: Backend; info: PreviewInfo; query: string; page: number | null; boxes: HitBox[] | null; onPage: (n: number) => void;
}) {
  const pages = info.pdf?.pages ?? null;
  const [n, setN] = useState(() => clampPage(page ?? 1, pages));
  useEffect(() => { onPage(n); }, [n, onPage]);
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
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={n <= 1} onClick={() => go(-1)} aria-label="Previous page" title="Previous page">‹</button>
        <span className="pv-pageno" aria-live="polite">Page {n} of {pages}</span>
        <button className="btn quiet pv-btn" tabIndex={-1} onMouseDown={keep} disabled={n >= pages} onClick={() => go(1)} aria-label="Next page" title="Next page">›</button>
      </div>
      <div className={"pv-pdfpage" + (loading ? " stale" : "")} ref={box}>
        {err ? (
          <div className="pv-msg" role="alert">{err}</div>
        ) : url ? (
          <ImageStage key={url} src={url} name={`${info.name}, page ${n}`} dims={null} note={null} boxes={drawn} onError={() => setErr("This page cannot be shown.")} />
        ) : (
          <div className="pv-msg"><span className="spin" aria-label="Loading" /></div>
        )}
      </div>
    </div>
  );
}

// ---- on-demand document parser: extract text, copy a table, copy as Markdown ----------------------------------

function ParseBar({ backend, path, page, available }: { backend: Backend; path: string; page: number | null; available: boolean }) {
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
      <div className="pv-parse">
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
    <div className="pv-parse">
      <div className="pv-parse-bar">
        {btn("Extract text", extract, "Read the text on this page with the document parser")}
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
    onLoadedMetadata: (e: React.SyntheticEvent<HTMLMediaElement>) => {
      if (start && start > 0) e.currentTarget.currentTime = Math.min(start, e.currentTarget.duration || start);
    },
  };
  return info.kind === "video" ? (
    <div className="pv-media"><video ref={ref as React.RefObject<HTMLVideoElement>} {...common} /></div>
  ) : (
    <div className="pv-media audio">
      <div className="pv-bigicon"><FileIcon name={info.name} isDir={false} src={null} /></div>
      <audio ref={ref as React.RefObject<HTMLAudioElement>} {...common} />
    </div>
  );
}

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
  const rows: [string, string][] = [];
  const type = info.kind === "folder" ? "Folder" : info.ext ? `${info.ext.toUpperCase()} file` : "File";
  rows.push(["Type", type]);
  if (info.size !== null) rows.push(["Size", formatSize(info.size)]);
  if (info.mtime !== null) rows.push(["Modified", formatDate(info.mtime)]);
  if (info.ctime !== null) rows.push(["Created", formatDate(info.ctime)]);
  return (
    <div className="pv-other">
      <div className="pv-bigicon">
        {info.thumb && !broken ? (
          <img src={info.thumb} alt="" draggable={false} onError={() => setBroken(true)} />
        ) : (
          <FileIcon name={info.name} isDir={info.kind === "folder"} src={null} />
        )}
      </div>
      <div className="pv-oname" title={info.name}>{info.name}</div>
      {note && <div className="pv-note">{note}</div>}
      <dl className="pv-dl">
        {rows.map(([k, v]) => (<div key={k}><dt>{k}</dt><dd>{v}</dd></div>))}
      </dl>
    </div>
  );
}
