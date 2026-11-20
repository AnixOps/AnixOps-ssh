/** Editing a remote file in a local editor: the decisions, free of the UI. */
import type { RemoteFile } from "@/lib/backend";

/** Larger files are not opened for editing (the same limit as the backend's). */
export const MAX_EDIT_BYTES = 20 * 1024 * 1024;

/** Quiet time after the last follow-the-terminal folder change before the pane moves. */
export const FOLLOW_DEBOUNCE_MS = 300;

/** What is known of the remote file: enough to notice that someone else changed it. */
export interface RemoteStamp {
  size: number;
  modified: string;
}

export const stampOf = (entry: Pick<RemoteFile, "size" | "modified">): RemoteStamp => ({
  size: entry.size,
  modified: entry.modified,
});

/** One watch per remote file on a host. */
export const editKey = (hostId: string, remote: string) => `${hostId}\u0000${remote}`;

export type EditCheck = "ok" | "not-file" | "too-large";

export function checkEditable(entry: Pick<RemoteFile, "kind" | "size">): EditCheck {
  if (entry.kind !== "file" && entry.kind !== "symlink") return "not-file";
  return entry.size > MAX_EDIT_BYTES ? "too-large" : "ok";
}

export type EditConflict = "same" | "changed" | "gone";

/** Compares the remote file now with what it was when the copy was downloaded. */
export function editConflict(base: RemoteStamp, current: RemoteStamp | null): EditConflict {
  if (!current) return "gone";
  return current.size === base.size && current.modified === base.modified ? "same" : "changed";
}

export interface EditingFile {
  key: string;
  /** The remote file. */
  remote: string;
  /** The folder it is in. */
  dir: string;
  name: string;
  /** The local copy. */
  local: string;
  base: RemoteStamp;
  /** The copy is being downloaded or uploaded. */
  busy: "download" | "upload" | null;
}

/** The local copy is named after the remote file, so the upload of a save lands on the same name. */
export function isEditableName(name: string): boolean {
  return name !== "" && name !== "." && name !== ".." && !/[\\/:*?"<>|\u0000-\u001f]/.test(name);
}
