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

/** What a single-pane paste check needs to know. */
export interface PasteGuard {
  /** The app in the pane turned on bracketed paste, so a newline will not run anything. */
  bracketedPaste: boolean;
  /** The setting: warn before a multi-line paste into one pane. */
  warnMultiline: boolean;
  /** The host is production: ask even when the setting is off. */
  production?: boolean;
}

/** Whether a paste needs confirmation: always when it has a newline and goes
 *  to several panes; into one pane only when the setting asks for it and the
 *  app there has not turned on bracketed paste (each line would run). A
 *  production host asks whatever the setting says. */
export function pasteNeedsConfirm(text: string, receivers: number, guard?: PasteGuard): boolean {
  if (!/[\r\n]/.test(text)) return false;
  if (receivers > 0) return true;
  if (guard === undefined || guard.bracketedPaste) return false;
  return guard.warnMultiline || guard.production === true;
}

/** Production tabs by key, with the name to show for each. */
export type ProductionTabs = ReadonlyMap<string, string>;

/** The production tabs a scope would send input to, as `[key, name]`, whose
 *  broadcast the person has not yet confirmed. `scope` is where `tabKey`
 *  would broadcast to. */
export function unconfirmedProduction(
  scope: BroadcastScope,
  tabKey: string,
  panes: PanesByTab,
  production: ProductionTabs,
  confirmed: ReadonlySet<string>,
): Array<[string, string]> {
  if (scope === null) return [];
  const keys = scope === "global" ? [...panes.keys()] : [tabKey];
  const out: Array<[string, string]> = [];
  for (const key of keys) {
    const name = production.get(key);
    if (name !== undefined && !confirmed.has(key)) out.push([key, name]);
  }
  return out;
}

/** Panes that may receive broadcast input: those of a production tab only
 *  once the person confirmed it, so a production terminal that joins later
 *  is never typed into unasked. */
export function withoutUnconfirmed(
  ids: readonly string[],
  panes: PanesByTab,
  production: ProductionTabs,
  confirmed: ReadonlySet<string>,
): string[] {
  const blocked = new Set<string>();
  for (const [key, paneIds] of panes) {
    if (production.has(key) && !confirmed.has(key)) for (const id of paneIds) blocked.add(id);
  }
  return ids.filter((id) => !blocked.has(id));
}

/* The store. */

export interface BroadcastSnapshot extends BroadcastState {
  panes: PanesByTab;
  /** Tabs of production hosts, by key, with their names. */
  production: ProductionTabs;
  /** Production tabs the person confirmed to receive broadcast input. */
  confirmed: ReadonlySet<string>;
}

let snapshot: BroadcastSnapshot = {
  global: false,
  tabs: new Set(),
  panes: new Map(),
  production: new Map(),
  confirmed: new Set(),
};
const listeners = new Set<() => void>();

function publish(next: BroadcastSnapshot) {
  // With broadcast off nothing stays confirmed: the next start asks again.
  if (!next.global && next.tabs.size === 0 && next.confirmed.size > 0) next = { ...next, confirmed: new Set() };
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

/** Sets where what is typed in `tabKey` goes: nowhere else, the tab's other
 *  panes, or every pane. Choosing one scope turns the other off. */
export function setBroadcastScope(tabKey: string, scope: BroadcastScope) {
  const tabs = new Set(snapshot.tabs);
  if (scope === "tab") tabs.add(tabKey);
  else tabs.delete(tabKey);
  publish({ ...snapshot, global: scope === "global", tabs });
}

/** Tells the store which tabs are production hosts. */
export function setProductionTabs(production: ProductionTabs) {
  const same =
    production.size === snapshot.production.size &&
    [...production].every(([key, name]) => snapshot.production.get(key) === name);
  if (same) return;
  // A tab that stopped being production, or closed, needs no confirmation any more.
  const confirmed = new Set([...snapshot.confirmed].filter((key) => production.has(key)));
  publish({ ...snapshot, production, confirmed });
}

/** The production tabs that would newly receive input if `tabKey` broadcast to `scope`. */
export function pendingProduction(tabKey: string, scope: BroadcastScope): Array<[string, string]> {
  return unconfirmedProduction(scope, tabKey, snapshot.panes, snapshot.production, snapshot.confirmed);
}

/** Records that the person agreed to broadcast to these production tabs. */
export function confirmProduction(keys: readonly string[]) {
  publish({ ...snapshot, confirmed: new Set([...snapshot.confirmed, ...keys]) });
}

/** Panes (other than `source`) that should mirror what it receives. */
export function targetsFor(source: string, isConnected: (paneId: string) => boolean): string[] {
  const ids = broadcastTargets(snapshot, snapshot.panes, source, isConnected);
  return withoutUnconfirmed(ids, snapshot.panes, snapshot.production, snapshot.confirmed);
}
