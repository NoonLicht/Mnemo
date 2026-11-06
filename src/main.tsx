import React, { Suspense } from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./i18n";

async function boot() {
  // Outside the desktop shell, load example data so every screen can be previewed.
  if (!("__TAURI_INTERNALS__" in window)) await import("./dev/mockTauri");
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <Suspense fallback={null}>
        <App />
      </Suspense>
    </React.StrictMode>,
  );
}
boot();
