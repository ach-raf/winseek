import { useCallback, useEffect, useRef, useState } from "react";
import { formatClock, seekTarget } from "./previewLogic";

/**
 * Video / audio player for the preview pane. The browser's native controls are unusable here (the pane
 * swallows pointer focus, the volume popup is clipped, audio gets a 36 px strip), so this draws its own:
 * play / skip / seek with a buffered track, volume with mute, speed, fullscreen, keyboard shortcuts and a
 * "Match at 1:23" jump for a search hit's moment. Volume and mute persist across files.
 */

const VOL_KEY = "winseek.media.volume";
const RATES = [0.75, 1, 1.25, 1.5, 2];
const WAVE = Array.from({ length: 64 }, (_, j) => 18 + Math.abs(Math.sin(j * 0.7) * 46 + Math.sin(j * 2.3) * 26));

function loadVolume(): { v: number; m: boolean } {
  try {
    const j = JSON.parse(localStorage.getItem(VOL_KEY) ?? "null");
    if (j && typeof j.v === "number") return { v: Math.min(1, Math.max(0, j.v)), m: !!j.m };
  } catch { /* storage unavailable */ }
  return { v: 1, m: false };
}

const I = {
  play: <path d="M8 5.5v13a1 1 0 0 0 1.5.86l11-6.5a1 1 0 0 0 0-1.72l-11-6.5A1 1 0 0 0 8 5.5Z" />,
  pause: <path d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z" />,
  back: <path d="M12 5V2L7 6.5 12 11V8a5 5 0 1 1-5 5H5a7 7 0 1 0 7-8Z" />,
  fwd: <path d="M12 5V2l5 4.5L12 11V8a5 5 0 1 0 5 5h2a7 7 0 1 1-7-8Z" />,
  vol: <path d="M4 9.5v5h3.5L12 19V5L7.5 9.5H4Zm11.5-1.2a5 5 0 0 1 0 7.4l-1.2-1.4a3 3 0 0 0 0-4.6l1.2-1.4Zm2.4-2.4a8.5 8.5 0 0 1 0 12.2l-1.3-1.5a6.5 6.5 0 0 0 0-9.2l1.3-1.5Z" />,
  low: <path d="M4 9.5v5h3.5L12 19V5L7.5 9.5H4Zm11.5-1.2a5 5 0 0 1 0 7.4l-1.2-1.4a3 3 0 0 0 0-4.6l1.2-1.4Z" />,
  mute: <path d="M4 9.5v5h3.5L12 19V5L7.5 9.5H4Zm10.4 1.1L16 12l-1.6 1.4 1 1.1 1.6-1.5 1.6 1.5 1-1.1L18 12l1.6-1.4-1-1.1L17 11l-1.6-1.5-1 1.1Z" />,
  full: <path d="M5 5h5v2H7v3H5V5Zm9 0h5v5h-2V7h-3V5ZM5 14h2v3h3v2H5v-5Zm12 0h2v5h-5v-2h3v-3Z" />,
  exit: <path d="M8 5h2v5H5V8h3V5Zm6 0h2v3h3v2h-5V5ZM5 14h5v5H8v-3H5v-2Zm9 0h5v2h-3v3h-2v-5Z" />,
  note: <path d="M9 4.5v10.2A3.5 3.5 0 1 0 11 18V8.4l7-1.6v6.9a3.5 3.5 0 1 0 2 3.2V3L9 4.5Z" />,
};
function Icon({ d }: { d: keyof typeof I }) {
  return <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden>{I[d]}</svg>;
}

/** A horizontal slider driven by pointer capture (works through the pane's focus handling, unlike <input type=range>). */
function Slider({ value, max, buffered, step, label, text, onChange, onCommit, className }: {
  value: number; max: number; buffered?: number; step: number; label: string; text?: string;
  onChange: (v: number) => void; onCommit?: (v: number) => void; className?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const drag = useRef(false);
  const at = (e: React.PointerEvent) => {
    const r = box.current!.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / Math.max(1, r.width))) * max;
  };
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  const bpct = max > 0 && buffered ? Math.min(100, (buffered / max) * 100) : 0;
  return (
    <div
      ref={box} role="slider" tabIndex={0} className={`mp-slider ${className ?? ""}`}
      aria-label={label} aria-valuemin={0} aria-valuemax={Math.round(max)} aria-valuenow={Math.round(value)} aria-valuetext={text}
      onPointerDown={(e) => { e.preventDefault(); e.stopPropagation(); e.currentTarget.focus(); e.currentTarget.setPointerCapture(e.pointerId); drag.current = true; onChange(at(e)); }}
      onPointerMove={(e) => { if (drag.current) onChange(at(e)); }}
      onPointerUp={(e) => { if (!drag.current) return; drag.current = false; onCommit?.(at(e)); }}
      onPointerCancel={() => { drag.current = false; }}
      onKeyDown={(e) => {
        const d = e.key === "ArrowRight" || e.key === "ArrowUp" ? step : e.key === "ArrowLeft" || e.key === "ArrowDown" ? -step : e.key === "Home" ? -max : e.key === "End" ? max : 0;
        if (!d) return;
        e.preventDefault(); e.stopPropagation();
        const v = Math.min(max, Math.max(0, value + d));
        onChange(v); onCommit?.(v);
      }}
    >
      <div className="mp-track">
        {bpct > 0 && <div className="mp-buf" style={{ width: `${bpct}%` }} />}
        <div className="mp-fill" style={{ width: `${pct}%` }} />
        <div className="mp-thumb" style={{ left: `${pct}%` }} />
      </div>
    </div>
  );
}

export function MediaPlayer({ kind, src, name, start, onError }: {
  kind: "video" | "audio"; src: string; name: string; start?: number; onError: () => void;
}) {
  const ref = useRef<HTMLMediaElement>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [dur, setDur] = useState(0);
  const [buf, setBuf] = useState(0);
  const [scrub, setScrub] = useState<number | null>(null);
  const [{ v: vol, m: muted }, setVm] = useState(loadVolume);
  const [rate, setRate] = useState(1);
  const [full, setFull] = useState(false);
  const [idle, setIdle] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [ar, setAr] = useState(16 / 9);
  const idleTimer = useRef<number>(0);
  const isVideo = kind === "video";

  // Source lifecycle: unmounting (selection moved on) must stop the download as well as the sound.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (el.getAttribute("src") !== src) el.src = src; // re-arm after StrictMode's simulated unmount
    setTime(0); setDur(0); setBuf(0); setPlaying(false); setScrub(null);
    return () => { el.pause(); el.removeAttribute("src"); el.load(); };
  }, [src]);

  // Volume / mute / speed live on the element; state is the source of truth.
  useEffect(() => {
    const el = ref.current;
    if (el) { el.volume = vol; el.muted = muted || vol === 0; }
    try { localStorage.setItem(VOL_KEY, JSON.stringify({ v: vol, m: muted })); } catch { /* ignore */ }
  }, [vol, muted, src]);
  useEffect(() => { if (ref.current) ref.current.playbackRate = rate; }, [rate, src]);

  useEffect(() => {
    const f = () => setFull(document.fullscreenElement === wrap.current);
    document.addEventListener("fullscreenchange", f);
    return () => document.removeEventListener("fullscreenchange", f);
  }, []);

  const wake = useCallback(() => {
    setIdle(false);
    window.clearTimeout(idleTimer.current);
    if (isVideo) idleTimer.current = window.setTimeout(() => setIdle(true), 2600);
  }, [isVideo]);
  useEffect(() => () => window.clearTimeout(idleTimer.current), []);
  useEffect(() => { if (!playing) { window.clearTimeout(idleTimer.current); setIdle(false); } else wake(); }, [playing, wake]);

  const toggle = () => {
    const el = ref.current;
    if (!el) return;
    if (el.paused) void el.play().catch(() => {}); else el.pause();
  };
  const seek = (t: number) => {
    const el = ref.current;
    if (!el) return;
    const d = Number.isFinite(el.duration) ? el.duration : t;
    el.currentTime = Math.min(Math.max(0, t), d);
    setTime(el.currentTime);
  };
  const skip = (d: number) => seek((ref.current?.currentTime ?? 0) + d);
  const setVol = (v: number) => setVm({ v: Math.min(1, Math.max(0, v)), m: false });
  const toggleMute = () => setVm((s) => (s.v === 0 ? { v: 0.6, m: false } : { ...s, m: !s.m }));
  const toggleFull = () => {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void wrap.current?.requestFullscreen?.().catch(() => {});
  };
  const cycleRate = () => setRate((r) => RATES[(RATES.indexOf(r) + 1) % RATES.length] ?? 1);

  const jump = () => {
    const el = ref.current;
    const t = seekTarget(start, el?.duration ?? NaN);
    if (!el || t === null) return;
    seek(t);
    void el.play().catch(() => {});
  };
  const anchor = seekTarget(start, Infinity);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key.toLowerCase();
    let handled = true;
    if (k === " " || k === "k") toggle();
    else if (k === "arrowleft" || k === "j") skip(e.shiftKey ? -30 : -10);
    else if (k === "arrowright" || k === "l") skip(e.shiftKey ? 30 : 10);
    else if (k === "arrowup") setVol(vol + 0.1);
    else if (k === "arrowdown") setVol(vol - 0.1);
    else if (k === "m") toggleMute();
    else if (k === "f" && isVideo) toggleFull();
    else if (k === "home") seek(0);
    else if (k === "end") seek(dur);
    else if (k >= "0" && k <= "9" && k.length === 1) seek((dur * Number(k)) / 10);
    else handled = false;
    if (handled) { e.preventDefault(); e.stopPropagation(); }
    wake();
  };

  const shown = scrub ?? time;
  const level = muted || vol === 0 ? "mute" : vol < 0.5 ? "low" : "vol";
  const Btn = ({ d, title, onClick, big, cls }: { d: keyof typeof I; title: string; onClick: () => void; big?: boolean; cls?: string }) => (
    <button type="button" className={`mp-btn${big ? " big" : ""}${cls ? " " + cls : ""}`} title={title} aria-label={title} tabIndex={-1}
      onClick={(e) => { e.stopPropagation(); onClick(); wrap.current?.focus(); }} onDoubleClick={(e) => e.stopPropagation()}>
      <Icon d={d} />
    </button>
  );

  const bar = (
    <div className="mp-bar" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
      <div className="mp-seekrow">
        <span className="mp-time">{formatClock(shown)}</span>
        <Slider
          className="mp-seek" value={shown} max={dur || 0} buffered={buf} step={5} label="Seek" text={`${formatClock(shown)} of ${formatClock(dur)}`}
          onChange={(v) => setScrub(v)} onCommit={(v) => { seek(v); setScrub(null); }}
        />
        <span className="mp-time">{formatClock(dur)}</span>
      </div>
      <div className="mp-row">
        <Btn d="back" cls="mp-skip" title="Back 10 s (←)" onClick={() => skip(-10)} />
        <Btn d={playing ? "pause" : "play"} title={playing ? "Pause (Space)" : "Play (Space)"} onClick={toggle} big />
        <Btn d="fwd" cls="mp-skip" title="Forward 10 s (→)" onClick={() => skip(10)} />
        <span className="mp-gap" />
        <button type="button" className="mp-rate" tabIndex={-1} title="Playback speed" onClick={(e) => { e.stopPropagation(); cycleRate(); wrap.current?.focus(); }}>{rate}×</button>
        <div className="mp-vol">
          <Btn d={level} title={muted ? "Unmute (M)" : "Mute (M)"} onClick={toggleMute} />
          <Slider className="mp-volsl" value={muted ? 0 : vol} max={1} step={0.1} label="Volume" text={`${Math.round((muted ? 0 : vol) * 100)}%`} onChange={setVol} />
        </div>
        {isVideo && <Btn d={full ? "exit" : "full"} title="Fullscreen (F)" onClick={toggleFull} />}
      </div>
    </div>
  );

  const at = anchor !== null && (
    <button className="btn pv-at ov" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={(e) => { e.stopPropagation(); jump(); }} title="Jump to the moment that matched">
      Match at {formatClock(anchor)}
    </button>
  );

  const common = {
    src, preload: "metadata" as const, onError,
    onPlay: () => setPlaying(true), onPause: () => setPlaying(false), onEnded: () => setPlaying(false),
    onWaiting: () => setWaiting(true), onPlaying: () => setWaiting(false), onCanPlay: () => setWaiting(false),
    onTimeUpdate: (e: React.SyntheticEvent<HTMLMediaElement>) => { if (scrub === null) setTime(e.currentTarget.currentTime); },
    onDurationChange: (e: React.SyntheticEvent<HTMLMediaElement>) => setDur(Number.isFinite(e.currentTarget.duration) ? e.currentTarget.duration : 0),
    onProgress: (e: React.SyntheticEvent<HTMLMediaElement>) => {
      const b = e.currentTarget.buffered;
      setBuf(b.length ? b.end(b.length - 1) : 0);
    },
    // The hit's moment: seek there as soon as the clip knows its length (the matching window or shot starts here).
    onLoadedMetadata: (e: React.SyntheticEvent<HTMLMediaElement>) => {
      const el = e.currentTarget;
      el.volume = vol; el.muted = muted || vol === 0; el.playbackRate = rate;
      if (el instanceof HTMLVideoElement && el.videoWidth > 0 && el.videoHeight > 0) setAr(el.videoWidth / el.videoHeight);
      const t = seekTarget(start, el.duration);
      if (t !== null) el.currentTime = t;
    },
  };

  if (isVideo) {
    return (
      <div
        ref={wrap} tabIndex={0} className={`pv-media mp video${idle ? " idle" : ""}${playing ? " playing" : ""}${full ? " full" : ""}`}
        onKeyDown={onKey} onPointerMove={wake} onPointerDown={() => wrap.current?.focus()}
        onClick={toggle} onDoubleClick={toggleFull}
      >
        {/* The frame is the picture's own box (any aspect: portrait, square, ultrawide): the controls live inside it. */}
        <div className="mp-frame" style={{ "--ar": ar } as React.CSSProperties}>
          <video ref={ref as React.RefObject<HTMLVideoElement>} playsInline {...common} />
          {waiting && playing && <span className="mp-spin" aria-hidden />}
          {!playing && !waiting && (
            <span className="playb" aria-hidden><Icon d="play" /></span>
          )}
          {at}
          {bar}
        </div>
      </div>
    );
  }

  return (
    <div ref={wrap} tabIndex={0} className="pv-media mp audio" onKeyDown={onKey} onPointerDown={() => wrap.current?.focus()}>
      <audio ref={ref as React.RefObject<HTMLAudioElement>} {...common} />
      <div className={`mp-art${playing ? " on" : ""}`} aria-hidden><Icon d="note" /></div>
      <div className="mp-name" title={name}>{name}</div>
      <Wave progress={dur > 0 ? shown / dur : 0} playing={playing} onSeek={(f) => dur > 0 && seek(f * dur)} />
      {at}
      {bar}
    </div>
  );
}

/** Decorative waveform that doubles as a seek bar: the bars already played take the accent colour. */
function Wave({ progress, playing, onSeek }: { progress: number; playing: boolean; onSeek: (frac: number) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const at = (e: React.PointerEvent) => {
    const r = box.current!.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / Math.max(1, r.width)));
  };
  const drag = useRef(false);
  const cut = progress * WAVE.length;
  return (
    <div
      ref={box} className={`wave${playing ? " live" : ""}`} aria-hidden
      onPointerDown={(e) => { e.preventDefault(); e.currentTarget.setPointerCapture(e.pointerId); drag.current = true; onSeek(at(e)); }}
      onPointerMove={(e) => { if (drag.current) onSeek(at(e)); }}
      onPointerUp={() => { drag.current = false; }}
    >
      {WAVE.map((h, j) => <i key={j} className={j < cut ? "done" : ""} style={{ height: `${h}%`, animationDelay: `${(j % 9) * 70}ms` }} />)}
    </div>
  );
}
