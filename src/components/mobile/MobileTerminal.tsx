import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Activity, ChevronLeft, ChevronsDown, FolderOpen, ChevronsUp, Keyboard, KeyboardOff, RotateCw, SlidersHorizontal, Unplug } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import { MobileMonitor } from "@/components/mobile/MobileMonitor";
import { MobileKeyBar, type KeyBarActions, type ModifierName } from "@/components/mobile/MobileKeyBar";
import { SelectionOverlay } from "@/components/mobile/SelectionOverlay";
import { attachTerminalTouch, type TouchController, type TouchOverlay } from "@/components/mobile/terminalTouch";
import { Button } from "@/components/ui/button";
import { openSession, isTauri, type Host, type SessionEvent, type SessionHandle } from "@/lib/backend";
import { snippetBytes } from "@/lib/barLayout";
import { binaryInput } from "@/lib/terminalBytes";
import { BackgroundPolicy, resumeAction } from "@/lib/backgroundPolicy";
import { watchLifecycle } from "@/lib/mobileVault";
import { EnvBadge } from "@/components/app/EnvBadge";
import { t } from "@/lib/i18n";
import { attachImeLayer, type ImeLayer } from "@/lib/imeLayer";
import {
  consumeModifier,
  keySequence,
  modifyTyped,
  MODIFIER_OFF,
  tapModifier,
  type BarKey,
  type ModifierState,
} from "@/lib/mobileKeys";
import { dynamicTypeScale, haptic, hostAttach, isCjkLanguage, updateMobilePrefs, useMobilePrefs } from "@/lib/mobilePrefs";
import { onSettings, settings } from "@/lib/settings";
import { capabilityReply } from "@/lib/terminalInput";
import { disposeTerminal, liveTerminalOptions, terminalOptions, UNICODE_VERSION } from "@/lib/terminalOptions";
import { terminalFont, terminalTheme, useTerminalScheme } from "@/lib/terminalTheme";

type Status =
  | { state: "connecting" }
  | { state: "connected" }
  | { state: "reconnecting"; attempt: number }
  /** Closed on purpose when the grace period in the background ran out. */
  | { state: "paused" }
  | { state: "exited"; error?: string };

/** What the app level (hardware keyboard shortcuts) can ask of the terminal. */
export interface TerminalController {
  copy: () => void;
  paste: () => void;
  /** Ends the session and leaves the screen. */
  close: () => void;
}

const TOOL_NAMES = { tmux: "tmux", screen: "screen" } as const;

/** One terminal on one host, in-process (russh, no daemon), with the key bar.
 *
 *  Typing goes through lib/imeLayer.ts (composition, predictive text,
 *  Backspace on an empty field), touch through terminalTouch.ts (tap, scroll,
 *  select, pinch), the key bar through lib/mobileKeys.ts. When the app goes to
 *  the background lib/backgroundPolicy.ts decides how long the connection
 *  lives; hosts that keep their session in tmux or screen reconnect into it.
 *  Leaving the screen ends the session: a phone has no background process to
 *  keep it (docs/architecture-v2.md section 2). */
export function MobileTerminal({
  host,
  onBack,
  onFiles,
  onOpenSettings,
  controllerRef,
  keyboardOpen,
}: {
  host: Host;
  onBack: () => void;
  /** Opens the file browser on this host over the terminal. */
  onFiles?: () => void;
  onOpenSettings: () => void;
  controllerRef: MutableRefObject<TerminalController | null>;
  keyboardOpen: boolean;
}) {
  const prefs = useMobilePrefs();
  const prefsRef = useRef(prefs);
  prefsRef.current = prefs;

  const wrapperRef = useRef<HTMLDivElement>(null);
  useTerminalScheme(wrapperRef);
  const surfaceRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const sessionRef = useRef<SessionHandle | null>(null);
  const touchRef = useRef<TouchController | null>(null);
  const imeRef = useRef<ImeLayer | null>(null);
  const policyRef = useRef<BackgroundPolicy | null>(null);
  const startRef = useRef<(reconnected: boolean) => void>(() => {});
  const generation = useRef(0);
  const statusRef = useRef<Status>({ state: "connecting" });
  const scaleRef = useRef(1);
  /** Everything written to the session, kept only in the browser preview so tests can read it. */
  const writesRef = useRef<string[] | null>(null);
  const sendRaw = useCallback((data: string) => {
    writesRef.current?.push(data);
    sessionRef.current?.write(data);
  }, []);

  const ctrlRef = useRef<ModifierState>(MODIFIER_OFF);
  const altRef = useRef<ModifierState>(MODIFIER_OFF);
  const [ctrl, setCtrlState] = useState<ModifierState>(MODIFIER_OFF);
  const [alt, setAltState] = useState<ModifierState>(MODIFIER_OFF);
  const [status, setStatusState] = useState<Status>({ state: "connecting" });
  const [overlay, setOverlay] = useState<TouchOverlay>({ selection: null, pastePoint: null });
  const [, redraw] = useState(0);
  const [preedit, setPreedit] = useState("");
  const [notice, setNotice] = useState("");
  const [monitorOpen, setMonitorOpen] = useState(false);

  const setStatus = useCallback((next: Status | ((current: Status) => Status)) => {
    const value = typeof next === "function" ? next(statusRef.current) : next;
    statusRef.current = value;
    setStatusState(value);
  }, []);
  const setCtrl = useCallback((next: ModifierState) => {
    ctrlRef.current = next;
    setCtrlState(next);
  }, []);
  const setAlt = useCallback((next: ModifierState) => {
    altRef.current = next;
    setAltState(next);
  }, []);

  const attach = hostAttach(host.id);

  useEffect(() => {
    const surface = surfaceRef.current;
    if (!surface) return;
    let disposed = false;
    const scale = dynamicTypeScale();
    scaleRef.current = scale;
    const effectiveSize = () => Math.round(prefsRef.current.fontSize * scale);

    const terminal = new Terminal(
      terminalOptions(settings(), {
        fontFamily: terminalFont(),
        fontSize: effectiveSize(),
        screenReaderMode: prefsRef.current.screenReader,
        theme: terminalTheme(),
      }),
    );
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.loadAddon(new Unicode11Addon());
    terminal.unicode.activeVersion = UNICODE_VERSION;
    terminal.open(surface);
    terminalRef.current = terminal;
    fitRef.current = fit;
    fit.fit();

    const modifiers = () => ({ ctrl: ctrlRef.current.mode !== "off", alt: altRef.current.mode !== "off" });
    const consume = () => {
      setCtrl(consumeModifier(ctrlRef.current));
      setAlt(consumeModifier(altRef.current));
    };
    if (!isTauri) writesRef.current = [];
    const send = sendRaw;
    /** Typed text (hardware keys, the soft keyboard, a key-bar symbol) under the armed modifiers. */
    const typed = (data: string) => {
      touchRef.current?.clear();
      send(modifyTyped(data, modifiers()) ?? data);
      if ([...data].length === 1) consume();
    };

    // Typing from the soft keyboard: the IME layer, not xterm.js's own handling.
    const textarea = terminal.textarea;
    if (textarea) {
      textarea.setAttribute("autocapitalize", "off");
      textarea.setAttribute("autocorrect", "off");
      textarea.setAttribute("autocomplete", "off");
      textarea.setAttribute("spellcheck", "false");
      textarea.setAttribute("enterkeyhint", "enter");
      imeRef.current = attachImeLayer(textarea, surface, {
        mode: prefsRef.current.imeMode,
        cjk: isCjkLanguage(),
        send: typed,
        onPreedit: setPreedit,
      });
    }
    terminal.onData(typed);
    // Mouse reports in the original encoding come as binary, not as typed text.
    terminal.onBinary((data) => send(binaryInput(data)));
    // XTGETTCAP: Neovim and others ask whether 24-bit colour and undercurl work.
    const capabilities = terminal.parser.registerDcsHandler({ intermediates: "+", final: "q" }, (data) => {
      sendRaw(capabilityReply(data));
      return true;
    });
    // OSC 8 hyperlinks are not opened from a tap; without this xterm.js would
    // ask with a browser dialog.
    terminal.options.linkHandler = { activate: () => {} };
    terminal.onResize(({ cols, rows }) => sessionRef.current?.resize(cols, rows));
    const rerender = () => redraw((count) => count + 1);
    terminal.onScroll(rerender);
    terminal.onResize(rerender);
    terminal.onSelectionChange(rerender);

    let noticeTimer: number | null = null;
    const announce = (text: string) => {
      setNotice(text);
      if (noticeTimer !== null) window.clearTimeout(noticeTimer);
      noticeTimer = window.setTimeout(() => setNotice(""), 2000);
    };

    const touch = attachTerminalTouch({
      terminal,
      surface,
      fontSize: () => terminal.options.fontSize ?? effectiveSize(),
      setFontSize: (size, persist) => {
        terminal.options.fontSize = size;
        fit.fit();
        rerender();
        if (persist) updateMobilePrefs({ fontSize: Math.round(size / scale) });
      },
      sendKeys: send,
      onTap: () => {
        // A keyboard that was dismissed with Back still has the focus; blur
        // first so focusing again shows it.
        if (document.activeElement === terminal.textarea && !keyboardVisible()) terminal.textarea?.blur();
        terminal.focus();
      },
      onOverlay: setOverlay,
      onNotice: announce,
      haptic: () => haptic(prefsRef.current),
      strings: { copied: t.mobileCopied, clipboardEmpty: t.mobileClipboardEmpty },
    });
    touchRef.current = touch;
    controllerRef.current = {
      copy: () => void touch.copy(),
      paste: () => void touch.paste(),
      close: () => {
        sessionRef.current?.close();
        onBackRef.current();
      },
    };

    // The session.
    const start = (reconnected: boolean) => {
      const mine = ++generation.current;
      setStatus({ state: "connecting" });
      let exited = false;
      const live = () => !disposed && mine === generation.current;
      const onEvent = (event: SessionEvent) => {
        if (!live()) return;
        if (event.kind === "data") {
          terminal.write(event.data);
          setStatus((current) => (current.state === "connecting" || current.state === "reconnecting" ? { state: "connected" } : current));
        } else if (event.kind === "exit") {
          exited = true;
          sessionRef.current = null;
          if (statusRef.current.state !== "paused") setStatus({ state: "exited" });
        } else if (event.kind === "reconnecting") {
          setStatus({ state: "reconnecting", attempt: event.attempt ?? 1 });
        } else if (event.kind === "resumed") {
          divider();
          setStatus({ state: "connected" });
        }
      };
      const divider = () => {
        const time = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
        terminal.write(`\r\n\x1b[2m── ${t.mobileDividerReconnected} ${time} ──\x1b[0m\r\n`);
      };
      if (reconnected) divider();
      const chosen = hostAttach(host.id);
      openSession(host.id, terminal.cols, terminal.rows, onEvent, {
        attach: chosen === "off" ? undefined : chosen,
        keepaliveSecs: prefsRef.current.keepaliveSeconds,
      })
        .then((session) => {
          if (!live() || exited) {
            if (!exited) session.close();
            return;
          }
          sessionRef.current = session;
          session.resize(terminal.cols, terminal.rows);
        })
        .catch((reason) => {
          if (live()) setStatus({ state: "exited", error: String(reason) });
        });
    };
    startRef.current = (reconnected) => {
      sessionRef.current?.close();
      sessionRef.current = null;
      start(reconnected);
      terminal.focus();
    };
    start(false);
    terminal.focus();

    // The background policy.
    const policy = new BackgroundPolicy(prefsRef.current.graceSeconds, {
      onExpire: () => {
        const current = statusRef.current.state;
        if (!sessionRef.current || (current !== "connected" && current !== "connecting" && current !== "reconnecting")) return;
        // Close cleanly rather than leave a half-open socket behind.
        setStatus({ state: "paused" });
        sessionRef.current.close();
        sessionRef.current = null;
        generation.current++;
      },
      onResume: (info) => {
        const current = statusRef.current.state;
        const alive = sessionRef.current !== null && current !== "exited" && current !== "paused";
        const attached = hostAttach(host.id) !== "off";
        const action = resumeAction(info, attached, alive);
        if (action === "reconnect") {
          if (sessionRef.current?.reconnect && alive) {
            setStatus({ state: "reconnecting", attempt: 1 });
            sessionRef.current.reconnect();
          } else startRef.current(true);
        }
        // "offer": the card is already showing (paused or ended).
      },
    });
    policyRef.current = policy;
    // The same lifecycle source as the vault auto-lock (lib/mobileVault.ts): one
    // definition of "the app left" for visibilitychange, pagehide and pageshow.
    const stopLifecycle = watchLifecycle(
      document,
      {
        background: async () => policy.hidden(),
        foreground: async () => policy.visible(),
      },
      () => {},
      window,
    );

    const observer = new ResizeObserver(() => {
      if (surface.clientWidth > 0 && surface.clientHeight > 0) {
        fit.fit();
        rerender();
      }
    });
    observer.observe(surface);
    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    const updateTheme = () => (terminal.options.theme = terminalTheme());
    scheme.addEventListener("change", updateTheme);
    const stopSettings = onSettings((next) => {
      Object.assign(terminal.options, liveTerminalOptions(next, terminalFont()));
      updateTheme();
    });

    if (writesRef.current) {
      (window as unknown as { __mobileDebug?: unknown }).__mobileDebug = {
        terminal,
        writes: writesRef.current,
        status: () => statusRef.current,
      };
    }

    return () => {
      disposed = true;
      stopLifecycle();
      policy.dispose();
      policyRef.current = null;
      scheme.removeEventListener("change", updateTheme);
      stopSettings();
      observer.disconnect();
      if (noticeTimer !== null) window.clearTimeout(noticeTimer);
      touch.dispose();
      capabilities.dispose();
      touchRef.current = null;
      imeRef.current?.dispose();
      imeRef.current = null;
      controllerRef.current = null;
      sessionRef.current?.close();
      sessionRef.current = null;
      disposeTerminal(terminal);
      terminalRef.current = null;
      fitRef.current = null;
    };
    // The terminal is built once per host; preferences are read through refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host.id]);

  const onBackRef = useRef(onBack);
  onBackRef.current = onBack;

  // Preferences that change while the terminal is open.
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    const size = Math.round(prefs.fontSize * scaleRef.current);
    if (terminal.options.fontSize !== size) {
      terminal.options.fontSize = size;
      fitRef.current?.fit();
    }
    terminal.options.screenReaderMode = prefs.screenReader;
    imeRef.current?.configure(prefs.imeMode, isCjkLanguage());
    policyRef.current?.setGrace(prefs.graceSeconds);
  }, [prefs.fontSize, prefs.screenReader, prefs.imeMode, prefs.graceSeconds]);

  const press = (key: BarKey) => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    haptic(prefsRef.current);
    const data = keySequence(key, terminal.modes.applicationCursorKeysMode, {
      ctrl: ctrlRef.current.mode !== "off",
      alt: altRef.current.mode !== "off",
    });
    touchRef.current?.clear();
    sendRaw(data);
    setCtrl(consumeModifier(ctrlRef.current));
    setAlt(consumeModifier(altRef.current));
  };

  const actions: KeyBarActions = {
    key: press,
    char: (text) => {
      haptic(prefsRef.current);
      const terminal = terminalRef.current;
      // A symbol typed from the bar is typed text: the armed modifiers apply.
      const data = modifyTyped(text, {
        ctrl: ctrlRef.current.mode !== "off",
        alt: altRef.current.mode !== "off",
      });
      sendRaw(data ?? text);
      setCtrl(consumeModifier(ctrlRef.current));
      setAlt(consumeModifier(altRef.current));
      terminal?.focus();
    },
    snippet: (snippet) => {
      haptic(prefsRef.current);
      sendRaw(snippetBytes(snippet));
    },
    paste: () => {
      haptic(prefsRef.current);
      void touchRef.current?.paste();
    },
    modifier: (name: ModifierName) => {
      haptic(prefsRef.current);
      const now = performance.now();
      if (name === "ctrl") setCtrl(tapModifier(ctrlRef.current, now));
      else setAlt(tapModifier(altRef.current, now));
    },
  };

  const toggleKeyboard = () => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    if (keyboardOpen || document.activeElement === terminal.textarea) terminal.textarea?.blur();
    else terminal.focus();
  };

  const statusText =
    status.state === "connecting"
      ? t.connecting
      : status.state === "connected"
        ? t.connected
        : status.state === "reconnecting"
          ? `${t.mobileStatusReconnecting} ${status.attempt}`
          : status.state === "paused"
            ? t.mobileStatusPaused
            : t.sessionEnded;

  const tool = attach === "off" ? null : TOOL_NAMES[attach];
  const card = status.state === "exited" || status.state === "paused";

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col bg-bg">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="flex h-14 items-center gap-1 pr-safe pl-1">
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileBack} onClick={onBack} className="md:hidden">
            <ChevronLeft />
          </Button>
          <div className="min-w-0 flex-1 pl-2 md:pl-4">
            <div className="flex items-center gap-1.5">
              <h2 className="min-w-0 truncate text-body font-medium">{host.name}</h2>
              <EnvBadge environment={host.environment} />
            </div>
            <p role="status" className="truncate text-callout text-label-2">
              {statusText} · {host.target}
            </p>
          </div>
          {onFiles && (
            <Button variant="quiet" size="icon-lg" aria-label={t.files} onClick={onFiles}>
              <FolderOpen />
            </Button>
          )}
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={t.mobileMonitor}
            aria-pressed={monitorOpen}
            onClick={() => setMonitorOpen((on) => !on)}
          >
            <Activity />
          </Button>
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={prefs.extraRow ? t.mobileMoreKeysHide : t.mobileMoreKeys}
            aria-pressed={prefs.extraRow}
            onClick={() => updateMobilePrefs({ extraRow: !prefs.extraRow })}
          >
            {prefs.extraRow ? <ChevronsDown /> : <ChevronsUp />}
          </Button>
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={keyboardOpen ? t.mobileKeyboardHide : t.mobileKeyboardShow}
            onClick={toggleKeyboard}
          >
            {keyboardOpen ? <KeyboardOff /> : <Keyboard />}
          </Button>
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileTerminalMenu} onClick={onOpenSettings}>
            <SlidersHorizontal />
          </Button>
        </div>
      </header>

      <div ref={wrapperRef} role="group" aria-label={t.mobileTerminal} className="relative min-h-0 flex-1 bg-bg">
        <div ref={surfaceRef} data-terminal-surface className="touch-surface absolute inset-x-3 top-2 bottom-1 isolate" />
        {terminalRef.current && (
          <SelectionOverlay
            terminal={terminalRef.current}
            surface={surfaceRef.current}
            wrapper={wrapperRef.current}
            overlay={overlay}
            controller={touchRef.current}
          />
        )}
        {preedit && (
          <p
            role="status"
            aria-label={t.mobilePreedit}
            data-preedit
            className="absolute bottom-2 left-3 max-w-[calc(100%-1.5rem)] truncate rounded-md bg-bg-elevated px-3 py-1 text-body text-label-1 shadow-2 ring-1 ring-separator"
          >
            {preedit}
          </p>
        )}
        <p role="status" aria-live="polite" className="sr-only">
          {notice}
        </p>
        {notice && (
          <p
            aria-hidden
            className="absolute top-2 left-1/2 -translate-x-1/2 rounded-pill bg-bg-elevated px-4 py-1 text-callout shadow-2 ring-1 ring-separator"
          >
            {notice}
          </p>
        )}
        {monitorOpen && (
          <MobileMonitor hostId={host.id} connected={status.state === "connected"} onClose={() => setMonitorOpen(false)} />
        )}
        {card && (
          <div className="absolute inset-x-0 bottom-3 flex justify-center px-4">
            <div role="alert" className="flex w-full max-w-xl flex-col gap-3 rounded-md bg-bg-elevated px-4 py-3 shadow-3">
              <div className="flex items-start gap-3">
                <Unplug className="mt-0.5 size-5 shrink-0 text-label-2" strokeWidth={1.5} />
                <div className="min-w-0 flex-1">
                  <p className="text-callout font-medium text-label-1">
                    {status.state === "paused" ? t.mobileStatusPaused : status.state === "exited" && status.error ? t.sessionFailed : t.sessionEnded}
                  </p>
                  {status.state === "paused" && <p className="text-callout text-label-2">{t.mobileClosedInBackground}</p>}
                  {status.state === "exited" && status.error && (
                    <p className="selectable line-clamp-4 break-words text-callout text-label-2">{status.error}</p>
                  )}
                  <p className="text-callout text-label-2">
                    {tool ? t.mobileWorkKept.replace("{tool}", tool) : t.mobileNewShell}
                  </p>
                </div>
              </div>
              <Button variant="primary" size="lg" onClick={() => startRef.current(true)}>
                <RotateCw />
                {t.reconnect}
              </Button>
            </div>
          </div>
        )}
      </div>

      <MobileKeyBar layout={prefs.bar} showExtra={prefs.extraRow} ctrl={ctrl} alt={alt} actions={actions} />
    </div>
  );
}

/** Whether the soft keyboard is up, from the visual viewport. */
function keyboardVisible(): boolean {
  return document.documentElement.dataset.keyboard === "open";
}
