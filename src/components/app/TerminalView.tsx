import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronDown, ChevronUp, RotateCw, SquareArrowOutUpRight, Unplug, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { CompletionPopup, type PopupPlacement } from "@/components/app/CompletionPopup";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import {
  attachSession,
  copyText,
  loadHosts,
  openFiles,
  openExternal,
  openSession,
  pasteText,
  type FilesHandle,
  type SessionEvent,
  type SessionHandle,
} from "@/lib/backend";
import { CompletionController, type ViewState } from "@/lib/completion/controller";
import { completionPrefs, commandHistory, onCompletionPrefs } from "@/lib/completion/prefs";
import { createPathLister } from "@/lib/completion/remotePaths";
import { commandNames, loadSpec } from "@/lib/completion/specs";
import { t } from "@/lib/i18n";
import { duration, ease, enter, leave } from "@/lib/motion";
import { onSettings, settings } from "@/lib/settings";
import { appShortcut } from "@/lib/shortcuts";
import { registerTerminal } from "@/lib/terminals";
import { terminalFont, terminalTheme } from "@/lib/terminalTheme";

export type SessionStatus =
  | { state: "connecting" }
  | { state: "connected" }
  | { state: "exited"; code: number | null; error?: string };

export function TerminalView({
  tabKey,
  hostId,
  sessionId,
  active,
  onStatus,
}: {
  tabKey: string;
  hostId: string | null;
  /** A live session to reattach to instead of opening a new one. */
  sessionId?: string;
  active: boolean;
  onStatus: (key: string, status: SessionStatus) => void;
}) {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const controllerRef = useRef<CompletionController | null>(null);
  const [completion, setCompletion] = useState<{ view: ViewState; placement: PopupPlacement } | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const sessionRef = useRef<SessionHandle | null>(null);
  const startRef = useRef<() => void>(() => {});
  const [status, setStatus] = useState<SessionStatus>({ state: "connecting" });
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const findInputRef = useRef<HTMLInputElement>(null);

  const report = useCallback(
    (next: SessionStatus) => {
      setStatus(next);
      onStatus(tabKey, next);
    },
    [onStatus, tabKey],
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;

    const terminal = new Terminal({
      fontFamily: terminalFont(),
      fontSize: settings().terminalFontSize,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: settings().terminalScrollback,
      allowProposedApi: true,
      macOptionIsMeta: true,
      theme: terminalTheme(),
    });
    const fit = new FitAddon();
    const search = new SearchAddon();
    terminal.loadAddon(fit);
    terminal.loadAddon(search);
    terminal.open(container);
    // Headless test browsers have no usable WebGL, so automation uses the DOM renderer.
    if (!navigator.webdriver) {
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        terminal.loadAddon(webgl);
      } catch {
        // The DOM renderer is the fallback when WebGL is unavailable.
      }
    }
    fit.fit();

    // Command completion: follows what is typed and shows a list at the cursor.
    let files: FilesHandle | null = null;
    let hostNames: Promise<string[]> | null = null;
    const controller = new CompletionController({
      hostId,
      history: commandHistory,
      deps: {
        loadSpec,
        commandNames,
        history: commandHistory,
        paths: hostId
          ? createPathLister(async () => {
              const opened = await openFiles(hostId);
              if (disposed) opened.close();
              files = opened;
              return opened;
            })
          : undefined,
        hostNames: () =>
          (hostNames ??= loadHosts()
            .then((snapshot) =>
              [...new Set(snapshot.hosts.flatMap((host) => [host.fields.alias ?? host.fields.hostname, host.name]))].filter(Boolean),
            )
            .catch(() => [])),
      },
      cursor: () => {
        const active = terminal.buffer.active;
        return { x: active.cursorX, y: active.cursorY, absoluteY: active.baseY + active.cursorY, cols: terminal.cols };
      },
      write: (data) => sessionRef.current?.write(data),
      enabled: () => completionPrefs().enabled,
    });
    controllerRef.current = controller;
    const stopView = controller.subscribe((view) => {
      const wrapper = wrapperRef.current;
      const screen = container.querySelector<HTMLElement>(".xterm-screen");
      if (!view.popup || !wrapper || !screen) {
        setCompletion(null);
        return;
      }
      const box = wrapper.getBoundingClientRect();
      const grid = screen.getBoundingClientRect();
      const cellW = grid.width / terminal.cols;
      const cellH = grid.height / terminal.rows;
      const lineTop = grid.top - box.top + view.cursor.y * cellH;
      const left = Math.max(8, Math.min(grid.left - box.left + view.cursor.x * cellW, box.width - 336));
      const below = box.height - (lineTop + cellH);
      const placement: PopupPlacement =
        below < 290 && lineTop > below ? { left, bottom: box.height - lineTop + 2 } : { left, top: lineTop + cellH + 2 };
      setCompletion({ view, placement });
    });
    const stopPrefs = onCompletionPrefs(() => controller.outputParsed());
    const writeParsed = terminal.onWriteParsed(() => controller.outputParsed());
    const bufferChange = terminal.buffer.onBufferChange((buffer) => controller.setAlternateScreen(buffer.type === "alternate"));
    terminalRef.current = terminal;
    fitRef.current = fit;
    searchRef.current = search;

    // The key the list took on keydown; its keypress and keyup are dropped too.
    let swallowed: string | null = null;
    terminal.attachCustomKeyEventHandler((event) => {
      if (event.type === "keydown") {
        if (controller.handleKey(event)) {
          event.preventDefault();
          swallowed = event.key;
          return false;
        }
      } else if (swallowed !== null && event.key === swallowed) {
        if (event.type === "keyup") swallowed = null;
        return false;
      }
      const shortcut = appShortcut(event);
      if (!shortcut) return true;
      event.preventDefault();
      if (shortcut === "copy") {
        const selection = terminal.getSelection();
        if (selection) void copyText(selection);
      } else if (shortcut === "paste") {
        void pasteText().then((text) => text && terminal.paste(text));
      } else if (shortcut === "find") {
        setFindOpen(true);
        window.requestAnimationFrame(() => findInputRef.current?.select());
      }
      // Other shortcuts are handled by the app's window listener.
      return false;
    });
    terminal.onData((data) => {
      controller.input(data);
      sessionRef.current?.write(data);
    });
    terminal.onResize(({ cols, rows }) => sessionRef.current?.resize(cols, rows));

    // Only the first start may reattach; "Reconnect" opens a fresh session.
    let reattach = sessionId;
    const start = () => {
      report({ state: "connecting" });
      let connected = false;
      let exited = false;
      const onEvent = (event: SessionEvent) => {
        if (disposed) return;
        if (event.kind === "data") {
          terminal.write(event.data);
          if (!connected) {
            connected = true;
            report({ state: "connected" });
          }
        } else {
          exited = true;
          sessionRef.current = null;
          report({ state: "exited", code: event.code });
        }
      };
      const attaching = reattach;
      reattach = undefined;
      (attaching
        ? attachSession(attaching, onEvent)
        : openSession(hostId, terminal.cols, terminal.rows, onEvent)
      )
        .then((session) => {
          if (disposed) {
            // A session nobody ever saw is closed. A live one is left alone:
            // the next view's own attach replaces this one's.
            if (!attaching) session.close();
          } else if (!exited) {
            sessionRef.current = session;
            if (attaching) {
              // The replay was produced for whatever size the session had.
              session.resize(terminal.cols, terminal.rows);
              if (!connected) {
                connected = true;
                report({ state: "connected" });
              }
            }
          }
        })
        .catch((reason) => {
          if (!disposed) report({ state: "exited", code: null, error: String(reason) });
        });
    };
    const unregister = registerTerminal(tabKey, {
      send(text, run) {
        terminal.paste(text);
        if (run) {
          controller.input("\r");
          sessionRef.current?.write("\r");
        }
        terminal.focus();
      },
      connected: () => sessionRef.current !== null,
      close() {
        sessionRef.current?.close();
        sessionRef.current = null;
      },
    });
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
    const stopSettings = onSettings((next) => {
      terminal.options.fontSize = next.terminalFontSize;
      terminal.options.scrollback = next.terminalScrollback;
      updateTheme();
      if (container.clientWidth > 0) fit.fit();
    });

    return () => {
      disposed = true;
      unregister();
      stopView();
      stopPrefs();
      writeParsed.dispose();
      bufferChange.dispose();
      controller.dispose();
      controllerRef.current = null;
      (files as FilesHandle | null)?.close();
      setCompletion(null);
      scheme.removeEventListener("change", updateTheme);
      stopSettings();
      observer.disconnect();
      // Detach only: ending the session is closeTab's job, so that a reload
      // (or this effect re-running) leaves it running in the daemon.
      sessionRef.current?.detach();
      sessionRef.current = null;
      terminal.dispose();
      terminalRef.current = null;
    };
  }, [hostId, report, sessionId, tabKey]);

  useEffect(() => {
    if (!active) return;
    const frame = window.requestAnimationFrame(() => {
      fitRef.current?.fit();
      terminalRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [active]);

  const find = (forward: boolean) => {
    if (!query) return;
    if (forward) searchRef.current?.findNext(query);
    else searchRef.current?.findPrevious(query);
  };

  const closeFind = () => {
    setFindOpen(false);
    searchRef.current?.clearDecorations();
    terminalRef.current?.focus();
  };

  return (
    <div ref={wrapperRef} className="relative h-full min-w-0 flex-1 bg-bg">
      <div ref={containerRef} className="absolute inset-0 py-2 pl-3" />

      {completion?.view.popup && (
        <CompletionPopup
          popup={completion.view.popup}
          placement={completion.placement}
          onHover={(index) => controllerRef.current?.highlight(index)}
          onPick={(index) => {
            const item = completion.view.popup?.items[index];
            if (item) controllerRef.current?.accept(item);
            terminalRef.current?.focus();
          }}
        />
      )}

      <AnimatePresence>
      {findOpen && (
        <motion.div
          initial={{ opacity: 0, y: -6 }}
          animate={{ opacity: 1, y: 0, transition: { duration: duration.toggle, ease: ease.emphasized } }}
          exit={{ opacity: 0, y: -6, transition: leave }}
          className="material-bar absolute top-3 right-4 z-dropdown flex items-center gap-1 rounded-sm p-1 shadow-2 ring-1 ring-separator"
        >
          <Input
            ref={findInputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") find(!event.shiftKey);
              if (event.key === "Escape") closeFind();
            }}
            placeholder={t.find}
            aria-label={t.find}
            className="h-7 w-52"
          />
          <IconButton label={t.findPrevious} onClick={() => find(false)}>
            <ChevronUp />
          </IconButton>
          <IconButton label={t.findNext} onClick={() => find(true)}>
            <ChevronDown />
          </IconButton>
          <IconButton label={t.closeFind} onClick={closeFind}>
            <X />
          </IconButton>
        </motion.div>
      )}
      </AnimatePresence>

      <AnimatePresence>
      {status.state === "exited" && (
        <div className="pointer-events-none absolute inset-x-0 bottom-6 flex justify-center px-4">
          <motion.div
            role="status"
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1, transition: enter }}
            exit={{ opacity: 0, y: 8, transition: leave }}
            className="pointer-events-auto flex max-w-xl flex-wrap items-center gap-3 rounded-md bg-bg-elevated px-4 py-3 shadow-3"
          >
            <Unplug className="size-5 shrink-0 text-label-2" strokeWidth={1.5} />
            <div className="min-w-0 flex-1">
              <p className="text-callout font-medium text-label-1">
                {status.error ? t.sessionFailed : t.sessionEnded}
              </p>
              <p className="selectable truncate text-caption font-normal text-label-2">
                {status.error ?? (status.code === null ? "" : `${t.exitCode} ${status.code}`)}
              </p>
            </div>
            <Button variant="primary" size="sm" onClick={() => startRef.current()}>
              <RotateCw />
              {hostId ? t.reconnect : t.reopen}
            </Button>
            {hostId && (
              <Button variant="secondary" size="sm" onClick={() => void openExternal(hostId)}>
                <SquareArrowOutUpRight />
                {t.openExternal}
              </Button>
            )}
          </motion.div>
        </div>
      )}
      </AnimatePresence>
    </div>
  );
}

function IconButton({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label}>
      <Button variant="ghost" size="icon-sm" aria-label={label} onClick={onClick}>
        {children}
      </Button>
    </Tooltip>
  );
}
