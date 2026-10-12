import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { loadBackend, inTauri } from "./api";
import "./styles.css";

if (!inTauri) document.documentElement.classList.add("web");

loadBackend().then((backend) => {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App backend={backend} />
    </StrictMode>,
  );
});
