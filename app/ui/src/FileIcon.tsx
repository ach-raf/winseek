import { kindOf } from "./format";

const PATHS: Record<string, string> = {
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z",
  image: "M5 5h14a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM5 17l4.5-5 3 3 2-2L19 17M8 9h.01",
  doc: "M7 3h7l4 4v13a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm6 0v5h5M9 13h6M9 16.5h6",
  video: "M4 6h11a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1zm13 4 4-2.5v9L17 14",
  audio: "M9 17V6l10-2v11M9 17a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0zm10-2a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z",
  archive: "M5 4h14v4H5zM6 8h12v12H6zm4 3h4",
  code: "M9 8l-5 4 5 4M15 8l5 4-5 4M13.5 6l-3 12",
  app: "M4 5h16v14H4zM4 9h16M7 7h.01M10 7h.01",
  file: "M7 3h7l4 4v13a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm6 0v5h5",
};

export function FileIcon({ name, isDir }: { name: string; isDir: boolean }) {
  const k = kindOf(name, isDir);
  return (
    <span className={`ficon k-${k}`} aria-hidden>
      <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <path d={PATHS[k]} />
      </svg>
    </span>
  );
}
