import { invoke } from "@tauri-apps/api/core";
import { isTauri, type SshEngine } from "./backend";
import { defaultTerminalFont, type TerminalFontSettings } from "./terminalFont";
import { readMirror, writeMirror, type LocaleSetting, type ThemeSetting } from "./prefsMirror";

/** What a right-click in the terminal does: a menu, or paste (PuTTY style). */
export type TerminalRightClick = "menu" | "paste";

export interface Settings {
  theme: ThemeSetting;
  locale: LocaleSetting;
  terminalFontSize: number;
  terminalScrollback: number;
  /** macOS: Option sends Meta (Esc prefix) instead of typing layout characters. */
  terminalOptionAsMeta: boolean;
  /** Ask before a multi-line paste into a shell without bracketed paste. */
  terminalWarnMultilinePaste: boolean;
  terminalRightClick: TerminalRightClick;
  /** Selecting text copies it. */
  terminalCopyOnSelect: boolean;
  /** Remote programs may write the clipboard with OSC 52 (never read it). */
  terminalOsc52: boolean;
  /** Font family, line height and letter spacing (the size is `terminalFontSize`). */
  terminalFont: TerminalFontSettings;
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
  terminalWarnMultilinePaste: true,
  terminalRightClick: "menu",
  terminalCopyOnSelect: false,
  terminalOsc52: true,
  terminalFont: defaultTerminalFont,
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
