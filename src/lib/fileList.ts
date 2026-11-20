/* How the files pane orders and filters a folder's entries: folders first,
   then by the column the user picked (name, size or modified time), and
   dot files shown or hidden. The choice is remembered on this device. */

import type { RemoteFile } from "@/lib/backend";

export type SortKey = "name" | "size" | "modified";

export interface FileSort {
  key: SortKey;
  descending: boolean;
}

export const DEFAULT_SORT: FileSort = { key: "name", descending: false };

const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/** The time in an `ls -l` style date (`Oct 14 09:12`, recent, or
 *  `Jan  3  2025`, older), as milliseconds, or null when it is not one. A
 *  date without a year is in the last twelve months, as `ls` prints it. */
export function listingTime(text: string, now: Date = new Date()): number | null {
  const match = /^([A-Za-z]{3})\s+(\d{1,2})\s+(?:(\d{1,2}):(\d{2})|(\d{4}))$/.exec(text.trim());
  if (!match) return null;
  const month = MONTHS.indexOf(match[1].toLowerCase());
  if (month < 0) return null;
  const day = Number(match[2]);
  if (match[5]) return new Date(Number(match[5]), month, day).getTime();
  const time = new Date(now.getFullYear(), month, day, Number(match[3]), Number(match[4])).getTime();
  // A day later than today belongs to last year.
  return time > now.getTime() + 86_400_000 ? new Date(now.getFullYear() - 1, month, day, Number(match[3]), Number(match[4])).getTime() : time;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Folders first, then by `sort`; ties fall back to the name. */
export function sortEntries(entries: RemoteFile[], sort: FileSort = DEFAULT_SORT, now: Date = new Date()): RemoteFile[] {
  const sign = sort.descending ? -1 : 1;
  const value = (entry: RemoteFile): number | null =>
    sort.key === "size" ? (entry.kind === "directory" ? null : entry.size) : sort.key === "modified" ? listingTime(entry.modified, now) : null;
  return [...entries].sort((a, b) => {
    const folders = (a.kind === "directory" ? 0 : 1) - (b.kind === "directory" ? 0 : 1);
    if (folders !== 0) return folders;
    const byName = collator.compare(a.name, b.name);
    if (sort.key === "name") return sign * byName;
    const [x, y] = [value(a), value(b)];
    // Entries without a value (folders by size, unreadable dates) keep name order, after the rest.
    if (x === null || y === null) return x === y ? byName : x === null ? 1 : -1;
    return x !== y ? sign * (x - y) : byName;
  });
}

/** The column's next order: a new column starts ascending for names and
 *  descending for size and time (largest, newest first), as in Finder. */
export function nextSort(current: FileSort, key: SortKey): FileSort {
  if (current.key === key) return { key, descending: !current.descending };
  return { key, descending: key !== "name" };
}

export function isHidden(entry: Pick<RemoteFile, "name">): boolean {
  return entry.name.startsWith(".");
}

export function visibleEntries(entries: RemoteFile[], showHidden: boolean): RemoteFile[] {
  return showHidden ? entries : entries.filter((entry) => !isHidden(entry));
}

const SORT_KEY = "anixops-ssh.files-sort";
const HIDDEN_KEY = "anixops-ssh.files-show-hidden";

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not remembered; the pane still works.
  }
}

export function savedSort(): FileSort {
  const [key, order] = (read(SORT_KEY) ?? "").split(":");
  if (key !== "name" && key !== "size" && key !== "modified") return DEFAULT_SORT;
  return { key, descending: order === "desc" };
}

export function rememberSort(sort: FileSort): void {
  write(SORT_KEY, `${sort.key}:${sort.descending ? "desc" : "asc"}`);
}

/** Dot files are shown unless the user hid them (FinalShell, WinSCP). */
export function savedShowHidden(): boolean {
  return read(HIDDEN_KEY) !== "false";
}

export function rememberShowHidden(show: boolean): void {
  write(HIDDEN_KEY, String(show));
}
