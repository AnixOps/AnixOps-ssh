/**
 * Where each host's terminal says it is, for the files pane that follows it.
 * Only a terminal whose shell has the integration script reports a folder;
 * with several terminals on one host the one that spoke last counts.
 */
export interface TerminalCwd {
  /** The shell reported itself, so a folder will be reported. */
  integration: boolean;
  cwd: string | undefined;
}

const none: TerminalCwd = { integration: false, cwd: undefined };
const states = new Map<string, { owner: symbol; state: TerminalCwd }>();
const listeners = new Map<string, Set<(state: TerminalCwd) => void>>();

export function terminalCwd(hostId: string): TerminalCwd {
  return states.get(hostId)?.state ?? none;
}

export function publishTerminalCwd(hostId: string, owner: symbol, state: TerminalCwd): void {
  states.set(hostId, { owner, state });
  for (const listener of listeners.get(hostId) ?? []) listener(state);
}

/** The terminal closed: its report goes, unless another terminal has taken over. */
export function clearTerminalCwd(hostId: string, owner: symbol): void {
  if (states.get(hostId)?.owner !== owner) return;
  states.delete(hostId);
  for (const listener of listeners.get(hostId) ?? []) listener(none);
}

export function onTerminalCwd(hostId: string, listener: (state: TerminalCwd) => void): () => void {
  let set = listeners.get(hostId);
  if (!set) listeners.set(hostId, (set = new Set()));
  set.add(listener);
  return () => void set.delete(listener);
}

/** The folder the pane should show, or null to leave it alone. */
export function followTarget(options: { enabled: boolean; state: TerminalCwd; shown: string }): string | null {
  const { enabled, state, shown } = options;
  if (!enabled || !state.integration || !state.cwd) return null;
  const cwd = state.cwd.length > 1 ? state.cwd.replace(/\/+$/, "") : state.cwd;
  if (!cwd.startsWith("/")) return null;
  return cwd === shown ? null : cwd;
}
