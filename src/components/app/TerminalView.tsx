import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { CaseSensitive, ChevronDown, ChevronUp, ClipboardPaste, Copy, Eraser, LoaderCircle, Regex, RotateCw, Search, Sparkles, SquareArrowOutUpRight, TextSelect, Unplug, WholeWord, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { CompletionPopup, type PopupPlacement } from "@/components/app/CompletionPopup";
import { AskAiPanel, ASK_AI_WIDTH } from "@/components/app/AskAiPanel";
import { useAiReady } from "@/lib/aiBackend";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ContextMenu, type MenuEntry } from "@/components/ui/menu";
import { PasteConfirm, BroadcastBadge } from "@/components/app/PaneOverlays";
import { environmentName, environmentTone, isProduction } from "@/lib/environment";
import { TransferSheet } from "@/components/app/TransferSheet";
import { Tooltip } from "@/components/ui/tooltip";
import {
  attachSession,
  copyText,
  isVaultLocked,
  loadHosts,
  onAskpass,
  onHostKey,
  openFiles,
  openExternal,
  openSession,
  openUrl,
  pasteText,
  type FilesHandle,
  type HostEnvironment,
  type SessionEvent,
  type SessionHandle,
  type SshEngine,
} from "@/lib/backend";
import {
  beginRetry,
  decideRetry,
  nextRetry,
  nudgeRetry,
  retryNow,
  secondsLeft,
  type RetryPlan,
} from "@/lib/autoReconnect";
import { isSerialPermissionError } from "@/lib/serialUi";
import { requestVaultUnlock } from "@/lib/vaultUi";
import { CompletionController, type ViewState } from "@/lib/completion/controller";
import { completionPrefs, commandHistory, onCompletionPrefs } from "@/lib/completion/prefs";
import { createPathLister } from "@/lib/completion/remotePaths";
import { commandNames, loadSpec } from "@/lib/completion/specs";
import { commandOutput, lastLines } from "@/lib/ai/buffer";
import type { ContextSource } from "@/lib/ai/context";
import { attachShellIntegration, type ShellIntegrationHandle } from "@/lib/shellIntegration/attach";
import { clearTerminalCwd, publishTerminalCwd } from "@/lib/terminalCwd";
import { ShellIntegrationLineSource } from "@/lib/shellIntegration/lineSource";
import { pasteNeedsConfirm, type BroadcastScope, targetsFor } from "@/lib/broadcast";
import { failureText } from "@/lib/connectFailureText";
import { requestHostEdit } from "@/lib/hostEditUi";
import { t } from "@/lib/i18n";
import { duration, ease, enter, leave } from "@/lib/motion";
import { animationFrameSchedule, OutputBuffer } from "@/lib/outputBuffer";
import { onSettings, settings } from "@/lib/settings";
import { appShortcut, isMac } from "@/lib/shortcuts";
import { shortcutHint } from "@/lib/shortcutTable";
import { capabilityReply, mouseTrackingRequest, parseOsc52, sanitizeTitle, shouldOpenLink } from "@/lib/terminalInput";
import { binaryInput } from "@/lib/terminalBytes";
import { registerTerminal, terminalFor } from "@/lib/terminals";
import { liveTerminalOptions, terminalOptions, UNICODE_VERSION } from "@/lib/terminalOptions";
import { attachCommand, claimSlot, hostKeepSession, keepSessionName, reconnectDivider } from "@/lib/keepSession";
import { exitTitle, RESIZE_DEBOUNCE_MS, TERMINAL_RESET, trailingDebounce, type SessionEnd } from "@/lib/terminalSession";
import { searchDecorations, terminalFont, terminalTheme } from "@/lib/terminalTheme";
import { createTerminalTransfer } from "@/lib/terminalTransferFiles";
import { cn } from "@/lib/utils";

/** Font zoom stays within what stays legible and fits a window. */
const MIN_FONT_SIZE = 6;
const MAX_FONT_SIZE = 48;

/** The SSH engine a session runs on, and why. */
export interface EngineInfo {
  engine: SshEngine;
  reason: string | null;
}

/** What the remote side said about itself during one session. */
export interface SessionMeta {
  /** The window title the remote program set (OSC 0/2), sanitized. */
  remoteTitle?: string;
  /** The bell rang while the pane was not in front. */
  bell?: boolean;
}

export type SessionStatus = (
  | { state: "connecting" }
  | { state: "connected"; engine?: EngineInfo }
  /** The connection to the background daemon dropped; the session is re-attached when it is back. */
  | { state: "reconnecting" }
  | ({ state: "exited"; error?: string } & SessionEnd)
) &
  SessionMeta;

export function TerminalView({
  tabKey,
  hostId,
  sessionId,
  active,
  onStatus,
  onSession,
  broadcast = null,
  environment = null,
  hostName = "",
}: {
  tabKey: string;
  hostId: string | null;
  /** The host's environment: production and staging get a coloured top edge, production asks before a multi-line paste. */
  environment?: HostEnvironment | null;
  hostName?: string;
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
  const cancelAutoRef = useRef<() => void>(() => {});
  /** The next automatic attempt, or "gaveup" once the retries ran out. */
  const [autoRetry, setAutoRetry] = useState<RetryPlan | "gaveup" | null>(null);
  const [status, setStatus] = useState<SessionStatus>({ state: "connecting" });
  /** Set while the session is over, so Enter reconnects instead of typing. */
  const endedRef = useRef(false);
  const integrationRef = useRef<ShellIntegrationHandle | null>(null);
  const [askOpen, setAskOpen] = useState(false);
  // Hidden until AI is on and configured, so it cannot be hit by accident.
  const aiReady = useAiReady();
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [regex, setRegex] = useState(false);
  const [results, setResults] = useState<{ index: number; count: number } | null>(null);
  const [badPattern, setBadPattern] = useState(false);
  const findInputRef = useRef<HTMLInputElement>(null);
  const [pendingPaste, setPendingPaste] = useState<{ text: string; count: number } | null>(null);
  const productionRef = useRef(isProduction(environment));
  productionRef.current = isProduction(environment);
  const pasteRef = useRef<(text: string, all: boolean) => void>(() => {});
  /** rz/sz: output goes through it, and it takes the terminal over while a transfer runs. */
  const transfer = useMemo(createTerminalTransfer, []);
  useEffect(() => () => transfer.dispose(), [transfer]);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const actionsRef = useRef({ copy: () => {}, paste: () => {}, clear: () => {} });
  const metaRef = useRef<SessionMeta>({});
  const statusRef = useRef<SessionStatus>({ state: "connecting" });
  const activeRef = useRef(active);
  activeRef.current = active;

  const report = useCallback(
    (next: SessionStatus) => {
      endedRef.current = next.state === "exited";
      const full = { ...next, ...metaRef.current };
      statusRef.current = full;
      setStatus(full);
      onStatus(tabKey, full);
    },
    [onStatus, tabKey],
  );
  const updateMeta = useCallback(
    (patch: SessionMeta) => {
      metaRef.current = { ...metaRef.current, ...patch };
      report(statusRef.current);
    },
    [report],
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
    // Links open with Ctrl-click (⌘-click), so a plain click still selects.
    const openLink = (event: MouseEvent, uri: string) => {
      if (shouldOpenLink(uri, event, isMac)) void openUrl(uri).catch(console.error);
    };
    const linkHover = {
      hover: () => (container.title = t.openLinkHint.replace("{key}", isMac ? "⌘" : "Ctrl")),
      leave: () => container.removeAttribute("title"),
    };
    terminal.loadAddon(new WebLinksAddon(openLink, linkHover));
    // OSC 8 hyperlinks (ls --hyperlink, gcc, systemd, delta, lazygit) follow
    // the same rule; xterm.js's default would ask with a browser dialog.
    terminal.options.linkHandler = { activate: openLink, ...linkHover };
    terminal.loadAddon(new Unicode11Addon());
    terminal.unicode.activeVersion = UNICODE_VERSION;
    terminal.open(container);
    // The terminal end-to-end test reads the buffer it renders; never set in the app.
    const e2eTerminals = (window as { __anixopsE2eTerminals?: Map<string, Terminal> }).__anixopsE2eTerminals;
    e2eTerminals?.set(tabKey, terminal);
    // Headless test browsers have no usable WebGL, so automation uses the DOM renderer.
    // `?webgl=1` keeps WebGL under automation, for the terminal end-to-end test.
    // Ligatures need the DOM renderer too: WebGL draws each cell on its own,
    // while the browser shapes a run of text in the DOM (see index.css).
    const webglAllowed = !navigator.webdriver || new URLSearchParams(window.location.search).has("webgl");
    let webgl: WebglAddon | null = null;
    const applyRenderer = (ligatures: boolean) => {
      container.classList.toggle("terminal-ligatures", ligatures);
      if (ligatures && webgl) {
        webgl.dispose();
        webgl = null;
      } else if (!ligatures && !webgl && webglAllowed) {
        try {
          const addon = new WebglAddon();
          addon.onContextLoss(() => {
            addon.dispose();
            if (webgl === addon) webgl = null;
          });
          terminal.loadAddon(addon);
          webgl = addon;
        } catch {
          // The DOM renderer is the fallback when WebGL is unavailable.
        }
      }
    };
    applyRenderer(settings().terminalLigatures);
    fit.fit();
    // Output is written once per frame. The daemon protocol has no flow
    // control to pause reads, so this only coalesces (see outputBuffer.ts).
    const output = new OutputBuffer({
      write: (data, done) => terminal.write(data, done),
      schedule: animationFrameSchedule,
    });
    const searchResults = search.onDidChangeResults(({ resultIndex, resultCount }) =>
      setResults({ index: resultIndex, count: resultCount }),
    );

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
    // The files pane can follow this terminal's folder.
    const cwdOwner = Symbol("terminal");
    if (hostId) {
      // A folder comes from the integration script or a plain OSC 7 report.
      lineSource.onChange(() =>
        publishTerminalCwd(hostId, cwdOwner, { integration: lineSource.isActive() || lineSource.cwd() !== undefined, cwd: lineSource.cwd() }),
      );
    }
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
    transfer.bind({
      // Through the frame buffer, so passed-through output stays in order.
      write: (text) => output.push(text),
      send: (text) => {
        const session = sessionRef.current;
        return session?.writeAsync ? session.writeAsync(text) : session?.write(text);
      },
    });
    terminalRef.current = terminal;
    fitRef.current = fit;
    searchRef.current = search;

    // Font zoom for this view only (Windows Terminal, iTerm2): a step from the
    // size in Settings, which Ctrl+0 / ⌘0 goes back to.
    let zoomSteps = 0;
    const zoomedSize = (base: number) => Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, base + zoomSteps));
    const zoom = (direction: 1 | -1 | 0) => {
      const base = settings().terminalFontSize;
      zoomSteps = direction === 0 ? 0 : zoomedSize(base + direction) - base;
      terminal.options.fontSize = zoomedSize(base);
      if (container.clientWidth > 0) fit.fit();
    };
    // Ctrl+wheel (⌘+wheel on macOS) zooms, instead of scrolling or zooming the page.
    const onZoomWheel = (event: WheelEvent) => {
      if (!(isMac ? event.metaKey : event.ctrlKey) || event.deltaY === 0) return;
      event.preventDefault();
      event.stopPropagation();
      zoom(event.deltaY < 0 ? 1 : -1);
    };
    container.addEventListener("wheel", onZoomWheel, { capture: true, passive: false });

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
      // Windows Terminal keys (a setting, on by default on Windows): Ctrl+C
      // copies when text is selected and is ^C otherwise; Ctrl+V pastes.
      if (
        !isMac &&
        event.type === "keydown" &&
        event.ctrlKey &&
        !event.shiftKey &&
        !event.altKey &&
        !event.metaKey &&
        settings().terminalCtrlCopyPaste
      ) {
        const key = event.code === "KeyC" || event.key.toLowerCase() === "c" ? "c" : event.code === "KeyV" || event.key.toLowerCase() === "v" ? "v" : null;
        if (key === "c" && terminal.hasSelection()) {
          event.preventDefault();
          copySelection();
          terminal.clearSelection();
          return false;
        }
        if (key === "v") {
          event.preventDefault();
          actionsRef.current.paste();
          return false;
        }
      }
      const shortcut = appShortcut(event);
      if (!shortcut) return true;
      event.preventDefault();
      if (shortcut === "zoom-in" || shortcut === "zoom-out" || shortcut === "zoom-reset") {
        zoom(shortcut === "zoom-in" ? 1 : shortcut === "zoom-out" ? -1 : 0);
      } else if (shortcut === "copy") {
        const selection = terminal.getSelection();
        if (selection) void copyText(selection);
      } else if (shortcut === "paste") {
        void pasteText().then((text) => text && routePaste(text));
      } else if (shortcut === "find") {
        setFindOpen(true);
        window.requestAnimationFrame(() => findInputRef.current?.select());
      } else if (shortcut === "clear-terminal") {
        // ⌘K on macOS: the screen and the scrollback; the prompt line stays.
        terminal.clear();
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
      if (transfer.input(data)) return;
      controller.input(data);
      sessionRef.current?.write(data);
      if (typing) for (const id of receivers()) terminalFor(id)?.write(data);
    });
    // Mouse reports in the original X10 encoding (Vim, mc and others that do
    // not ask for SGR) arrive as binary; they go to this pane only.
    const binary = terminal.onBinary((data) => {
      if (endedRef.current || transfer.input(data)) return;
      sessionRef.current?.write(binaryInput(data));
    });
    // "Let programs use the mouse" off: requests to turn on mouse tracking are
    // dropped, so a click always selects. The last one is remembered, to be
    // applied if the setting is turned back on while the program runs.
    let requestedMouse: number | null = null;
    let localMouseWrites = 0;
    const mouseOn = terminal.parser.registerCsiHandler({ prefix: "?", final: "h" }, (params) => {
      const request = mouseTrackingRequest(params);
      if (request.mode === null) return false;
      if (localMouseWrites === 0) requestedMouse = request.mode;
      return request.only && !settings().terminalMouseReporting;
    });
    const mouseOff = terminal.parser.registerCsiHandler({ prefix: "?", final: "l" }, (params) => {
      if (localMouseWrites === 0 && mouseTrackingRequest(params).mode !== null) requestedMouse = null;
      return false;
    });
    const writeMouseMode = (sequence: string) => {
      localMouseWrites += 1;
      terminal.write(sequence, () => (localMouseWrites -= 1));
    };
    let mouseReporting = settings().terminalMouseReporting;
    const stopMouseSetting = onSettings((next) => {
      if (next.terminalMouseReporting === mouseReporting) return;
      mouseReporting = next.terminalMouseReporting;
      if (!mouseReporting && terminal.modes.mouseTrackingMode !== "none") writeMouseMode("\x1b[?1000l");
      else if (mouseReporting && requestedMouse !== null) writeMouseMode(`\x1b[?${requestedMouse}h`);
    });
    const pasteEverywhere = (text: string, all: boolean) => {
      terminal.paste(text);
      if (all) for (const id of receivers()) terminalFor(id)?.paste(text);
      terminal.focus();
    };
    pasteRef.current = pasteEverywhere;
    // One pane: ask before a multi-line paste the shell would run line by
    // line (no bracketed paste), unless that was turned off.
    const singleNeedsConfirm = (text: string) =>
      pasteNeedsConfirm(text, 0, {
        bracketedPaste: terminal.modes.bracketedPasteMode,
        warnMultiline: settings().terminalWarnMultilinePaste,
        production: productionRef.current,
      });
    function routePaste(text: string) {
      const count = receivers().length;
      if (count === 0 ? singleNeedsConfirm(text) : pasteNeedsConfirm(text, count)) setPendingPaste({ text, count });
      else if (count === 0) terminal.paste(text);
      else pasteEverywhere(text, true);
    }
    const onPaste = (event: ClipboardEvent) => {
      const text = event.clipboardData?.getData("text") ?? "";
      if (!text || (receivers().length === 0 && !singleNeedsConfirm(text))) return;
      event.preventDefault();
      event.stopPropagation();
      routePaste(text);
    };
    container.addEventListener("paste", onPaste, true);

    const copySelection = () => {
      const selection = terminal.getSelection();
      if (selection) void copyText(selection).catch(console.error);
    };
    actionsRef.current = {
      copy: copySelection,
      paste: () => void pasteText().then((text) => text && routePaste(text)),
      clear: () => terminal.clear(),
    };
    // Copy on select: once the mouse is released, not on every move of a drag.
    const onMouseUp = (event: MouseEvent) => {
      if (event.button === 0 && settings().terminalCopyOnSelect && terminal.hasSelection()) copySelection();
    };
    container.addEventListener("mouseup", onMouseUp);
    // Right-click: the menu, or (as in PuTTY and Windows Terminal) copy the
    // selection if there is one and paste otherwise.
    const onContextMenu = (event: MouseEvent) => {
      event.preventDefault();
      // A program that has the mouse (mc, Vim, tmux menus) gets the right
      // button; Shift-right-click still opens ours, as in Windows Terminal.
      if (terminal.modes.mouseTrackingMode !== "none" && !event.shiftKey && !(isMac && event.altKey)) return;
      if (settings().terminalRightClick === "paste") {
        if (terminal.hasSelection()) {
          copySelection();
          terminal.clearSelection();
        } else actionsRef.current.paste();
      } else setMenuAt({ x: event.clientX, y: event.clientY });
    };
    container.addEventListener("contextmenu", onContextMenu);

    // XTGETTCAP: Neovim and others ask whether 24-bit colour and undercurl work.
    const capabilities = terminal.parser.registerDcsHandler({ intermediates: "+", final: "q" }, (data) => {
      terminal.input(capabilityReply(data), false);
      return true;
    });
    // OSC 52: remote programs (tmux, Vim) may write the clipboard, never read it.
    const osc52 = terminal.parser.registerOscHandler(52, (data) => {
      if (settings().terminalOsc52) {
        const text = parseOsc52(data);
        if (text !== null) void copyText(text).catch(console.error);
      }
      return true;
    });
    const titleChange = terminal.onTitleChange((title) => updateMeta({ remoteTitle: sanitizeTitle(title) || undefined }));
    const bell = terminal.onBell(() => {
      if (!activeRef.current) updateMeta({ bell: true });
    });
    // The view fits at once; the session hears the size once dragging
    // settles, so the remote app redraws once instead of on every frame.
    const sendResize = trailingDebounce(
      (cols: number, rows: number) => sessionRef.current?.resize(cols, rows),
      RESIZE_DEBOUNCE_MS,
    );
    terminal.onResize(({ cols, rows }) => sendResize(cols, rows));

    // Automatic reconnect: a lost connection is retried with backoff, but
    // only after a session that was open, and never past a question the
    // user had to answer (password, passphrase, host key).
    let everConnected = false;
    // Keep session: the tmux/screen name stays with this view across reconnects.
    const keepSlot = hostId ? claimSlot(hostId) : null;
    let connectedAt: number | undefined;
    let prompted = false;
    let closing = false;
    let plan: RetryPlan | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stopTimer = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };
    const schedule = (next: RetryPlan) => {
      plan = next;
      stopTimer();
      setAutoRetry(next);
      timer = setTimeout(() => {
        timer = undefined;
        runStart(true);
      }, Math.max(0, next.dueAt - Date.now()));
    };
    const onEnded = (end: SessionEnd & { error?: string }) => {
      const decision = decideRetry(end, {
        enabled: settings().terminalAutoReconnect && !closing,
        remote: hostId !== null,
        wasConnected: everConnected,
        prompted,
      });
      if (!decision.retry) {
        plan = null;
        setAutoRetry(null);
        return;
      }
      const now = Date.now();
      // A session that held for a while starts a fresh chain; one that dropped
      // again at once keeps counting, so a flapping link still backs off.
      const stable = connectedAt !== undefined && now - connectedAt >= 10_000;
      const next = plan && !stable ? nextRetry(plan, now) : beginRetry(now);
      if (next) schedule(next);
      else {
        plan = null;
        setAutoRetry("gaveup");
      }
    };
    const unprompted = [
      onAskpass(() => (prompted = true)),
      onHostKey(() => (prompted = true)),
    ];
    const onOnline = () => {
      if (plan && timer !== undefined) schedule(nudgeRetry(Date.now()));
    };
    const onVisible = () => {
      if (document.visibilityState === "visible" && plan && timer !== undefined) schedule(retryNow(plan, Date.now()));
    };
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);
    cancelAutoRef.current = () => {
      stopTimer();
      plan = null;
      setAutoRetry(null);
    };

    // Only the first start may reattach; "Reconnect" opens a fresh session.
    let reattach = sessionId;
    const start = () => {
      metaRef.current = {};
      report({ state: "connecting" });
      let connected = false;
      let exited = false;
      connectedAt = undefined;
      prompted = false;
      const up = () => {
        connected = true;
        everConnected = true;
        connectedAt = Date.now();
        setAutoRetry(null);
      };
      // Known once the session is open; the first output may come sooner.
      let engine: EngineInfo | undefined;
      const onEvent = (event: SessionEvent) => {
        if (disposed) return;
        if (event.kind === "data") {
          transfer.feed(event.data);
          if (!connected) {
            up();
            report({ state: "connected", engine });
          }
        } else if (event.kind === "reconnecting") {
          if (!exited) report({ state: "reconnecting" });
        } else if (event.kind === "resumed") {
          if (!exited) {
            up();
            report({ state: "connected", engine });
            // The view may have been resized while the link was down.
            sessionRef.current?.resize(terminal.cols, terminal.rows);
          }
        } else {
          exited = true;
          transfer.cancel();
          sessionRef.current = null;
          report({ state: "exited", code: event.code, reason: event.reason, signal: event.signal });
          onEnded({ code: event.code, reason: event.reason, signal: event.signal });
        }
      };
      const attaching = reattach;
      reattach = undefined;
      const kept = hostId ? hostKeepSession(hostId) : "off";
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
            // Enter the multiplexer on a fresh shell (a reattached one is
            // already there). Typed once per shell, never replayed input.
            if (!attaching && kept !== "off" && keepSlot) {
              session.write(attachCommand(kept, keepSessionName(keepSlot.slot)));
            }
            if (session.engine) {
              engine = { engine: session.engine, reason: session.engineReason ?? null };
              if (connected) report({ state: "connected", engine });
            }
            if (attaching) {
              // The replay was produced for whatever size the session had.
              session.resize(terminal.cols, terminal.rows);
              if (!connected) {
                up();
                report({ state: "connected", engine });
              }
            }
          }
        })
        .catch((reason) => {
          if (isVaultLocked(reason)) requestVaultUnlock();
          if (!disposed) {
            report({ state: "exited", code: null, error: String(reason) });
            onEnded({ code: null, error: String(reason) });
          }
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
        closing = true;
        stopTimer();
        sessionRef.current?.close();
        sessionRef.current = null;
      },
    });
    const runStart = (auto: boolean) => {
      stopTimer();
      if (!auto) {
        plan = null;
        setAutoRetry(null);
      }
      // Undo modes the old session left on (alternate screen, mouse
      // reporting, bracketed paste) before the new one starts.
      output.flush();
      terminal.write(TERMINAL_RESET + "\r\n");
      // After a shell that was up, say that this is a new one (local only,
      // never sent to the host).
      if (everConnected && hostId) terminal.write(reconnectDivider(new Date(), hostKeepSession(hostId)));
      start();
      terminal.focus();
    };
    startRef.current = () => runStart(false);
    start();

    const observer = new ResizeObserver(() => {
      if (container.clientWidth > 0 && container.clientHeight > 0) fit.fit();
    });
    observer.observe(container);
    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    const updateTheme = () => (terminal.options.theme = terminalTheme());
    scheme.addEventListener("change", updateTheme);
    const stopSettings = onSettings((next) => {
      terminal.options.fontSize = zoomedSize(next.terminalFontSize);
      Object.assign(terminal.options, liveTerminalOptions(next, terminalFont()));
      applyRenderer(next.terminalLigatures);
      updateTheme();
      if (container.clientWidth > 0) fit.fit();
    });

    return () => {
      disposed = true;
      keepSlot?.release();
      transfer.cancel();
      transfer.bind(null);
      stopTimer();
      for (const stop of unprompted) stop();
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
      cancelAutoRef.current = () => {};
      unregister();
      stopView();
      stopPrefs();
      integration.dispose();
      if (hostId) clearTerminalCwd(hostId, cwdOwner);
      integrationRef.current = null;
      writeParsed.dispose();
      searchResults.dispose();
      output.dispose();
      bufferChange.dispose();
      controller.dispose();
      controllerRef.current = null;
      (files as FilesHandle | null)?.close();
      setCompletion(null);
      for (const type of typingEvents) container.removeEventListener(type, markTyping, true);
      container.removeEventListener("paste", onPaste, true);
      container.removeEventListener("mouseup", onMouseUp);
      container.removeEventListener("contextmenu", onContextMenu);
      container.removeEventListener("wheel", onZoomWheel, { capture: true });
      osc52.dispose();
      capabilities.dispose();
      binary.dispose();
      mouseOn.dispose();
      mouseOff.dispose();
      stopMouseSetting();
      titleChange.dispose();
      bell.dispose();
      window.clearTimeout(typingTimer);
      sendResize.cancel();
      scheme.removeEventListener("change", updateTheme);
      stopSettings();
      observer.disconnect();
      // Detach only: ending the session is closeTab's job, so that a reload
      // (or this effect re-running) leaves it running in the daemon.
      sessionRef.current?.detach();
      sessionRef.current = null;
      e2eTerminals?.delete(tabKey);
      terminal.dispose();
      terminalRef.current = null;
    };
  }, [hostId, onSession, report, sessionId, tabKey, transfer, updateMeta]);

  useEffect(() => {
    if (!active) return;
    if (metaRef.current.bell) updateMeta({ bell: false });
    const frame = window.requestAnimationFrame(() => {
      fitRef.current?.fit();
      terminalRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [active, updateMeta]);

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

  const find = (forward: boolean, incremental = false) => {
    const search = searchRef.current;
    if (!search) return;
    if (!query) {
      search.clearDecorations();
      setResults(null);
      setBadPattern(false);
      return;
    }
    const options = { caseSensitive, wholeWord, regex, incremental, decorations: searchDecorations() };
    try {
      setBadPattern(false);
      if (forward) search.findNext(query, options);
      else search.findPrevious(query, options);
    } catch {
      // An unfinished regular expression while typing.
      setBadPattern(true);
      setResults(null);
    }
  };

  // Highlights follow the text and the toggles as they change.
  useEffect(() => {
    if (!findOpen) return;
    find(true, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [findOpen, query, caseSensitive, wholeWord, regex]);

  const closeFind = () => {
    setFindOpen(false);
    setResults(null);
    searchRef.current?.clearDecorations();
    terminalRef.current?.focus();
  };

  const closeMenu = useCallback(() => {
    setMenuAt(null);
    terminalRef.current?.focus();
  }, []);
  const menuItems: MenuEntry[] = [
    {
      key: "copy",
      label: t.copy,
      icon: <Copy />,
      shortcut: shortcutHint("copy"),
      disabled: !terminalRef.current?.hasSelection(),
      onSelect: () => actionsRef.current.copy(),
    },
    {
      key: "paste",
      label: t.paste,
      icon: <ClipboardPaste />,
      shortcut: shortcutHint("paste"),
      onSelect: () => actionsRef.current.paste(),
    },
    {
      key: "select-all",
      label: t.selectAll,
      icon: <TextSelect />,
      onSelect: () => terminalRef.current?.selectAll(),
    },
    {
      key: "find",
      label: t.find,
      icon: <Search />,
      shortcut: shortcutHint("find", "terminal"),
      onSelect: () => {
        setFindOpen(true);
        window.requestAnimationFrame(() => findInputRef.current?.select());
      },
    },
    "separator",
    { key: "clear", label: t.clearTerminal, icon: <Eraser />, onSelect: () => actionsRef.current.clear() },
  ];

  const tone = environmentTone(environment);
  return (
    <div ref={wrapperRef} className="relative h-full min-w-0 flex-1 bg-bg">
      {/* Broadcasting reserves a strip below the text for its badge. */}
      <div ref={containerRef} className={`absolute top-2 left-3 ${broadcast ? "bottom-9" : "bottom-2"}`}
        style={{ right: askOpen ? ASK_AI_WIDTH : 0 }}
      />
      {broadcast && <BroadcastBadge scope={broadcast} />}
      {tone && (
        <div
          role="img"
          aria-label={t.envBadgeAria.replace("{env}", environmentName(environment!))}
          data-env-edge={environment}
          className={cn("pointer-events-none absolute inset-x-0 top-0 z-sticky h-1", tone === "danger" ? "bg-danger" : "bg-warning")}
        />
      )}
      <ContextMenu
        label={t.terminalMenu}
        at={menuAt}
        items={menuItems}
        onClose={closeMenu}
      />
      {pendingPaste && (
        <PasteConfirm
          count={pendingPaste.count}
          lines={pendingPaste.text.split(/\r\n|\r|\n/).filter(Boolean).length}
          production={isProduction(environment) ? hostName : undefined}
          onChoose={(all) => {
            const { text } = pendingPaste;
            setPendingPaste(null);
            if (all !== null) pasteRef.current(text, all);
            else terminalRef.current?.focus();
          }}
        />
      )}

      <TransferSheet transfer={transfer} />

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
          onKeyDown={(event) => event.key === "Escape" && closeFind()}
          className="material-bar absolute top-3 z-dropdown flex items-center gap-1 rounded-sm p-1 shadow-2 ring-1 ring-separator"
        >
          <Input
            ref={findInputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") find(!event.shiftKey);
            }}
            placeholder={t.find}
            aria-label={t.find}
            aria-invalid={badPattern || undefined}
            className={cn("h-7 w-44", badPattern && "text-danger")}
          />
          <ToggleButton label={t.findCaseSensitive} on={caseSensitive} onClick={() => setCaseSensitive((on) => !on)}>
            <CaseSensitive />
          </ToggleButton>
          <ToggleButton label={t.findWholeWord} on={wholeWord} onClick={() => setWholeWord((on) => !on)}>
            <WholeWord />
          </ToggleButton>
          <ToggleButton label={t.findRegex} on={regex} onClick={() => setRegex((on) => !on)}>
            <Regex />
          </ToggleButton>
          <span aria-live="polite" className="min-w-12 px-1 text-center text-callout tabular-nums text-label-2">
            {findCount(query, results)}
          </span>
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
            {/* The words keep 12rem; the buttons wrap under them in a narrow pane. */}
            <div className="min-w-0 grow basis-48">
              {status.error && !isSerialPermissionError(status.error) ? (
                <ConnectFailureText error={status.error} />
              ) : (
                <>
                  <p className="text-callout font-medium text-label-1">
                    {status.error ? t.sessionFailed : exitTitle(status)}
                  </p>
                  <p className="selectable line-clamp-4 break-words text-callout text-label-2">
                    {status.error
                      ? t.serialPermissionDenied
                      : autoRetry === "gaveup"
                        ? t.autoReconnectGaveUp
                        : hostId
                          ? t.enterToReconnect
                          : t.enterToReopen}
                  </p>
                </>
              )}
              {autoRetry && autoRetry !== "gaveup" && <AutoReconnectCountdown plan={autoRetry} />}
            </div>
            <Button variant="primary" size="sm" onClick={() => startRef.current()}>
              <RotateCw />
              {autoRetry && autoRetry !== "gaveup" ? t.autoReconnectNow : hostId ? t.reconnect : t.reopen}
            </Button>
            {autoRetry && autoRetry !== "gaveup" && (
              <Button variant="secondary" size="sm" onClick={() => cancelAutoRef.current()}>
                {t.autoReconnectCancel}
              </Button>
            )}
            {(() => {
              const action = status.error ? failureAction(status.error, hostId) : null;
              return action && (
                <Button variant="secondary" size="sm" onClick={action.run}>
                  {action.label}
                </Button>
              );
            })()}
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

/** The button that fixes the stage: the host editor (for the key, address or port), or the vault dialog. */
function failureAction(error: string, hostId: string | null): { label: string; run: () => void } | null {
  const { stage } = failureText(error);
  if (stage === "vault") return { label: t.vaultUnlock, run: requestVaultUnlock };
  if (!hostId || stage === "cancelled") return null;
  return { label: stage === "auth" ? t.failChooseKey : t.editHost, run: () => requestHostEdit(hostId) };
}

/** The staged explanation of a failed connect, with the engine's own text behind a disclosure. */
function ConnectFailureText({ error }: { error: string }) {
  const { title, hint } = failureText(error);
  const [open, setOpen] = useState(false);
  return (
    <>
      <p className="text-callout font-medium text-label-1">{title}</p>
      {hint && <p className="selectable break-words text-callout text-label-2">{hint}</p>}
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="mt-1 inline-flex items-center gap-1 rounded-xs text-callout text-label-2 hover:text-label-1"
      >
        <ChevronDown className={cn("size-4 transition-transform", open && "rotate-180")} strokeWidth={1.5} />
        {open ? t.failHideDetails : t.failShowDetails}
      </button>
      {open && (
        <div className="mt-1 flex items-start gap-1">
          <pre className="selectable max-h-32 min-w-0 grow overflow-auto whitespace-pre-wrap break-words rounded-xs bg-fill-1 px-2 py-1 font-mono text-callout text-label-2">
            {error}
          </pre>
          <Tooltip label={t.failCopyDetails}>
            <Button variant="quiet" size="icon-sm" aria-label={t.failCopyDetails} onClick={() => void copyText(error).catch(console.error)}>
              <Copy />
            </Button>
          </Tooltip>
        </div>
      )}
    </>
  );
}

/** "Reconnecting in 4s (attempt 2)", counting down each second. */
function AutoReconnectCountdown({ plan }: { plan: RetryPlan }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [plan]);
  return (
    <p className="text-callout text-label-2" aria-live="polite">
      {t.autoReconnectIn.replace("{seconds}", String(secondsLeft(plan, now))).replace("{attempt}", String(plan.attempt))}
    </p>
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

/** "3/17" for the current match, the count alone when it is unknown (past the addon's limit), or "No results". */
function findCount(query: string, results: { index: number; count: number } | null): string {
  if (!query || !results) return "";
  if (results.count === 0) return t.findNoMatch;
  return results.index < 0 ? `${results.count}+` : `${results.index + 1}/${results.count}`;
}

function ToggleButton({
  label,
  on,
  onClick,
  children,
}: {
  label: string;
  on: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label}>
      <Button
        variant="quiet"
        size="icon-sm"
        aria-label={label}
        aria-pressed={on}
        onClick={onClick}
        className={cn(on && "bg-accent-soft text-accent hover:text-accent")}
      >
        {children}
      </Button>
    </Tooltip>
  );
}
