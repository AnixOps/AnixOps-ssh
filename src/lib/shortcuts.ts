/* App shortcuts (guidelines/keyboard.md). Reserved AnixOps shortcuts use Mod
   (Ctrl, or Cmd on macOS): Mod+K palette, Mod+/ shortcut list, Mod+, settings,
   Mod+1..4 pages, Mod+I import, Mod+N new host, Mod+F find.

   A terminal needs plain Ctrl keys (Ctrl+K, Ctrl+W, Ctrl+N mean something in
   bash and vim), so on Windows and Linux the reserved shortcuts work with plain
   Ctrl everywhere except while the terminal has the focus, where the same
   commands use Ctrl+Shift (K22: never Ctrl+Alt). The Ctrl+Shift form works
   everywhere and is the one shown, so a hint is right wherever the focus is. Terminal commands (tabs,
   splits, broadcast) use Ctrl+Shift always. macOS has no such conflict and uses
   Cmd, except that ⌘K clears the screen and scrollback inside a terminal (the
   palette is ⌘⇧K there). The table below is the one place shortcuts are written down (K9): the
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
  | "clear-terminal"
  | "shortcut-list"
  | "settings"
  | "go-hosts"
  | "go-forwards"
  | "go-snippets"
  | "go-team"
  | "import"
  | "new-host"
  | "next-tab"
  | "prev-tab"
  | "zoom-in"
  | "zoom-out"
  | "zoom-reset"
  | "toggle-sidebar"
  | "toggle-focus";

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
  const tab = tabShortcut(input, mac);
  if (tab) return tab;
  const zoom = zoomShortcut(input, mac);
  if (zoom) return zoom;
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
    // Plain ⌘K is the terminal's (clear) or the palette's; resolveGlobalShortcut decides.
    if (letter === "k" && !shiftKey) return null;
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

/** Next and previous tab: Ctrl+Tab and Ctrl+Shift+Tab on every system
 *  (keyboard.md; Windows Terminal, iTerm2 and browsers agree), and ⌘⇧] / ⌘⇧[
 *  on macOS as in iTerm2 and Terminal. A shell has no use for Ctrl+Tab. */
function tabShortcut(input: KeyInput, mac: boolean): AppShortcut | null {
  if (input.altKey) return null;
  if (input.key === "Tab" && input.ctrlKey && !input.metaKey) return input.shiftKey ? "prev-tab" : "next-tab";
  if (mac && input.metaKey && input.shiftKey && !input.ctrlKey) {
    if (input.code === "BracketRight" || input.key === "}") return "next-tab";
    if (input.code === "BracketLeft" || input.key === "{") return "prev-tab";
  }
  return null;
}

/** Terminal font zoom, as in Windows Terminal, WezTerm, iTerm2 and VS Code:
 *  Ctrl+= (or Ctrl++), Ctrl+- and Ctrl+0; ⌘ on macOS. Ctrl+Shift+- stays the
 *  shell's (readline undo is Ctrl+_). */
function zoomShortcut(input: KeyInput, mac: boolean): AppShortcut | null {
  if (input.altKey) return null;
  if (mac ? !input.metaKey || input.ctrlKey : !input.ctrlKey || input.metaKey) return null;
  const code = input.code ?? "";
  if (code === "Equal" || code === "NumpadAdd" || input.key === "=" || input.key === "+") return "zoom-in";
  if (input.shiftKey) return null;
  if (code === "Minus" || code === "NumpadSubtract" || input.key === "-") return "zoom-out";
  if (code === "Digit0" || code === "Numpad0" || input.key === "0") return "zoom-reset";
  return null;
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
  // Focus mode (only the tabs and the terminal): F11 as in Windows Terminal's
  // full screen, ⌃⌘Z on macOS (⌃⌘F is the system's full screen). Both work
  // inside a terminal; full-screen programs use F1–F10.
  if (!mac && input.key === "F11" && !input.ctrlKey && !input.metaKey && !shiftKey) return "toggle-focus";
  if (mac && input.metaKey && input.ctrlKey && !shiftKey && (letter === "z" || input.code === "KeyZ")) return "toggle-focus";
  if (mac && input.metaKey && input.ctrlKey && !shiftKey && (letter === "s" || input.code === "KeyS")) return "toggle-sidebar";
  if (mac) {
    if (!input.metaKey || input.ctrlKey || shiftKey) return null;
    // iTerm2 habit: ⌘K clears the terminal; the palette is ⌘⇧K there.
    if (letter === "k") return inTerminal ? "clear-terminal" : "search-hosts";
    if (slash) return "shortcut-list";
    if (comma) return "settings";
    if (page) return page;
    if (letter === "i") return "import";
    if (letter === "n") return "new-host";
    return null;
  }
  if (input.metaKey) return null;
  // Ctrl+Shift everywhere (the form the app shows, so one shortcut works
  // wherever the focus is, as in Windows Terminal); plain Ctrl also works
  // outside the terminal, where it does not belong to the shell.
  if (!input.ctrlKey || (inTerminal && !shiftKey)) return null;
  if (letter === "k") return "search-hosts";
  if (slash) return "shortcut-list";
  if (comma) return "settings";
  if (page) return page;
  if (letter === "i") return "import";
  if (letter === "n") return "new-host";
  if (letter === "f") return "find";
  // keyboard.md: Ctrl+B shows or hides the sidebar. Inside a terminal Ctrl+B is
  // the shell's (and tmux's prefix), and Ctrl+Shift+B is broadcast.
  if (letter === "b" && !shiftKey) return "toggle-sidebar";
  // Ctrl+Shift+S folds the sidebar away from anywhere, the terminal included.
  if ((letter === "s" || input.code === "KeyS") && shiftKey) return "toggle-sidebar";
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
