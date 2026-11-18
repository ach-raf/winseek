import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Backend, Tier1Status, VolumeStatus } from "./types";
import { qk } from "./qk";
import { activeJobs, FINISHED_TEXT, jobsFrac, parserBusy, type JobKey } from "./jobsLogic";
import type { Models } from "./hooks";

const FLASH_MS = 5000;

/** The bottom jobs strip. While any job runs — drive scan, meaning index, document reading,
 *  component downloads — it is the last strip of the window on every screen, so the user always
 *  knows work is happening and what exactly. When a job drains, its "finished" line flashes for
 *  a moment before the strip goes away again. */
export function JobsBar({ backend, volumes, t1, models }: {
  backend: Backend;
  volumes: VolumeStatus[];
  t1: Tier1Status | null;
  models: Models;
}) {
  // Shares the Settings›Status cache entry (`qk.statusPage`): when that page is open, its 1 s
  // observer already refreshes the answer, so this one only polls fast while the parser — the
  // one running-job signal that lives only on the status page — still has pages left.
  const pageQ = useQuery({
    queryKey: qk.statusPage,
    queryFn: () => backend.statusPage(),
    refetchInterval: (q) => (parserBusy(q.state.data) ? 1000 : 5000),
  });

  const jobs = activeJobs(volumes, t1, pageQ.data ?? null, {
    running: models.running,
    progress: models.progress,
    failed: !!models.interrupted,
  });

  // A job that was running and is gone gets a short "finished" flash. Two exceptions: a download
  // that was cancelled or failed did not finish, and a status-page error blanks the parse /
  // ffmpeg answer — neither may say "finished". The effect keys on the key set, so polls that
  // only move a counter do not re-run it; the mutable bits ride in refs.
  const [flash, setFlash] = useState<{ text: string; n: number } | null>(null);
  const prev = useRef<{ keys: JobKey[]; modelsFailed: boolean }>({ keys: [], modelsFailed: false });
  const modelsInterruptedRef = useRef(false);
  modelsInterruptedRef.current = !!models.interrupted;
  const pageOkRef = useRef(false);
  pageOkRef.current = pageQ.data != null;
  const flashN = useRef(0);
  const jobsKey = jobs.map((j) => j.key).join("|");
  useEffect(() => {
    const keys = jobsKey ? jobsKey.split("|") as JobKey[] : [];
    const { keys: before, modelsFailed } = prev.current;
    prev.current = { keys, modelsFailed: modelsInterruptedRef.current };
    const gone = before.find((k) => !keys.includes(k) && !(!pageOkRef.current && (k === "parse" || k === "ffmpeg")));
    if (gone && !(gone === "models" && modelsFailed)) setFlash({ text: FINISHED_TEXT[gone], n: ++flashN.current });
  }, [jobsKey]);

  useEffect(() => {
    if (!flash) return;
    const t = window.setTimeout(() => setFlash(null), FLASH_MS);
    return () => window.clearTimeout(t);
  }, [flash]);

  const frac = jobsFrac(jobs);
  if (!jobs.length && !flash) return null;
  return (
    <div className="jbar" role="status" aria-live="polite">
      {jobs.length > 0 && <span className="jspin" aria-hidden />}
      {jobs.map((j) => (
        <span key={j.key} className="jseg">{j.label}</span>
      ))}
      {flash && <span key={flash.n} className="jdone">✓ {flash.text}</span>}
      {frac != null && <i className="jtrack" style={{ width: `${Math.min(100, frac * 100)}%` }} aria-hidden />}
    </div>
  );
}
