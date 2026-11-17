/* App shortcuts (guidelines/keyboard.md). Reserved AnixOps shortcuts use Mod
   (Ctrl, or Cmd on macOS): Mod+K palette, Mod+/ shortcut list, Mod+, settings,
   Mod+1..4 pages, Mod+I import, Mod+N new host, Mod+F find.

   A terminal needs plain Ctrl keys (Ctrl+K, Ctrl+W, Ctrl+N mean something in
   bash and vim), so on Windows and Linux the reserved shortcuts work with plain
   Ctrl everywhere except while the terminal has the focus, where the same
   commands use Ctrl+Shift (K22: never Ctrl+Alt). Terminal commands (tabs,
   splits, broadcast) use Ctrl+Shift always. macOS has no such conflict and uses
   Cmd. The table below is the one place shortcuts are written down (K9): the
   shortcut list, tooltips, menus and the palette read it. */

import { t } from "./i18n";

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export type AppShortcut =
  | "search-hosts"
  | "new-local"
  | "close-tab"
  | "toggle-files"
  | "find"
  | "copy"
  | "paste"
  | "split-right"
  | "split-down"
  | "focus-left"
  | "focus-right"
  | "focus-up"
  | "focus-down"
  | "toggle-broadcast"
  | "toggle-maximise"
  | "prev-prompt"
  | "next-prompt"
  | "shortcut-list"
  | "settings"
  | "go-hosts"
  | "go-forwards"
  | "go-snippets"
  | "go-team"
  | "import"
  | "new-host";

const letters: Record<string, AppShortcut> = {
  k: "search-hosts",
  t: "new-local",
  w: "close-tab",
  e: "toggle-files",
  f: "find",
  c: "copy",
  v: "paste",
};

const arrows: Record<string, AppShortcut> = {
  ArrowLeft: "focus-left",
  ArrowRight: "focus-right",
  ArrowUp: "focus-up",
  ArrowDown: "focus-down",
};

const pages: Record<string, AppShortcut> = {
  Digit1: "go-hosts",
  Digit2: "go-forwards",
  Digit3: "go-snippets",
  Digit4: "go-team",
};

export interface KeyInput {
  key: string;
  code?: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** The terminal commands: Ctrl+Shift+key on Windows and Linux, Cmd+key on macOS.
 *
 *  Windows and Linux: Ctrl+Shift+key; "split down" is Ctrl+Shift+O (K22 rules out Ctrl+Alt).
 *  macOS: Cmd+key; Cmd+Shift+D splits down, Cmd+Option+Arrow moves focus,
 *  Cmd+Shift+B toggles broadcast. Prompt jumps: Ctrl+Shift+PageUp/PageDown, or
 *  Cmd+Up/Down on macOS. */
export function resolveShortcut(input: KeyInput, mac: boolean): AppShortcut | null {
  const { shiftKey, altKey } = input;
  const letter = input.key.toLowerCase();
  const isD = letter === "d" || input.code === "KeyD";
  if (mac) {
    if (!input.metaKey || input.ctrlKey) return null;
    if (arrows[input.key]) {
      if (altKey && !shiftKey) return arrows[input.key];
      // ⌘↑ / ⌘↓ jump between prompts (shell integration).
      if (!altKey && !shiftKey) return input.key === "ArrowUp" ? "prev-prompt" : input.key === "ArrowDown" ? "next-prompt" : null;
      return null;
    }
    if (altKey) return null;
    if (isD) return shiftKey ? "split-down" : "split-right";
    if (letter === "b") return shiftKey ? "toggle-broadcast" : null;
    if (input.key === "Enter") return shiftKey ? "toggle-maximise" : null;
    const shortcut = letters[letter] ?? null;
    // The native Cmd+C / Cmd+V already copy and paste in the terminal.
    return shortcut === "copy" || shortcut === "paste" ? null : shortcut;
  }
  if (!input.ctrlKey || !shiftKey || input.metaKey) return null;
  if (altKey) return null; // K22: Ctrl+Alt is AltGr on many layouts
  if (isD) return "split-right";
  if (letter === "o" || input.code === "KeyO") return "split-down";
  if (arrows[input.key]) return arrows[input.key];
  // Ctrl+Shift+Arrow already moves pane focus, so prompts use the page keys.
  if (input.key === "PageUp") return "prev-prompt";
  if (input.key === "PageDown") return "next-prompt";
  if (letter === "b") return "toggle-broadcast";
  if (input.key === "Enter") return "toggle-maximise";
  return letters[letter] ?? null;
}

/** The reserved app-wide shortcuts of keyboard.md section 3. Shortcuts follow the
 *  character on the key where there is one; digits and punctuation also match
 *  `code`, so they work on every layout (K8). */
export function resolveGlobalShortcut(input: KeyInput, mac: boolean, inTerminal: boolean): AppShortcut | null {
  const { shiftKey, altKey } = input;
  if (altKey) return null;
  const letter = input.key.toLowerCase();
  const slash = input.key === "/" || input.key === "?" || input.code === "Slash";
  const comma = input.key === "," || input.key === "<" || input.code === "Comma";
  const page = input.code ? pages[input.code] : undefined;
  if (mac) {
    if (!input.metaKey || input.ctrlKey || shiftKey) return null;
    if (slash) return "shortcut-list";
    if (comma) return "settings";
    if (page) return page;
    if (letter === "i") return "import";
    if (letter === "n") return "new-host";
    return null;
  }
  if (input.metaKey) return null;
  // Plain Ctrl outside the terminal, Ctrl+Shift inside it.
  if (!input.ctrlKey || shiftKey !== inTerminal) return null;
  if (letter === "k") return "search-hosts";
  if (slash) return "shortcut-list";
  if (comma) return "settings";
  if (page) return page;
  if (letter === "i") return "import";
  if (letter === "n") return "new-host";
  if (letter === "f") return "find";
  return null;
}

/** Whether the event comes from inside a terminal, where plain Ctrl belongs to the shell. */
export function inTerminal(event: Pick<Event, "target">): boolean {
  return event.target instanceof Element && event.target.closest(".xterm") !== null;
}

export function appShortcut(event: KeyboardEvent): AppShortcut | null {
  if (event.type !== "keydown") return null;
  return resolveShortcut(event, isMac) ?? resolveGlobalShortcut(event, isMac, inTerminal(event));
}

/** Display form for a letter shortcut (Ctrl+Shift+W or ⌘W). */
export function shortcutLabel(key: string): string {
  return isMac ? `⌘${key.toUpperCase()}` : `Ctrl+Shift+${key.toUpperCase()}`;
}

export type ShortcutId = AppShortcut | "focus-pane" | "rename" | "remove" | "row-menu" | "next-tab";

export interface ShortcutRow {
  id: ShortcutId;
  group: "app" | "terminal" | "list";
  title: () => string;
  pc: string;
  mac: string;
  /** Shown beside the shortcut when the terminal has the focus and it differs. */
  pcInTerminal?: string;
}

/** Every shortcut, once (K9). */
export const shortcutTable: ShortcutRow[] = [
  { id: "search-hosts", group: "app", title: () => t.palette, pc: "Ctrl+K", mac: "⌘K", pcInTerminal: "Ctrl+Shift+K" },
  { id: "shortcut-list", group: "app", title: () => t.shortcutList, pc: "Ctrl+/", mac: "⌘/", pcInTerminal: "Ctrl+Shift+/" },
  { id: "settings", group: "app", title: () => t.navSettings, pc: "Ctrl+,", mac: "⌘,", pcInTerminal: "Ctrl+Shift+," },
  { id: "go-hosts", group: "app", title: () => t.navHosts, pc: "Ctrl+1", mac: "⌘1", pcInTerminal: "Ctrl+Shift+1" },
  { id: "go-forwards", group: "app", title: () => t.navForwards, pc: "Ctrl+2", mac: "⌘2", pcInTerminal: "Ctrl+Shift+2" },
  { id: "go-snippets", group: "app", title: () => t.navSnippets, pc: "Ctrl+3", mac: "⌘3", pcInTerminal: "Ctrl+Shift+3" },
  { id: "go-team", group: "app", title: () => t.navTeam, pc: "Ctrl+4", mac: "⌘4", pcInTerminal: "Ctrl+Shift+4" },
  { id: "import", group: "app", title: () => t.importHosts, pc: "Ctrl+I", mac: "⌘I", pcInTerminal: "Ctrl+Shift+I" },
  { id: "new-host", group: "app", title: () => t.newHost, pc: "Ctrl+N", mac: "⌘N", pcInTerminal: "Ctrl+Shift+N" },
  { id: "find", group: "app", title: () => t.searchHosts, pc: "Ctrl+F", mac: "⌘F" },
  { id: "new-local", group: "terminal", title: () => t.newTab, pc: "Ctrl+Shift+T", mac: "⌘T" },
  { id: "close-tab", group: "terminal", title: () => t.closeTab, pc: "Ctrl+Shift+W", mac: "⌘W" },
  { id: "toggle-files", group: "terminal", title: () => t.toggleFiles, pc: "Ctrl+Shift+E", mac: "⌘E" },
  { id: "find", group: "terminal", title: () => t.find, pc: "Ctrl+Shift+F", mac: "⌘F" },
  { id: "copy", group: "terminal", title: () => t.copy, pc: "Ctrl+Shift+C", mac: "⌘C" },
  { id: "paste", group: "terminal", title: () => t.paste, pc: "Ctrl+Shift+V", mac: "⌘V" },
  { id: "split-right", group: "terminal", title: () => t.splitRight, pc: "Ctrl+Shift+D", mac: "⌘D" },
  { id: "split-down", group: "terminal", title: () => t.splitDown, pc: "Ctrl+Shift+O", mac: "⌘⇧D" },
  { id: "focus-pane", group: "terminal", title: () => t.focusPane, pc: "Ctrl+Shift+←↑↓→", mac: "⌘⌥←↑↓→" },
  { id: "toggle-broadcast", group: "terminal", title: () => t.broadcastTab, pc: "Ctrl+Shift+B", mac: "⌘⇧B" },
  { id: "toggle-maximise", group: "terminal", title: () => t.maximisePane, pc: "Ctrl+Shift+Enter", mac: "⌘⇧↵" },
  { id: "prev-prompt", group: "terminal", title: () => t.promptPrevious, pc: "Ctrl+Shift+PageUp", mac: "⌘↑" },
  { id: "next-prompt", group: "terminal", title: () => t.promptNext, pc: "Ctrl+Shift+PageDown", mac: "⌘↓" },
  { id: "rename", group: "list", title: () => t.editHost, pc: "F2", mac: "⌘E" },
  { id: "remove", group: "list", title: () => t.delete, pc: "Delete", mac: "⌘⌫" },
  { id: "row-menu", group: "list", title: () => t.more, pc: "Shift+F10", mac: "⌃↵" },
];

/** The shortcut for a command, as the platform writes it; "" when there is none. */
export function shortcutHint(id: ShortcutId): string {
  const row = shortcutTable.find((candidate) => candidate.id === id);
  return row ? (isMac ? row.mac : row.pc) : "";
}
