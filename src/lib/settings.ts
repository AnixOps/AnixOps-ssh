import { invoke } from "@tauri-apps/api/core";
import { isTauri, type SshEngine } from "./backend";
import { readMirror, writeMirror, type LocaleSetting, type ThemeSetting } from "./prefsMirror";

export interface Settings {
  theme: ThemeSetting;
  locale: LocaleSetting;
  terminalFontSize: number;
  terminalScrollback: number;
  /** macOS: Option sends Meta (Esc prefix) instead of typing layout characters. */
  terminalOptionAsMeta: boolean;
  /** The command "Open in editor" starts with the file appended; empty uses the system's default application. */
  editorCommand: string;
  /** The files pane shows the folder the terminal is in (needs shell integration). */
  followTerminalFolder: boolean;
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
  editorCommand: "",
  followTerminalFolder: false,
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
