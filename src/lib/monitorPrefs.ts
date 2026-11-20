import { useSyncExternalStore } from "react";

/* Whether new host tabs show the server monitor: the last choice, per
   device. Browser storage can be missing (private mode, tests), so every
   access is guarded and the default is "shown". */

const KEY = "anixops-ssh.monitor-open";

export function monitorOpenByDefault(): boolean {
  try {
    return window.localStorage.getItem(KEY) !== "false";
  } catch {
    return true;
  }
}

export function rememberMonitorOpen(open: boolean): void {
  try {
    window.localStorage.setItem(KEY, String(open));
  } catch {
    // Not remembered; the default stays.
  }
}

/* The monitor's width and whether it is folded to a narrow rail: one choice
   for every host, remembered per device. */

const WIDTH_KEY = "anixops-ssh.monitor-width";
const COLLAPSED_KEY = "anixops-ssh.monitor-collapsed";
const SORT_KEY = "anixops-ssh.monitor-top-sort";

export const MONITOR_WIDTH = { min: 248, max: 480, default: 288 } as const;

export function clampMonitorWidth(width: number): number {
  return Math.round(Math.min(MONITOR_WIDTH.max, Math.max(MONITOR_WIDTH.min, width)));
}

export function monitorWidth(): number {
  try {
    const value = Number(window.localStorage.getItem(WIDTH_KEY));
    return value > 0 ? clampMonitorWidth(value) : MONITOR_WIDTH.default;
  } catch {
    return MONITOR_WIDTH.default;
  }
}

export function rememberMonitorWidth(width: number): void {
  try {
    window.localStorage.setItem(WIDTH_KEY, String(clampMonitorWidth(width)));
  } catch {
    // Not remembered.
  }
}

export function monitorCollapsed(): boolean {
  try {
    return window.localStorage.getItem(COLLAPSED_KEY) === "true";
  } catch {
    return false;
  }
}

export function rememberMonitorCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(COLLAPSED_KEY, String(collapsed));
  } catch {
    // Not remembered.
  }
}

export function monitorTopSort(): "cpu" | "rss" {
  try {
    return window.localStorage.getItem(SORT_KEY) === "rss" ? "rss" : "cpu";
  } catch {
    return "cpu";
  }
}

export function rememberMonitorTopSort(sort: "cpu" | "rss"): void {
  try {
    window.localStorage.setItem(SORT_KEY, sort);
  } catch {
    // Not remembered.
  }
}

/* Every host tab has its own monitor, but the layout is one choice: a
   small store so that folding or resizing one folds or resizes them all. */

export interface MonitorLayout {
  width: number;
  collapsed: boolean;
}

let layout: MonitorLayout | null = null;
const listeners = new Set<() => void>();

function currentLayout(): MonitorLayout {
  layout ??= { width: monitorWidth(), collapsed: monitorCollapsed() };
  return layout;
}

export function setMonitorLayout(next: Partial<MonitorLayout>): void {
  const old = currentLayout();
  const merged = { ...old, ...next, width: clampMonitorWidth(next.width ?? old.width) };
  if (merged.width === old.width && merged.collapsed === old.collapsed) return;
  layout = merged;
  if (merged.width !== old.width) rememberMonitorWidth(merged.width);
  if (merged.collapsed !== old.collapsed) rememberMonitorCollapsed(merged.collapsed);
  for (const listener of listeners) listener();
}

export function useMonitorLayout(): MonitorLayout {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    currentLayout,
    currentLayout,
  );
}
