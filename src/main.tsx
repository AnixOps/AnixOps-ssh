import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./design/fonts/inter.css";
import "./index.css";
import { App } from "./App";
import { reportFrontendError } from "./lib/backend";
import { installErrorReporting } from "./lib/telemetry";

installErrorReporting(window, reportFrontendError);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
