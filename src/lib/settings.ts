import { invoke } from "@tauri-apps/api/core";
import { isTauri, type SshEngine } from "./backend";
import { readMirror, writeMirror, type LocaleSetting, type ThemeSetting } from "./prefsMirror";

export type CursorStyle = "block" | "bar" | "underline";
/** What a right-click in the terminal does: a menu, or paste (PuTTY style). */
export type TerminalRightClick = "menu" | "paste";
/** The terminal's colours: dark (default, whatever the app theme), light, or the app's. */
export type TerminalScheme = "dark" | "light" | "app";

export interface Settings {
  theme: ThemeSetting;
  locale: LocaleSetting;
  terminalFontSize: number;
  terminalScrollback: number;
  /** macOS: Option sends Meta (Esc prefix) instead of typing layout characters. */
  terminalOptionAsMeta: boolean;
  /** A font name or CSS font list; empty uses the built-in monospace stack. */
  terminalFontFamily: string;
  /** Programming ligatures (=> != ===) of fonts that have them, such as
   *  Fira Code, Cascadia Code or JetBrains Mono. Uses the DOM renderer. */
  terminalLigatures: boolean;
  /** 1.0 to 1.6. */
  terminalLineHeight: number;
  terminalCursorStyle: CursorStyle;
  terminalCursorBlink: boolean;
  /** Ask before a multi-line paste into a shell without bracketed paste. */
  terminalWarnMultilinePaste: boolean;
  terminalRightClick: TerminalRightClick;
  terminalScheme: TerminalScheme;
  /** Selecting text copies it. */
  terminalCopyOnSelect: boolean;
  /** Windows Terminal keys: Ctrl+C copies a selection (and interrupts without
   *  one), Ctrl+V pastes. Not offered on macOS, where ⌘C / ⌘V do this. */
  terminalCtrlCopyPaste: boolean;
  /** Remote programs may write the clipboard with OSC 52 (never read it). */
  terminalOsc52: boolean;
  /** Full-screen programs (btop, vim, tmux) may take mouse clicks and the wheel. */
  terminalMouseReporting: boolean;
  /** Reconnect by itself, with backoff, when a connection drops. */
  terminalAutoReconnect: boolean;
  /** The command "Open in editor" starts with the file appended; empty uses the system's default application. */
  editorCommand: string;
  /** The files pane shows the folder the terminal is in (needs shell integration). */
  followTerminalFolder: boolean;
  /** A double-click on a file in the files pane: open it in the editor (FinalShell, WinSCP) or download it. */
  filesDoubleClick: "edit" | "download";
  /** Compare SHA-256 of a file here and on the server after each transfer. */
  filesVerifyTransfers: boolean;
  /** A single click on a host connects; off, a click selects and a double-click or Enter connects. */
  hostsSingleClick: boolean;
  /** The SSH engine for hosts that follow the global setting: the built-in
   * engine unless OpenSSH was chosen (an advanced option kept for compatibility). */
  sshEngine: SshEngine;
  /** null until the first-run question has been answered. */
  crashReports: boolean | null;
  usageStats: boolean;
}

const mirror = readMirror();
let current: Settings = {
  ...mirror,
  terminalFontSize: 13,
  terminalScrollback: 10000,
  terminalOptionAsMeta: false,
  terminalFontFamily: "",
  terminalLigatures: false,
  terminalLineHeight: 1.2,
  terminalCursorStyle: "block",
  terminalCursorBlink: true,
  terminalWarnMultilinePaste: true,
  terminalRightClick: "menu",
  terminalScheme: "dark",
  terminalCopyOnSelect: false,
  terminalCtrlCopyPaste: typeof navigator !== "undefined" && /Win/.test(navigator.platform),
  terminalOsc52: true,
  terminalMouseReporting: true,
  terminalAutoReconnect: true,
  editorCommand: "",
  followTerminalFolder: false,
  filesDoubleClick: "edit",
  filesVerifyTransfers: false,
  hostsSingleClick: false,
  sshEngine: "russh",
  crashReports: null,
  usageStats: false,
};
const listeners = new Set<(settings: Settings) => void>();

/** `data-theme` overrides the system choice; none follows the system. */
function applyTheme(theme: ThemeSetting) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}
applyTheme(current.theme);

function publish(next: Settings) {
  const localeChanged = next.locale !== readMirror().locale;
  current = next;
  writeMirror({ theme: next.theme, locale: next.locale });
  applyTheme(next.theme);
  for (const listener of listeners) listener(next);
  // Strings are chosen at start-up, so a new language needs a reload.
  if (localeChanged) window.location.reload();
}

export function settings(): Settings {
  return current;
}

export function onSettings(listener: (settings: Settings) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function loadSettings(): Promise<void> {
  if (!isTauri) return;
  publish(await invoke<Settings>("settings_get"));
}

export async function updateSettings(patch: Partial<Settings>): Promise<void> {
  const next = { ...current, ...patch };
  if (isTauri) await invoke("settings_save", { settings: next });
  publish(next);
}
