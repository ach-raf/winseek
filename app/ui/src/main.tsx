import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import Library from "./Library";
import { loadBackend, inTauri } from "./api";
import "./styles.css";

if (!inTauri) document.documentElement.classList.add("web");
// The library window loads this same bundle with ?view=library (or #library).
const library = new URLSearchParams(location.search).get("view") === "library" || location.hash === "#library";
if (library) document.documentElement.classList.add("lib-view");

loadBackend().then((backend) => {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>{library ? <Library backend={backend} /> : <App backend={backend} />}</StrictMode>,
  );
});
