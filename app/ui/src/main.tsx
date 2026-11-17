import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import Library from "./Library";
import { ErrorBoundary } from "./ErrorBoundary";
import { createQueryClient } from "./qk";
import { applyAccent, loadAccent } from "./accent";
import { applyTheme, loadTheme, watchSystemTheme } from "./theme";
import { applyOpacity, loadOpacity } from "./transparency";
import { loadBackend, inTauri } from "./api";
import "@fontsource-variable/figtree";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./base.css";
import "./shell.css";
import "./search.css";
import "./preview.css";
import "./library.css";
import "./settings.css";

if (!inTauri) document.documentElement.classList.add("web");
// The library window loads this same bundle with ?view=library (or #library).
const library = new URLSearchParams(location.search).get("view") === "library" || location.hash === "#library";
if (library) document.documentElement.classList.add("lib-view");

applyAccent(loadAccent());
applyTheme(loadTheme());
applyOpacity(loadOpacity());
// The other window (popup / library) follows a change made in Settings: localStorage is shared,
// and the storage event fires only in the windows that did not make the change.
window.addEventListener("storage", (e) => {
  if (e.key === "winseek.accent" && e.newValue) applyAccent(loadAccent());
  if (e.key === "winseek.theme") applyTheme(loadTheme());
  if (e.key === "winseek.opacity") applyOpacity(loadOpacity());
});
// On System, a flip of the Windows theme re-resolves live (explicit Light/Dark ignores this).
watchSystemTheme(() => { if (loadTheme() === "system") applyTheme("system"); });

loadBackend().then((backend) => {
  // One client per window: the popup and the library window are separate JS contexts.
  const queryClient = createQueryClient();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <ErrorBoundary>
        <QueryClientProvider client={queryClient}>
          {library ? <Library backend={backend} /> : <App backend={backend} />}
        </QueryClientProvider>
      </ErrorBoundary>
    </StrictMode>,
  );
});
