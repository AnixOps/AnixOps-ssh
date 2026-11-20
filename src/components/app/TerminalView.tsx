import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronDown, ChevronUp, LoaderCircle, RotateCw, Sparkles, SquareArrowOutUpRight, Unplug, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { CompletionPopup, type PopupPlacement } from "@/components/app/CompletionPopup";
import { AskAiPanel, ASK_AI_WIDTH } from "@/components/app/AskAiPanel";
import { useAiReady } from "@/lib/aiBackend";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { PasteConfirm, BroadcastBadge } from "@/components/app/PaneOverlays";
import { Tooltip } from "@/components/ui/tooltip";
import {
  attachSession,
  copyText,
  isVaultLocked,
  loadHosts,
  openFiles,
  openExternal,
  openSession,
  pasteText,
  type FilesHandle,
  type SessionEvent,
  type SessionHandle,
  type SshEngine,
} from "@/lib/backend";
import { isSerialPermissionError } from "@/lib/serialUi";
import { requestVaultUnlock } from "@/lib/vaultUi";
import { CompletionController, type ViewState } from "@/lib/completion/controller";
import { completionPrefs, commandHistory, onCompletionPrefs } from "@/lib/completion/prefs";
import { createPathLister } from "@/lib/completion/remotePaths";
import { commandNames, loadSpec } from "@/lib/completion/specs";
import { commandOutput, lastLines } from "@/lib/ai/buffer";
import type { ContextSource } from "@/lib/ai/context";
import { attachShellIntegration, type ShellIntegrationHandle } from "@/lib/shellIntegration/attach";
import { ShellIntegrationLineSource } from "@/lib/shellIntegration/lineSource";
import { pasteNeedsConfirm, type BroadcastScope, targetsFor } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { duration, ease, enter, leave } from "@/lib/motion";
import { onSettings, settings } from "@/lib/settings";
import { appShortcut } from "@/lib/shortcuts";
import { registerTerminal, terminalFor } from "@/lib/terminals";
import { liveTerminalOptions, terminalOptions, UNICODE_VERSION } from "@/lib/terminalOptions";
import { exitTitle, RESIZE_DEBOUNCE_MS, TERMINAL_RESET, trailingDebounce, type SessionEnd } from "@/lib/terminalSession";
import { terminalFont, terminalTheme } from "@/lib/terminalTheme";

/** The SSH engine a session runs on, and why. */
export interface EngineInfo {
  engine: SshEngine;
  reason: string | null;
}

export type SessionStatus =
  | { state: "connecting" }
  | { state: "connected"; engine?: EngineInfo }
  /** The connection to the background daemon dropped; the session is re-attached when it is back. */
  | { state: "reconnecting" }
  | ({ state: "exited"; error?: string } & SessionEnd);

export function TerminalView({
  tabKey,
  hostId,
  sessionId,
  active,
  onStatus,
  onSession,
  broadcast = null,
}: {
  tabKey: string;
  hostId: string | null;
  /** A live session to reattach to instead of opening a new one. */
  sessionId?: string;
  active: boolean;
  onStatus: (key: string, status: SessionStatus) => void;
  /** Reports the daemon session behind this view, so a layout can be restored. */
  onSession?: (key: string, sessionId: string) => void;
  /** Set while this pane receives broadcast input; shows the indicator. */
  broadcast?: BroadcastScope;
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
  /** Set while the session is over, so Enter reconnects instead of typing. */
  const endedRef = useRef(false);
  const integrationRef = useRef<ShellIntegrationHandle | null>(null);
  const [askOpen, setAskOpen] = useState(false);
  // Hidden until AI is on and configured, so it cannot be hit by accident.
  const aiReady = useAiReady();
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const findInputRef = useRef<HTMLInputElement>(null);
  const [pendingPaste, setPendingPaste] = useState<{ text: string; count: number } | null>(null);
  const pasteRef = useRef<(text: string, all: boolean) => void>(() => {});

  const report = useCallback(
    (next: SessionStatus) => {
      endedRef.current = next.state === "exited";
      setStatus(next);
      onStatus(tabKey, next);
    },
    [onStatus, tabKey],
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;

    const terminal = new Terminal(terminalOptions(settings(), { fontFamily: terminalFont(), theme: terminalTheme() }));
    const fit = new FitAddon();
    const search = new SearchAddon();
    terminal.loadAddon(fit);
    terminal.loadAddon(search);
    terminal.loadAddon(new Unicode11Addon());
    terminal.unicode.activeVersion = UNICODE_VERSION;
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
    const cursorOf = () => {
      const active = terminal.buffer.active;
      return { x: active.cursorX, y: active.cursorY, absoluteY: active.baseY + active.cursorY, cols: terminal.cols };
    };
    // Keystrokes until the host's shell reports itself through the opt-in
    // shell integration; from then on the shell's own command lines and cwd.
    const lineSource = new ShellIntegrationLineSource(() => {
      const cursor = cursorOf();
      return { x: cursor.x, y: cursor.absoluteY, cols: cursor.cols };
    });
    const integration = attachShellIntegration(terminal, lineSource);
    integrationRef.current = integration;
    const controller = new CompletionController({
      source: lineSource,
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
      cursor: cursorOf,
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
        void pasteText().then((text) => text && routePaste(text));
      } else if (shortcut === "find") {
        setFindOpen(true);
        window.requestAnimationFrame(() => findInputRef.current?.select());
      } else if (shortcut === "prev-prompt" || shortcut === "next-prompt") {
        integration.jump(shortcut === "prev-prompt" ? "previous" : "next");
      }
      // Other shortcuts are handled by the app's window listener.
      return false;
    });
    // Broadcast: what the user types here also goes to the other panes in
    // scope. xterm's own replies to the shell arrive through onData as well,
    // so only data produced inside a key or input event is mirrored.
    let typing = false;
    let typingTimer: number | undefined;
    const markTyping = () => {
      typing = true;
      window.clearTimeout(typingTimer);
      typingTimer = window.setTimeout(() => (typing = false), 0);
    };
    const typingEvents = ["keydown", "keypress", "input", "compositionend"];
    for (const type of typingEvents) container.addEventListener(type, markTyping, true);
    const receivers = () => targetsFor(tabKey, (id) => terminalFor(id)?.connected() ?? false);
    terminal.onData((data) => {
      if (endedRef.current) {
        // Like PuTTY-style clients: Enter on an ended session starts it again.
        if (data === "\r") startRef.current();
        return;
      }
      controller.input(data);
      sessionRef.current?.write(data);
      if (typing) for (const id of receivers()) terminalFor(id)?.write(data);
    });
    const pasteEverywhere = (text: string, all: boolean) => {
      terminal.paste(text);
      if (all) for (const id of receivers()) terminalFor(id)?.paste(text);
      terminal.focus();
    };
    pasteRef.current = pasteEverywhere;
    function routePaste(text: string) {
      const count = receivers().length;
      if (count === 0) terminal.paste(text);
      else if (pasteNeedsConfirm(text, count)) setPendingPaste({ text, count });
      else pasteEverywhere(text, true);
    }
    const onPaste = (event: ClipboardEvent) => {
      const text = event.clipboardData?.getData("text") ?? "";
      if (!text || receivers().length === 0) return;
      event.preventDefault();
      event.stopPropagation();
      routePaste(text);
    };
    container.addEventListener("paste", onPaste, true);
    // The view fits at once; the session hears the size once dragging
    // settles, so the remote app redraws once instead of on every frame.
    const sendResize = trailingDebounce(
      (cols: number, rows: number) => sessionRef.current?.resize(cols, rows),
      RESIZE_DEBOUNCE_MS,
    );
    terminal.onResize(({ cols, rows }) => sendResize(cols, rows));

    // Only the first start may reattach; "Reconnect" opens a fresh session.
    let reattach = sessionId;
    const start = () => {
      report({ state: "connecting" });
      let connected = false;
      let exited = false;
      // Known once the session is open; the first output may come sooner.
      let engine: EngineInfo | undefined;
      const onEvent = (event: SessionEvent) => {
        if (disposed) return;
        if (event.kind === "data") {
          terminal.write(event.data);
          if (!connected) {
            connected = true;
            report({ state: "connected", engine });
          }
        } else if (event.kind === "reconnecting") {
          if (!exited) report({ state: "reconnecting" });
        } else if (event.kind === "resumed") {
          if (!exited) {
            connected = true;
            report({ state: "connected", engine });
            // The view may have been resized while the link was down.
            sessionRef.current?.resize(terminal.cols, terminal.rows);
          }
        } else {
          exited = true;
          sessionRef.current = null;
          report({ state: "exited", code: event.code, reason: event.reason, signal: event.signal });
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
            onSession?.(tabKey, session.id);
            if (session.engine) {
              engine = { engine: session.engine, reason: session.engineReason ?? null };
              if (connected) report({ state: "connected", engine });
            }
            if (attaching) {
              // The replay was produced for whatever size the session had.
              session.resize(terminal.cols, terminal.rows);
              if (!connected) {
                connected = true;
                report({ state: "connected", engine });
              }
            }
          }
        })
        .catch((reason) => {
          if (isVaultLocked(reason)) requestVaultUnlock();
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
      write: (data) => sessionRef.current?.write(data),
      paste: (text) => terminal.paste(text),
      connected: () => sessionRef.current !== null,
      close() {
        sessionRef.current?.close();
        sessionRef.current = null;
      },
    });
    startRef.current = () => {
      // Undo modes the old session left on (alternate screen, mouse
      // reporting, bracketed paste) before the new one starts.
      terminal.write(TERMINAL_RESET + "\r\n");
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
      Object.assign(terminal.options, liveTerminalOptions(next));
      updateTheme();
      if (container.clientWidth > 0) fit.fit();
    });

    return () => {
      disposed = true;
      unregister();
      stopView();
      stopPrefs();
      integration.dispose();
      integrationRef.current = null;
      writeParsed.dispose();
      bufferChange.dispose();
      controller.dispose();
      controllerRef.current = null;
      (files as FilesHandle | null)?.close();
      setCompletion(null);
      for (const type of typingEvents) container.removeEventListener(type, markTyping, true);
      container.removeEventListener("paste", onPaste, true);
      window.clearTimeout(typingTimer);
      sendResize.cancel();
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
  }, [hostId, onSession, report, sessionId, tabKey]);

  useEffect(() => {
    if (!active) return;
    const frame = window.requestAnimationFrame(() => {
      fitRef.current?.fit();
      terminalRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [active]);

  // The badge strip changes the usable height.
  useEffect(() => {
    const frame = window.requestAnimationFrame(() => fitRef.current?.fit());
    return () => window.cancelAnimationFrame(frame);
  }, [broadcast]);

  // What the "Ask AI" panel may offer; read only when the person asks for it.
  const askSource = useMemo<ContextSource>(() => {
    const bottom = (terminal: Terminal) => terminal.buffer.active.baseY + terminal.buffer.active.cursorY;
    return {
      selection: () => terminalRef.current?.getSelection() ?? "",
      lastLines: (count) => {
        const terminal = terminalRef.current;
        return terminal ? lastLines(terminal.buffer.active, bottom(terminal), count) : "";
      },
      lastCommand: () => {
        const terminal = terminalRef.current;
        const reported = integrationRef.current?.lastCommand();
        if (!terminal || !reported) return null;
        return {
          command: reported.command,
          exitCode: reported.exitCode,
          output: commandOutput(terminal.buffer.active, reported.promptLine, reported.nextPromptLine, bottom(terminal)),
        };
      },
    };
  }, []);

  // The panel narrows the terminal.
  useEffect(() => {
    const frame = window.requestAnimationFrame(() => fitRef.current?.fit());
    return () => window.cancelAnimationFrame(frame);
  }, [askOpen]);

  const closeAsk = () => {
    setAskOpen(false);
    terminalRef.current?.focus();
  };

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
      {/* Broadcasting reserves a strip below the text for its badge. */}
      <div ref={containerRef} className={`absolute top-2 left-3 ${broadcast ? "bottom-9" : "bottom-2"}`}
        style={{ right: askOpen ? ASK_AI_WIDTH : 0 }}
      />
      {broadcast && <BroadcastBadge scope={broadcast} />}
      {pendingPaste && (
        <PasteConfirm
          count={pendingPaste.count}
          onChoose={(all) => {
            const { text } = pendingPaste;
            setPendingPaste(null);
            if (all !== null) pasteRef.current(text, all);
            else terminalRef.current?.focus();
          }}
        />
      )}

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

      {aiReady && !askOpen && (
        <div className="absolute top-3 right-4 z-dropdown">
          <Tooltip label={t.askAi}>
            <Button variant="quiet" size="icon-sm" aria-label={t.askAi} onClick={() => setAskOpen(true)}>
              <Sparkles />
            </Button>
          </Tooltip>
        </div>
      )}
      {askOpen && (
        <AskAiPanel
          source={askSource}
          onInsert={(command) => {
            // Fills the input line only; Enter is the person's to press.
            terminalRef.current?.paste(command);
            terminalRef.current?.focus();
          }}
          onClose={closeAsk}
        />
      )}

      <AnimatePresence>
      {findOpen && (
        <motion.div
          initial={{ opacity: 0, y: -6 }}
          animate={{ opacity: 1, y: 0, transition: { duration: duration.toggle, ease: ease.emphasized } }}
          exit={{ opacity: 0, y: -6, transition: leave }}
          style={{ right: askOpen ? `calc(${ASK_AI_WIDTH} + 1rem)` : aiReady ? "3.25rem" : "1rem" }}
          className="material-bar absolute top-3 z-dropdown flex items-center gap-1 rounded-sm p-1 shadow-2 ring-1 ring-separator"
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
      {status.state === "reconnecting" && (
        <div className="pointer-events-none absolute inset-x-0 bottom-6 flex justify-center px-4">
          <motion.div
            role="status"
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1, transition: enter }}
            exit={{ opacity: 0, y: 8, transition: leave }}
            className="flex items-center gap-2 rounded-md bg-bg-elevated px-3 py-2 shadow-3"
          >
            <LoaderCircle
              className="size-4 shrink-0 animate-spin text-label-2 motion-reduce:animate-none"
              strokeWidth={1.5}
            />
            <p className="text-callout font-medium text-label-1">{t.reconnecting}</p>
          </motion.div>
        </div>
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
                {status.error ? t.sessionFailed : exitTitle(status)}
              </p>
              <p className="selectable line-clamp-4 break-words text-callout text-label-2">
                {status.error
                  ? isSerialPermissionError(status.error)
                    ? t.serialPermissionDenied
                    : status.error
                  : hostId
                    ? t.enterToReconnect
                    : t.enterToReopen}
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
      <Button variant="quiet" size="icon-sm" aria-label={label} onClick={onClick}>
        {children}
      </Button>
    </Tooltip>
  );
}
