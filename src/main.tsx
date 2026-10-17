import React, { lazy, Suspense } from "react";
import ReactDOM from "react-dom/client";

// One entry, no router (PLAN 1.1): the overlay window loads /overlay, everything else is the main window.
// The main window is lazy so the pill bundle stays small (DESIGN 11).
const isOverlay = window.location.pathname.startsWith("/overlay");
const Root = isOverlay ? lazy(() => import("./overlay/Overlay")) : lazy(() => import("./App"));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Suspense fallback={null}>
      <Root />
    </Suspense>
  </React.StrictMode>,
);
