import { t } from "./i18n";
import { isMac, type AppShortcut } from "./shortcuts";

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

/** The shortcut for a command, as the platform writes it; "" when there is none.
 *  `group` picks the row of a command listed twice (find: hosts or terminal). */
export function shortcutHint(id: ShortcutId, group?: ShortcutRow["group"]): string {
  const row = shortcutTable.find((candidate) => candidate.id === id && (!group || candidate.group === group));
  return row ? (isMac ? row.mac : row.pc) : "";
}
