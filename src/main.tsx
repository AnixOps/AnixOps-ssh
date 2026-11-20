import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { MobileApp } from "./components/mobile/MobileApp";
import { EditContextMenu } from "./components/ui/editMenu";
import { reportFrontendError } from "./lib/backend";
import { isMobileShell } from "./lib/platform";
import { installErrorReporting } from "./lib/telemetry";
import { installWebviewGuards } from "./lib/webviewChrome";

installErrorReporting(window, reportFrontendError);
const mobile = isMobileShell();
// A desktop window is an app, not a browser page (lib/webviewChrome.ts).
if (!mobile) installWebviewGuards();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {mobile ? (
      <MobileApp />
    ) : (
      <>
        <App />
        <EditContextMenu />
      </>
    )}
  </StrictMode>,
);
