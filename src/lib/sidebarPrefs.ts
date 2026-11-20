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
