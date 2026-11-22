/* Whether the sidebar (host list, forwards, snippets…) is hidden, so the
   terminal can use the whole window: remembered per device. Browser storage
   can be missing (private mode, tests), so every access is guarded. */

const KEY = "anixops-ssh.sidebar-hidden";
const FOCUS_KEY = "anixops-ssh.focus-mode";

/** Focus mode: only the tabs and the terminal, no icon rail or sidebar. */
export function focusModeByDefault(): boolean {
  try {
    return window.localStorage.getItem(FOCUS_KEY) === "true";
  } catch {
    return false;
  }
}

export function rememberFocusMode(on: boolean): void {
  try {
    window.localStorage.setItem(FOCUS_KEY, String(on));
  } catch {
    // Not remembered.
  }
}

export function sidebarHiddenByDefault(): boolean {
  try {
    return window.localStorage.getItem(KEY) === "true";
  } catch {
    return false;
  }
}

export function rememberSidebarHidden(hidden: boolean): void {
  try {
    window.localStorage.setItem(KEY, String(hidden));
  } catch {
    // Not remembered; the sidebar shows again next time.
  }
}

const PINNED_KEY = "anixops-ssh.sidebar-pinned";

/** Pinned: the sidebar stays beside the terminal while sessions are open.
 *  Unpinned (the default), opening a session folds it away so the terminal
 *  gets the window, and the rail brings it back over the terminal. */
export function sidebarPinnedByDefault(): boolean {
  try {
    return window.localStorage.getItem(PINNED_KEY) === "true";
  } catch {
    return false;
  }
}

export function rememberSidebarPinned(pinned: boolean): void {
  try {
    window.localStorage.setItem(PINNED_KEY, String(pinned));
  } catch {
    // Not remembered.
  }
}
