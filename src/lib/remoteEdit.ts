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

export type EditCheck = "ok" | "not-file" | "too-large" | "binary";

/** Kinds of file a text editor cannot show: opening one only hangs the editor. */
const BINARY_EXTENSIONS = new Set(
  (
    "7z apk a aab ar avi bin bmp bz2 class cab db deb dll dmg doc docx dylib ear elf eot exe flac gif gz ico iso " +
    "jar jpeg jpg ko lz lz4 lzma m4a mkv mov mp3 mp4 msi o obj ogg otf pcap pdf png ppt pptx pyc pyo qcow2 rar rpm " +
    "so sqlite sqlite3 swp tar tbz tbz2 tgz tif tiff ttf txz vdi vhd vmdk war wasm wav webm webp woff woff2 xls xlsx xz z zip zst"
  ).split(" "),
);

/** The name says the file is binary (an archive, image, program...). Files
 *  the name says nothing about are checked by their content when opened. */
export function isBinaryName(name: string): boolean {
  const lower = name.toLowerCase();
  // Shared libraries are often versioned: libssl.so.3, libc.so.6.
  if (/\.so(\.\d+)+$/.test(lower)) return true;
  const dot = lower.lastIndexOf(".");
  return dot > 0 && BINARY_EXTENSIONS.has(lower.slice(dot + 1));
}

export function checkEditable(entry: Pick<RemoteFile, "kind" | "size"> & { name?: string }): EditCheck {
  if (entry.kind !== "file" && entry.kind !== "symlink") return "not-file";
  if (entry.name !== undefined && isBinaryName(entry.name)) return "binary";
  return entry.size > MAX_EDIT_BYTES ? "too-large" : "ok";
}

/** What `edit_open` answers for a copy whose content is binary. */
export const BINARY_CONTENT = "binary file";

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
