/* Remembers each window's tabs and pane trees across a UI reload, so the
   daemon's sessions come back in the arrangement they had. Only shapes and
   session ids are saved; the sessions themselves live in the daemon. */

import {
  isSavedNode,
  pruneSaved,
  savedSessions,
  type SavedNode,
} from "./panes";

export interface SavedTab {
  hostId: string | null;
  filesOpen: boolean;
  tree: SavedNode;
  /** Position of the focused / maximised pane among the leaves, left to right. */
  focused: number;
  maximised: number | null;
}

export interface SavedLayout {
  version: 1;
  /** Index of the active tab. */
  active: number | null;
  tabs: SavedTab[];
}

/** A tab to rebuild after a reload. */
export type PlannedTab = SavedTab;

export interface LiveRef {
  id: string;
  hostId: string | null;
}

function isSavedTab(value: unknown): value is SavedTab {
  if (typeof value !== "object" || value === null) return false;
  const tab = value as Record<string, unknown>;
  return (
    (tab.hostId === null || typeof tab.hostId === "string") &&
    typeof tab.focused === "number" &&
    (tab.maximised === null || typeof tab.maximised === "number") &&
    isSavedNode(tab.tree)
  );
}

export function parseLayout(raw: string | null): SavedLayout | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<SavedLayout>;
    if (value.version !== 1 || !Array.isArray(value.tabs)) return null;
    const tabs = value.tabs.filter(isSavedTab).map((tab) => ({ ...tab, filesOpen: tab.filesOpen === true }));
    return { version: 1, active: typeof value.active === "number" ? value.active : null, tabs };
  } catch {
    return null;
  }
}

/** Matches a saved layout against the sessions the daemon still has: dead
 *  panes drop out, tabs left empty vanish, and live sessions the layout never
 *  mentioned come back as tabs of their own (the pre-split behaviour). */
export function planRestore(
  saved: SavedLayout | null,
  live: readonly LiveRef[],
): { tabs: PlannedTab[]; active: number | null } {
  const alive = new Set(live.map((session) => session.id));
  const used = new Set<string>();
  const tabs: PlannedTab[] = [];
  let active: number | null = null;

  (saved?.tabs ?? []).forEach((tab, index) => {
    const tree = pruneSaved(tab.tree, (id) => alive.has(id) && !used.has(id));
    if (!tree) return;
    for (const id of savedSessions(tree)) used.add(id);
    // Indices refer to the unpruned tree, so recompute them on the leaves that remain.
    const before = leafSessions(tab.tree);
    const after = leafSessions(tree);
    const keep = (index: number | null) => {
      if (index === null) return null;
      const session = before[index];
      const position = session === null || session === undefined ? -1 : after.indexOf(session);
      return position >= 0 ? position : null;
    };
    tabs.push({
      hostId: tab.hostId,
      filesOpen: tab.filesOpen,
      tree,
      focused: keep(tab.focused) ?? 0,
      maximised: keep(tab.maximised),
    });
    if (saved?.active === index) active = tabs.length - 1;
  });

  for (const session of live) {
    if (used.has(session.id)) continue;
    tabs.push({
      hostId: session.hostId,
      filesOpen: false,
      tree: { t: "leaf", session: session.id },
      focused: 0,
      maximised: null,
    });
  }
  return { tabs, active: active ?? (tabs.length > 0 ? 0 : null) };
}

function leafSessions(node: SavedNode): (string | null)[] {
  return node.t === "leaf" ? [node.session] : [...leafSessions(node.a), ...leafSessions(node.b)];
}

/* Browser storage; per window, so a second window does not overwrite the first. */

let windowLabel = "main";

export function setWindowLabel(label: string) {
  windowLabel = label;
}

const key = () => `anixops-ssh:layout:${windowLabel}`;

export function loadLayout(): SavedLayout | null {
  try {
    return parseLayout(localStorage.getItem(key()));
  } catch {
    return null;
  }
}

export function saveLayout(layout: SavedLayout): void {
  try {
    localStorage.setItem(key(), JSON.stringify(layout));
  } catch {
    // Storage can be unavailable; the tabs then come back one per session.
  }
}
