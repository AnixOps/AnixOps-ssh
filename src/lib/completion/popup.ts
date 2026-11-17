import type { Candidate } from "./types";

/** The open suggestion list. `index` is the highlighted item, or null when none is. */
export interface PopupState {
  items: Candidate[];
  index: number | null;
}

export interface PopupKey {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
}

export interface PopupOutcome {
  /** The key was used by the popup and must not reach the shell. */
  consumed: boolean;
  state: PopupState;
  /** The item to insert. */
  accept?: Candidate;
  /** Hide the list until the line changes. */
  dismiss?: boolean;
}

export function openPopup(items: Candidate[]): PopupState {
  return { items, index: null };
}

/**
 * Decides what a key does while a list is showing. Only Tab, Up/Down, Enter
 * (with an item highlighted) and Escape are taken; everything else, including
 * Enter with nothing highlighted, goes to the shell untouched.
 */
export function handlePopupKey(state: PopupState, event: PopupKey): PopupOutcome {
  const pass: PopupOutcome = { consumed: false, state };
  if (state.items.length === 0) return pass;
  if (event.ctrlKey || event.altKey || event.metaKey) return pass;
  const count = state.items.length;
  switch (event.key) {
    case "Tab": {
      if (event.shiftKey) return pass;
      const item = state.items[state.index ?? 0];
      return { consumed: true, state, accept: item };
    }
    case "ArrowDown": {
      if (event.shiftKey) return pass;
      const index = state.index === null ? 0 : (state.index + 1) % count;
      return { consumed: true, state: { ...state, index } };
    }
    case "ArrowUp": {
      // With nothing highlighted, Up still browses the shell's history.
      if (event.shiftKey || state.index === null) return pass;
      return { consumed: true, state: { ...state, index: state.index === 0 ? null : state.index - 1 } };
    }
    case "Enter":
      if (state.index === null || event.shiftKey) return pass;
      return { consumed: true, state, accept: state.items[state.index] };
    case "Escape":
      return { consumed: true, state, dismiss: true };
    default:
      return pass;
  }
}
