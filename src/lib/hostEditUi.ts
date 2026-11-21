const listeners = new Set<(hostId: string) => void>();

/** Asks the app to open the host editor for a saved host (from a failed connect card). */
export function requestHostEdit(hostId: string): void {
  for (const listener of listeners) listener(hostId);
}

export function onHostEditRequested(handler: (hostId: string) => void): () => void {
  listeners.add(handler);
  return () => void listeners.delete(handler);
}
