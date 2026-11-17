import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { MobileApp } from "./components/mobile/MobileApp";
import { reportFrontendError } from "./lib/backend";
import { isMobileShell } from "./lib/platform";
import { installErrorReporting } from "./lib/telemetry";

installErrorReporting(window, reportFrontendError);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {isMobileShell() ? <MobileApp /> : <App />}
  </StrictMode>,
);
