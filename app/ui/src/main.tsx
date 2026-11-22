import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import Library from "./Library";
import { ErrorBoundary } from "./ErrorBoundary";
import { createQueryClient, qk } from "./qk";
import { applyAccent, loadAccent } from "./accent";
import { loadTheme, watchSystemTheme } from "./theme";
import { applyThemeChoice } from "./themeChoice";
import { applyOpacity, loadOpacity } from "./transparency";
import { loadBackend, inTauri } from "./api";
import "@fontsource-variable/figtree";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./base.css";
import "./shell.css";
import "./search.css";
import "./picgrid.css";
import "./preview.css";
import "./library.css";
import "./settings.css";

if (!inTauri) document.documentElement.classList.add("web");
// The library window loads this same bundle with ?view=library (or #library).
const library = new URLSearchParams(location.search).get("view") === "library" || location.hash === "#library";
if (library) document.documentElement.classList.add("lib-view");

// The real width of a vertical scrollbar here (thin Windows bars are ~11px, classic 17px). Scrollers reserve
// that lane (scrollbar-gutter: stable) and take it off their right padding, so the visible gutters are equal.
{
  const probe = document.createElement("div");
  probe.className = "list";
  probe.style.cssText = "position:absolute;visibility:hidden;width:80px;height:40px;overflow-y:scroll";
  document.body.appendChild(probe);
  document.documentElement.style.setProperty("--sbw", `${probe.offsetWidth - probe.clientWidth}px`);
  probe.remove();
}

applyAccent(loadAccent());
applyThemeChoice(loadTheme());
applyOpacity(loadOpacity());
// The other window (popup / library) follows a change made in Settings: localStorage is shared,
// and the storage event fires only in the windows that did not make the change. A custom theme
// edited elsewhere ("winseek.customThemes") repaints this window too, when it is the one on.
window.addEventListener("storage", (e) => {
  if (e.key === "winseek.accent" && e.newValue) applyAccent(loadAccent());
  if (e.key === "winseek.theme") applyThemeChoice(loadTheme());
  if (e.key === "winseek.customThemes") applyThemeChoice(loadTheme());
  if (e.key === "winseek.opacity") applyOpacity(loadOpacity());
});
// On System, a flip of the Windows theme re-resolves live (explicit Light/Dark ignores this).
watchSystemTheme(() => { if (loadTheme() === "system") applyThemeChoice("system"); });

loadBackend().then((backend) => {
  // One client per window: the popup and the library window are separate JS contexts.
  const queryClient = createQueryClient();
  // Web-only verify hook: drop the shared statusPage answer so a jump into Status starts cold,
  // like a real session whose poll has been down or garbage-collected (window.__statusForget).
  if (!inTauri) (window as unknown as { __statusForget?: () => void }).__statusForget =
    () => queryClient.removeQueries({ queryKey: qk.statusPage });
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
