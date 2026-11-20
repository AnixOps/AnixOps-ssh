/* The frontend half of the update check. The Rust side fetches and verifies
   the signed feed (crates/easyssh-update); the page only shows the result and
   asks to open it. Free of DOM and Tauri imports so it can be tested. */

export interface UpdateInfo {
  version: string;
  build: number;
  /** The release asset; opened by the backend, never by the page. */
  url: string;
  notes: string;
  publishedAt: string | null;
}

export interface UpdateStatus {
  /** The build has a verification key: updates can be checked. */
  available: boolean;
  autoCheck: boolean;
  current: string;
  /** Unix seconds of the last successful check. */
  lastChecked: number | null;
  /** Only a build newer than the running one. */
  update: UpdateInfo | null;
}

export const NO_UPDATES: UpdateStatus = { available: false, autoCheck: true, current: "", lastChecked: null, update: null };

/** What the row says: no key, a newer release, current, or never checked. */
export function updateState(status: UpdateStatus): "unavailable" | "found" | "current" | "unchecked" {
  if (!status.available) return "unavailable";
  if (status.update) return "found";
  return status.lastChecked === null ? "unchecked" : "current";
}
