/** Open terminals by tab key, so other panels can paste into them. */
export interface TerminalTarget {
  /** Pastes text (bracketed when the shell asks for it); `run` presses Enter after. */
  send(text: string, run: boolean): void;
  /** Raw input to the session, as if typed here (no echo, no focus change). */
  write(data: string): void;
  /** Pastes text without running it or taking focus. */
  paste(text: string): void;
  connected(): boolean;
  /** Gives the terminal the keyboard focus. */
  focus(): void;
  /** Ends the tab's session (closing the tab); unmounting alone only detaches. */
  close(): void;
}

const terminals = new Map<string, TerminalTarget>();

export function registerTerminal(key: string, target: TerminalTarget): () => void {
  terminals.set(key, target);
  return () => {
    if (terminals.get(key) === target) terminals.delete(key);
  };
}

export function terminalFor(key: string | null): TerminalTarget | undefined {
  return key ? terminals.get(key) : undefined;
}

export function connectedTerminals(): TerminalTarget[] {
  return [...terminals.values()].filter((target) => target.connected());
}

/** Puts the keyboard back in a terminal (after a panel or overlay closes). */
export function focusTerminal(key: string | null): void {
  // After the closing panel has left the page and its own focus handling ran.
  window.requestAnimationFrame(() => terminalFor(key)?.focus());
}
