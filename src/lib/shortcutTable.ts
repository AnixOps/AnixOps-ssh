import { t } from "./i18n";
import { isMac, type AppShortcut } from "./shortcuts";

export type ShortcutId = AppShortcut | "focus-pane" | "rename" | "remove" | "select-all" | "properties" | "row-menu";

export interface ShortcutRow {
  id: ShortcutId;
  group: "app" | "terminal" | "list";
  title: () => string;
  /** The shortcut shown everywhere (tooltips, menus, the palette); it works
   *  whether or not the terminal has the focus. */
  pc: string;
  mac: string;
  /** A shorter form that works only outside the terminal, where plain Ctrl
   *  does not belong to the shell; listed in the shortcut list only. */
  pcOutside?: string;
  macOutside?: string;
}

/** Every shortcut, once (K9). */
export const shortcutTable: ShortcutRow[] = [
  { id: "search-hosts", group: "app", title: () => t.palette, pc: "Ctrl+Shift+K", mac: "⌘⇧K", pcOutside: "Ctrl+K", macOutside: "⌘K" },
  { id: "shortcut-list", group: "app", title: () => t.shortcutList, pc: "Ctrl+Shift+/", mac: "⌘/", pcOutside: "Ctrl+/" },
  { id: "settings", group: "app", title: () => t.navSettings, pc: "Ctrl+Shift+,", mac: "⌘,", pcOutside: "Ctrl+," },
  { id: "go-hosts", group: "app", title: () => t.navHosts, pc: "Ctrl+Shift+1", mac: "⌘1", pcOutside: "Ctrl+1" },
  { id: "go-forwards", group: "app", title: () => t.navForwards, pc: "Ctrl+Shift+2", mac: "⌘2", pcOutside: "Ctrl+2" },
  { id: "go-snippets", group: "app", title: () => t.navSnippets, pc: "Ctrl+Shift+3", mac: "⌘3", pcOutside: "Ctrl+3" },
  { id: "go-team", group: "app", title: () => t.navTeam, pc: "Ctrl+Shift+4", mac: "⌘4", pcOutside: "Ctrl+4" },
  { id: "import", group: "app", title: () => t.importHosts, pc: "Ctrl+Shift+I", mac: "⌘I", pcOutside: "Ctrl+I" },
  { id: "new-host", group: "app", title: () => t.newHost, pc: "Ctrl+Shift+N", mac: "⌘N", pcOutside: "Ctrl+N" },
  { id: "find", group: "app", title: () => t.searchHosts, pc: "Ctrl+F", mac: "⌘F" },
  { id: "toggle-sidebar", group: "app", title: () => t.toggleSidebar, pc: "Ctrl+Shift+S", mac: "⌃⌘S", pcOutside: "Ctrl+B" },
  { id: "toggle-focus", group: "app", title: () => t.focusMode, pc: "F11", mac: "⌃⌘Z" },
  { id: "new-local", group: "terminal", title: () => t.newTab, pc: "Ctrl+Shift+T", mac: "⌘T" },
  { id: "next-tab", group: "terminal", title: () => t.nextTab, pc: "Ctrl+Tab", mac: "⌃Tab" },
  { id: "prev-tab", group: "terminal", title: () => t.previousTab, pc: "Ctrl+Shift+Tab", mac: "⌃⇧Tab" },
  { id: "close-tab", group: "terminal", title: () => t.closeTab, pc: "Ctrl+Shift+W", mac: "⌘W" },
  { id: "toggle-files", group: "terminal", title: () => t.toggleFiles, pc: "Ctrl+Shift+E", mac: "⌘E" },
  { id: "find", group: "terminal", title: () => t.find, pc: "Ctrl+Shift+F", mac: "⌘F" },
  { id: "clear-terminal", group: "terminal", title: () => t.clearTerminal, pc: "", mac: "⌘K" },
  { id: "copy", group: "terminal", title: () => t.copy, pc: "Ctrl+Shift+C", mac: "⌘C" },
  { id: "paste", group: "terminal", title: () => t.paste, pc: "Ctrl+Shift+V", mac: "⌘V" },
  { id: "zoom-in", group: "terminal", title: () => t.zoomIn, pc: "Ctrl+=", mac: "⌘+" },
  { id: "zoom-out", group: "terminal", title: () => t.zoomOut, pc: "Ctrl+-", mac: "⌘-" },
  { id: "zoom-reset", group: "terminal", title: () => t.zoomReset, pc: "Ctrl+0", mac: "⌘0" },
  { id: "split-right", group: "terminal", title: () => t.splitRight, pc: "Ctrl+Shift+D", mac: "⌘D" },
  { id: "split-down", group: "terminal", title: () => t.splitDown, pc: "Ctrl+Shift+O", mac: "⌘⇧D" },
  { id: "focus-pane", group: "terminal", title: () => t.focusPane, pc: "Ctrl+Shift+←↑↓→", mac: "⌘⌥←↑↓→" },
  { id: "toggle-broadcast", group: "terminal", title: () => t.broadcastTab, pc: "Ctrl+Shift+B", mac: "⌘⇧B" },
  { id: "toggle-maximise", group: "terminal", title: () => t.maximisePane, pc: "Ctrl+Shift+Enter", mac: "⌘⇧↵" },
  { id: "prev-prompt", group: "terminal", title: () => t.promptPrevious, pc: "Ctrl+Shift+PageUp", mac: "⌘↑" },
  { id: "next-prompt", group: "terminal", title: () => t.promptNext, pc: "Ctrl+Shift+PageDown", mac: "⌘↓" },
  { id: "rename", group: "list", title: () => t.editHost, pc: "F2", mac: "⌘E" },
  { id: "remove", group: "list", title: () => t.delete, pc: "Delete", mac: "⌘⌫" },
  { id: "select-all", group: "list", title: () => t.selectAll, pc: "Ctrl+A", mac: "⌘A" },
  { id: "properties", group: "list", title: () => t.properties, pc: "Alt+Enter", mac: "⌘I" },
  { id: "row-menu", group: "list", title: () => t.more, pc: "Shift+F10", mac: "⌃↵" },
];

/** The shortcut for a command, as the platform writes it; "" when there is none.
 *  `group` picks the row of a command listed twice (find: hosts or terminal). */
export function shortcutHint(id: ShortcutId, group?: ShortcutRow["group"]): string {
  const row = shortcutTable.find((candidate) => candidate.id === id && (!group || candidate.group === group));
  return row ? (isMac ? row.mac : row.pc) : "";
}
