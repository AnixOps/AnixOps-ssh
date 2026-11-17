/* App shortcuts must not collide with the shell: Ctrl+W, Ctrl+K and Ctrl+T
   all mean something in bash and vim. So the app uses Cmd on macOS and
   Ctrl+Shift elsewhere, like most terminal apps. */

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
  | "toggle-maximise";

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

export interface KeyInput {
  key: string;
  code?: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** The shortcut a key press means on the given platform, if any.
 *
 *  Windows and Linux: Ctrl+Shift+key; Alt added for "split down".
 *  macOS: Cmd+key; Cmd+Shift+D splits down, Cmd+Option+Arrow moves focus,
 *  Cmd+Shift+B toggles broadcast. */
export function resolveShortcut(input: KeyInput, mac: boolean): AppShortcut | null {
  const { shiftKey, altKey } = input;
  const letter = input.key.toLowerCase();
  const isD = letter === "d" || input.code === "KeyD";
  if (mac) {
    if (!input.metaKey || input.ctrlKey) return null;
    if (arrows[input.key]) return altKey && !shiftKey ? arrows[input.key] : null;
    if (altKey) return null;
    if (isD) return shiftKey ? "split-down" : "split-right";
    if (letter === "b") return shiftKey ? "toggle-broadcast" : null;
    if (input.key === "Enter") return shiftKey ? "toggle-maximise" : null;
    const shortcut = letters[letter] ?? null;
    // The native Cmd+C / Cmd+V already copy and paste in the terminal.
    return shortcut === "copy" || shortcut === "paste" ? null : shortcut;
  }
  if (!input.ctrlKey || !shiftKey || input.metaKey) return null;
  if (isD) return altKey ? "split-down" : "split-right";
  if (altKey) return null;
  if (arrows[input.key]) return arrows[input.key];
  if (letter === "b") return "toggle-broadcast";
  if (input.key === "Enter") return "toggle-maximise";
  return letters[letter] ?? null;
}

export function appShortcut(event: KeyboardEvent): AppShortcut | null {
  if (event.type !== "keydown") return null;
  return resolveShortcut(event, isMac);
}

/** Display form for a letter shortcut (Ctrl+Shift+W or ⌘W). */
export function shortcutLabel(key: string): string {
  return isMac ? `⌘${key.toUpperCase()}` : `Ctrl+Shift+${key.toUpperCase()}`;
}

const labels: Partial<Record<AppShortcut, { pc: string; mac: string }>> = {
  "split-right": { pc: "Ctrl+Shift+D", mac: "⌘D" },
  "split-down": { pc: "Ctrl+Shift+Alt+D", mac: "⌘⇧D" },
  "toggle-broadcast": { pc: "Ctrl+Shift+B", mac: "⌘⇧B" },
  "toggle-maximise": { pc: "Ctrl+Shift+Enter", mac: "⌘⇧↵" },
  "close-tab": { pc: "Ctrl+Shift+W", mac: "⌘W" },
};

export function shortcutHint(shortcut: AppShortcut | "focus-pane"): string {
  if (shortcut === "focus-pane") return isMac ? "⌘⌥←↑↓→" : "Ctrl+Shift+←↑↓→";
  const label = labels[shortcut];
  return label ? (isMac ? label.mac : label.pc) : "";
}
