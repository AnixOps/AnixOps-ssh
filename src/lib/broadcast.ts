/* Broadcast input: what is typed in one pane is sent to the others. The
   scope is either one tab ("all panes in this tab") or everything ("all
   panes in all tabs"). Targeting is pure; a small store holds the switches
   so the terminals, the tab bar and the snippet panel agree. */

import { useSyncExternalStore } from "react";

export interface BroadcastState {
  global: boolean;
  /** Tabs whose panes mirror one another. */
  tabs: ReadonlySet<string>;
}

/** Pane ids by tab key. */
export type PanesByTab = ReadonlyMap<string, readonly string[]>;

export type BroadcastScope = "global" | "tab" | null;

export function scopeOf(state: BroadcastState, tabKey: string): BroadcastScope {
  if (state.global) return "global";
  return state.tabs.has(tabKey) ? "tab" : null;
}

/** Every pane that takes part in the broadcast the given tab belongs to (the source included). */
export function participants(state: BroadcastState, panes: PanesByTab, tabKey: string): string[] {
  const scope = scopeOf(state, tabKey);
  if (scope === "global") return [...panes.values()].flat();
  if (scope === "tab") return [...(panes.get(tabKey) ?? [])];
  return [];
}

/** The panes that should also receive what `source` gets; none when broadcast is off for its tab. */
export function broadcastTargets(
  state: BroadcastState,
  panes: PanesByTab,
  source: string,
  isConnected: (paneId: string) => boolean = () => true,
): string[] {
  let tabKey: string | undefined;
  for (const [key, ids] of panes) {
    if (ids.includes(source)) tabKey = key;
  }
  if (tabKey === undefined) return [];
  return participants(state, panes, tabKey).filter((id) => id !== source && isConnected(id));
}

/** Whether a paste needs confirmation before it is sent to several panes. */
export function pasteNeedsConfirm(text: string, receivers: number): boolean {
  return receivers > 0 && /[\r\n]/.test(text);
}

/* The store. */

export interface BroadcastSnapshot extends BroadcastState {
  panes: PanesByTab;
}

let snapshot: BroadcastSnapshot = { global: false, tabs: new Set(), panes: new Map() };
const listeners = new Set<() => void>();

function publish(next: BroadcastSnapshot) {
  snapshot = next;
  for (const listener of listeners) listener();
}

export function broadcastSnapshot(): BroadcastSnapshot {
  return snapshot;
}

export function useBroadcast(): BroadcastSnapshot {
  return useSyncExternalStore((listener) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  }, broadcastSnapshot);
}

/** Tells the store which panes exist; tabs that closed stop broadcasting. */
export function setPanes(panes: PanesByTab) {
  const tabs = new Set([...snapshot.tabs].filter((key) => panes.has(key)));
  publish({ ...snapshot, panes, tabs: tabs.size === snapshot.tabs.size ? snapshot.tabs : tabs });
}

export function toggleTabBroadcast(tabKey: string) {
  const tabs = new Set(snapshot.tabs);
  if (!tabs.delete(tabKey)) tabs.add(tabKey);
  publish({ ...snapshot, tabs });
}

export function toggleGlobalBroadcast() {
  publish({ ...snapshot, global: !snapshot.global });
}

/** Panes (other than `source`) that should mirror what it receives. */
export function targetsFor(source: string, isConnected: (paneId: string) => boolean): string[] {
  return broadcastTargets(snapshot, snapshot.panes, source, isConnected);
}
