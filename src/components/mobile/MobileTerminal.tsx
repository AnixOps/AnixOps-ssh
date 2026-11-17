import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronLeft, RotateCw, Unplug } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { MobileKeyBar } from "@/components/mobile/MobileKeyBar";
import { Button } from "@/components/ui/button";
import { openSession, type Host, type SessionEvent, type SessionHandle } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { applyCtrl, keySequence, type BarKey } from "@/lib/mobileKeys";
import { settings } from "@/lib/settings";
import { terminalFont, terminalTheme } from "@/lib/terminalTheme";

type Status = { state: "connecting" } | { state: "connected" } | { state: "exited"; error?: string };

/** One terminal on one host, in-process (russh, no daemon), with the key bar.
 *  Unmounting ends the session: a phone has no background process to keep it
 *  (docs/architecture-v2.md section 2). The page above sizes this component to
 *  the visible viewport, so the keyboard never covers the bar. */
export function MobileTerminal({ host, onBack }: { host: Host; onBack: () => void }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<SessionHandle | null>(null);
  const startRef = useRef<() => void>(() => {});
  const ctrlRef = useRef(false);
  const [ctrl, setCtrlState] = useState(false);
  const [status, setStatus] = useState<Status>({ state: "connecting" });

  const setCtrl = useCallback((on: boolean) => {
    ctrlRef.current = on;
    setCtrlState(on);
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;

    const terminal = new Terminal({
      fontFamily: terminalFont(),
      fontSize: settings().terminalFontSize,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      theme: terminalTheme(),
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(container);
    // The keyboard must not capitalise, correct or complete shell input.
    const input = terminal.textarea;
    input?.setAttribute("autocapitalize", "off");
    input?.setAttribute("autocorrect", "off");
    input?.setAttribute("spellcheck", "false");
    terminalRef.current = terminal;
    fit.fit();

    const send = (data: string) => sessionRef.current?.write(data);
    terminal.onData((data) => {
      if (ctrlRef.current) {
        const control = applyCtrl(data);
        if (control !== null) {
          setCtrl(false);
          send(control);
          return;
        }
      }
      send(data);
    });
    terminal.onResize(({ cols, rows }) => sessionRef.current?.resize(cols, rows));

    const start = () => {
      setStatus({ state: "connecting" });
      let exited = false;
      const onEvent = (event: SessionEvent) => {
        if (disposed) return;
        if (event.kind === "data") {
          terminal.write(event.data);
          setStatus((current) => (current.state === "connecting" ? { state: "connected" } : current));
        } else if (event.kind === "exit") {
          exited = true;
          sessionRef.current = null;
          setStatus({ state: "exited" });
        }
      };
      openSession(host.id, terminal.cols, terminal.rows, onEvent)
        .then((session) => {
          if (disposed || exited) {
            if (!exited) session.close();
            return;
          }
          sessionRef.current = session;
          session.resize(terminal.cols, terminal.rows);
        })
        .catch((reason) => {
          if (!disposed) setStatus({ state: "exited", error: String(reason) });
        });
    };
    startRef.current = () => {
      terminal.write("\r\n");
      start();
      terminal.focus();
    };
    start();

    const observer = new ResizeObserver(() => {
      if (container.clientWidth > 0 && container.clientHeight > 0) fit.fit();
    });
    observer.observe(container);
    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    const updateTheme = () => (terminal.options.theme = terminalTheme());
    scheme.addEventListener("change", updateTheme);

    return () => {
      disposed = true;
      scheme.removeEventListener("change", updateTheme);
      observer.disconnect();
      sessionRef.current?.close();
      sessionRef.current = null;
      terminal.dispose();
      terminalRef.current = null;
    };
  }, [host.id, setCtrl]);

  const pressKey = (key: BarKey) => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    sessionRef.current?.write(keySequence(key, terminal.modes.applicationCursorKeysMode, ctrlRef.current));
    setCtrl(false);
  };

  const statusText =
    status.state === "connecting" ? t.connecting : status.state === "connected" ? t.connected : t.sessionEnded;

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col bg-bg">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="flex h-14 items-center gap-1 pr-safe pl-1">
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileBack} onClick={onBack} className="md:hidden">
            <ChevronLeft />
          </Button>
          <div className="min-w-0 flex-1 pl-2 md:pl-4">
            <h2 className="truncate text-body font-medium">{host.name}</h2>
            <p role="status" className="truncate text-callout text-label-2">
              {statusText} · {host.target}
            </p>
          </div>
        </div>
      </header>

      <div className="relative min-h-0 flex-1">
        <div ref={containerRef} className="absolute inset-x-3 top-2 bottom-1" onClick={() => terminalRef.current?.focus()} />
        {status.state === "exited" && (
          <div className="absolute inset-x-0 bottom-3 flex justify-center px-4">
            <div
              role="status"
              className="flex w-full max-w-xl flex-col gap-3 rounded-md bg-bg-elevated px-4 py-3 shadow-3"
            >
              <div className="flex items-start gap-3">
                <Unplug className="mt-0.5 size-5 shrink-0 text-label-2" strokeWidth={1.5} />
                <div className="min-w-0 flex-1">
                  <p className="text-callout font-medium text-label-1">
                    {status.error ? t.sessionFailed : t.sessionEnded}
                  </p>
                  {status.error && (
                    <p className="selectable line-clamp-4 break-words text-callout text-label-2">{status.error}</p>
                  )}
                </div>
              </div>
              <Button variant="primary" size="lg" onClick={() => startRef.current()}>
                <RotateCw />
                {t.reconnect}
              </Button>
            </div>
          </div>
        )}
      </div>

      <MobileKeyBar ctrl={ctrl} onToggleCtrl={() => setCtrl(!ctrlRef.current)} onKey={pressKey} />
    </div>
  );
}
