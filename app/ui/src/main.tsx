import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import Library from "./Library";
import { ErrorBoundary } from "./ErrorBoundary";
import { applyAccent, loadAccent } from "./accent";
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
// The other window (popup / library) follows a change made in Settings: localStorage is shared,
// and the storage event fires only in the windows that did not make the change.
window.addEventListener("storage", (e) => { if (e.key === "winseek.accent" && e.newValue) applyAccent(loadAccent()); });

loadBackend().then((backend) => {
  createRoot(document.getElementById("root")!).render(
    <StrictMode><ErrorBoundary>{library ? <Library backend={backend} /> : <App backend={backend} />}</ErrorBoundary></StrictMode>,
  );
});
