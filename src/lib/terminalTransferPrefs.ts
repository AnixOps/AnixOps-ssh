/** rz/sz preferences. They stay on this device, like the completion ones. */

export interface TransferPrefs {
  /** Watch terminal output for rz/sz and run the transfer. */
  enabled: boolean;
  /** Ask where to save each received file; off saves to Downloads. */
  askWhere: boolean;
}

const KEY = "anixops-ssh:transfer";

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function read(): TransferPrefs {
  try {
    const parsed = JSON.parse(storage()?.getItem(KEY) ?? "{}") as Partial<TransferPrefs>;
    return { enabled: parsed.enabled !== false, askWhere: parsed.askWhere === true };
  } catch {
    return { enabled: true, askWhere: false };
  }
}

let current = read();
const listeners = new Set<(prefs: TransferPrefs) => void>();

export function transferPrefs(): TransferPrefs {
  return current;
}

export function onTransferPrefs(listener: (prefs: TransferPrefs) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function updateTransferPrefs(patch: Partial<TransferPrefs>): void {
  current = { ...current, ...patch };
  try {
    storage()?.setItem(KEY, JSON.stringify(current));
  } catch {
    // The choice still applies until the app closes.
  }
  for (const listener of listeners) listener(current);
}
