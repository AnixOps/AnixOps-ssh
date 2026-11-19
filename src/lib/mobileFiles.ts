import type { RemoteFile } from "./backend";

/** Pure helpers of the phone's file browser. */

/** The path of `name` inside `dir`, with exactly one slash between them. */
export function joinPath(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

export { parentPath } from "./remotePath";

export interface Crumb {
  name: string;
  path: string;
}

/** The path as steps from the root, for the breadcrumb row. */
export function crumbs(path: string): Crumb[] {
  const steps: Crumb[] = [{ name: "/", path: "/" }];
  let at = "";
  for (const part of path.split("/").filter(Boolean)) {
    at = `${at}/${part}`;
    steps.push({ name: part, path: at });
  }
  return steps;
}

/** Folders first, then by name ignoring case; hidden files sort with the rest. */
export function sortFiles(entries: RemoteFile[]): RemoteFile[] {
  const folder = (entry: RemoteFile) => entry.kind === "directory";
  return [...entries].sort(
    (a, b) => Number(folder(b)) - Number(folder(a)) || a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

/** Whether a name can be used as a single path component. */
export function validName(name: string): boolean {
  const trimmed = name.trim();
  return trimmed !== "" && trimmed !== "." && trimmed !== ".." && !/[/\0]/.test(trimmed);
}

/** `1.4 MB`, `812 B`; powers of 1024, like the desktop file pane. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/** The byte ranges a file of `size` bytes is sent in. An empty file is one
 *  empty range, so that the staged file still exists. */
export function chunkRanges(size: number, chunk: number): Array<[number, number]> {
  if (size <= 0) return [[0, 0]];
  const ranges: Array<[number, number]> = [];
  for (let start = 0; start < size; start += chunk) ranges.push([start, Math.min(size, start + chunk)]);
  return ranges;
}
