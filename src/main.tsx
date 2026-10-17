import React, { lazy, Suspense } from "react";
import ReactDOM from "react-dom/client";

// One entry, no router (PLAN 1.1): the overlay window loads /overlay, everything else is the main window.
// The main window is lazy so the pill bundle stays small (DESIGN 11).
// /marks is the voice-control marks window (full screen, click-through).
const path = window.location.pathname;
const Root = path.startsWith("/overlay")
  ? lazy(() => import("./overlay/Overlay"))
  : path.startsWith("/marks")
    ? lazy(() => import("./overlay/Marks"))
    : lazy(() => import("./App"));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Suspense fallback={null}>
      <Root />
    </Suspense>
  </React.StrictMode>,
);
