/** Completion preferences. They stay on this device, like the history they control. */
import { HistoryStore } from "./history";

export interface CompletionPrefs {
  enabled: boolean;
  rememberHistory: boolean;
}

const KEY = "anixops-ssh:completion";

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function read(): CompletionPrefs {
  try {
    const parsed = JSON.parse(storage()?.getItem(KEY) ?? "{}") as Partial<CompletionPrefs>;
    return { enabled: parsed.enabled !== false, rememberHistory: parsed.rememberHistory !== false };
  } catch {
    return { enabled: true, rememberHistory: true };
  }
}

let current = read();
const listeners = new Set<(prefs: CompletionPrefs) => void>();

export const commandHistory = new HistoryStore(storage(), () => current.rememberHistory);

export function completionPrefs(): CompletionPrefs {
  return current;
}

export function onCompletionPrefs(listener: (prefs: CompletionPrefs) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function updateCompletionPrefs(patch: Partial<CompletionPrefs>): void {
  current = { ...current, ...patch };
  try {
    storage()?.setItem(KEY, JSON.stringify(current));
  } catch {
    // The choice still applies until the app closes.
  }
  // Turning remembering off also forgets what was remembered.
  if (patch.rememberHistory === false) commandHistory.clear();
  for (const listener of listeners) listener(current);
}
