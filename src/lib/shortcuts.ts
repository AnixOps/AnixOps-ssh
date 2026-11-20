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
  | "new-host"
  | "clear-terminal"
  | "zoom-in"
  | "zoom-out"
  | "zoom-reset";

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

/** Zoom keys, matched by physical key as well so they work on every layout.
 *  Cmd/Ctrl+Plus, Cmd/Ctrl+Minus, Cmd/Ctrl+0 (Shift is allowed because Plus is Shift+= on US keyboards). */
function zoomShortcut(input: KeyInput): AppShortcut | null {
  if (input.key === "+" || input.key === "=" || input.code === "Equal" || input.code === "NumpadAdd") return "zoom-in";
  if (input.key === "-" || input.key === "_" || input.code === "Minus" || input.code === "NumpadSubtract") return "zoom-out";
  if (input.key === "0" || input.key === ")" || input.code === "Digit0" || input.code === "Numpad0") return "zoom-reset";
  return null;
}

/** The terminal commands: Ctrl+Shift+key on Windows and Linux, Cmd+key on macOS.
 *
 *  Windows and Linux: Ctrl+Shift+key; "split down" is Ctrl+Shift+O (K22 rules out Ctrl+Alt).
 *  macOS: Cmd+key; Cmd+Shift+D splits down, Cmd+Option+Arrow moves focus,
 *  Cmd+Shift+B toggles broadcast. Prompt jumps: Ctrl+Shift+PageUp/PageDown, or
 *  Cmd+Up/Down on macOS. */
export function resolveShortcut(input: KeyInput, mac: boolean, terminalFocus = false): AppShortcut | null {
  const { shiftKey, altKey } = input;
  const letter = input.key.toLowerCase();
  const zoom = zoomShortcut(input);
  const isD = letter === "d" || input.code === "KeyD";
  if (mac) {
    if (!input.metaKey || input.ctrlKey) return null;
    if (arrows[input.key]) {
      if (altKey && !shiftKey) return arrows[input.key];
      // ⌘↑ / ⌘↓ jump between prompts (shell integration).
      if (!altKey && !shiftKey) return input.key === "ArrowUp" ? "prev-prompt" : input.key === "ArrowDown" ? "next-prompt" : null;
      return null;
    }
    if (zoom) return zoom;
    if (altKey) return null;
    // As in iTerm2 and Terminal.app, Cmd+K clears the terminal that has the
    // focus; the palette stays on Cmd+K elsewhere and on Cmd+Shift+K / Cmd+Shift+P.
    if (letter === "k" && terminalFocus && !shiftKey) return "clear-terminal";
    if (letter === "p" && shiftKey) return "search-hosts";
    if (isD) return shiftKey ? "split-down" : "split-right";
    if (letter === "b") return shiftKey ? "toggle-broadcast" : null;
    if (input.key === "Enter") return shiftKey ? "toggle-maximise" : null;
    const shortcut = letters[letter] ?? null;
    // The native Cmd+C / Cmd+V already copy and paste in the terminal.
    return shortcut === "copy" || shortcut === "paste" ? null : shortcut;
  }
  if (!input.ctrlKey || !shiftKey || input.metaKey) return null;
  if (altKey) return null; // K22: Ctrl+Alt is AltGr on many layouts
  if (zoom) return zoom;
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
  return resolveShortcut(event, isMac, inTerminal(event)) ?? resolveGlobalShortcut(event, isMac, inTerminal(event));
}

/** Display form for a letter shortcut (Ctrl+Shift+W or ⌘W). */
export function shortcutLabel(key: string): string {
  return isMac ? `⌘${key.toUpperCase()}` : `Ctrl+Shift+${key.toUpperCase()}`;
}
