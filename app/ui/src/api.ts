import type { Backend } from "./types";

interface TauriGlobal {
  core: { invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen(ev: string, cb: () => void): Promise<() => void> };
  window: { getCurrentWindow(): { hide(): Promise<void> } };
}
declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
}

export const inTauri = typeof window !== "undefined" && !!window.__TAURI__;

function tauriBackend(t: TauriGlobal): Backend {
  const inv = t.core.invoke;
  return {
    search: (a) => inv("search", a),
    status: () => inv("status"),
    recent: (limit) => inv("recent", { limit }),
    openPath: (path) => inv("open_path", { path }),
    revealPath: (path) => inv("reveal_path", { path }),
    copyPath: (path) => inv("copy_path", { path }),
    copyFiles: (paths) => inv("copy_files", { paths }),
    recycle: (paths) => inv("recycle", { paths }),
    restoreRecycled: (paths) => inv("restore_recycled", { paths }),
    renamePath: (path, newName) => inv("rename_path", { path, newName }),
    showContextMenu: (path, x, y) => inv("show_context_menu", { path, x, y }),
    startDrag: (paths) => inv("start_drag", { paths }),
    iconUrl: (path, isDir, thumb) =>
      `http://wsicon.localhost/?p=${encodeURIComponent(path)}&d=${isDir ? 1 : 0}&s=${thumb ? 40 : 32}&t=${thumb ? 1 : 0}`,
    hide: () => t.window.getCurrentWindow().hide(),
    onShown: (cb) => t.event.listen("popup-shown", cb),
  };
}

/** Real Tauri bridge inside the app; mock data source in a plain browser. */
export async function loadBackend(): Promise<Backend> {
  if (window.__TAURI__) return tauriBackend(window.__TAURI__);
  return (await import("./mock")).mockBackend;
}
