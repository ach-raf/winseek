import type { Backend, Collection, DupGroup, Hit, IgnoreList, LibImage, NearState, PreviewInfo, ModelsDone, ModelsPlan, ModelsProgress, SemanticResult, ServiceState, StatusPage, Tier1Status } from "./types";

interface TauriGlobal {
  core: { invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T = unknown>(ev: string, cb: (e: { payload: T }) => void): Promise<() => void> };
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
    iconUrl: (path, isDir, thumb, size) =>
      `http://wsicon.localhost/?p=${encodeURIComponent(path)}&d=${isDir ? 1 : 0}&s=${size ?? (thumb ? 40 : 32)}&t=${thumb ? 1 : 0}`,
    hide: () => t.window.getCurrentWindow().hide(),
    onShown: (cb) => t.event.listen("popup-shown", cb),
    serviceState: () => inv<ServiceState>("service_state"),
    installService: () => inv("install_service"),
    startService: () => inv("start_service"),
    accentPalette: () => inv("accent_palette"),
    tier1Status: () => inv<Tier1Status>("tier1_status"),
    tier1SetScopes: (include, exclude) => inv("tier1_set_scopes", { include, exclude }),
    tier1Pause: (paused) => inv("tier1_pause", { paused }),
    tier1GetIgnore: () => inv<IgnoreList>("tier1_get_ignore"),
    tier1SetIgnore: (list) => inv<IgnoreList>("tier1_set_ignore", { list }),
    previewInfo: async (path) => {
      const i = await inv<PreviewInfo>("preview_info", { path });
      const big = (isDir: boolean) => `http://wsicon.localhost/?p=${encodeURIComponent(path)}&d=${isDir ? 1 : 0}&s=512&t=1`;
      return {
        ...i,
        src: i.kind === "image" || i.kind === "video" || i.kind === "audio" ? `http://wsfile.localhost/?p=${encodeURIComponent(path)}` : null,
        thumb: i.kind === "text" ? null : big(i.kind === "folder"),
      };
    },
    semanticSearch: (query, k) => inv<SemanticResult>("semantic_search", { query, k }),
    statusPage: () => inv<StatusPage>("status_page"),
    rescanVolume: (letter) => inv("rescan_volume", { letter }),
    tier1Rebuild: () => inv("tier1_rebuild"),
    modelsPlan: () => inv<ModelsPlan>("models_plan"),
    modelsDownload: () => inv("models_download"),
    modelsCancel: () => inv("models_cancel"),
    onModelsProgress: (cb) => t.event.listen<ModelsProgress>("models-progress", (e) => cb(e.payload)),
    onModelsDone: (cb) => t.event.listen<ModelsDone>("models-done", (e) => cb(e.payload)),
    onSourceChanged: (cb) => t.event.listen("source-changed", cb),
    onOpenStatus: (cb) => t.event.listen("open-status", cb),
    openLibrary: () => inv("open_library"),
    libraryImages: () => inv<LibImage[]>("library_images"),
    libraryCollections: () => inv<Collection[]>("library_collections"),
    librarySetCollections: (list) => inv<Collection[]>("library_set_collections", { list }),
    libraryCollection: (query, threshold, cap) => inv<SemanticResult>("library_collection", { query, threshold, cap }),
    libraryDupGroups: () => inv<DupGroup[]>("library_dup_groups"),
    libraryNearStart: (threshold) => inv("library_near_start", { threshold }),
    libraryNearPoll: () => inv<NearState>("library_near_poll"),
    libraryBigFiles: (limit) => inv<Hit[]>("library_big_files", { limit }),
    libraryDims: (paths) => inv<([number, number] | null)[]>("library_dims", { paths }),
  };
}

/** Real Tauri bridge inside the app; mock data source in a plain browser. */
export async function loadBackend(): Promise<Backend> {
  if (window.__TAURI__) return tauriBackend(window.__TAURI__);
  return (await import("./mock")).mockBackend;
}
