import type { RemoteFile } from "./backend";

/** Remote (POSIX) path helpers shared by the file panes. */

/** Drops `.`, empty and `..` steps; `..` never climbs above the root. */
export function normalizePath(path: string): string {
  const steps: string[] = [];
  for (const step of path.split("/")) {
    if (step === "" || step === ".") continue;
    if (step === "..") steps.pop();
    else steps.push(step);
  }
  return `/${steps.join("/")}`;
}

/**
 * The absolute path that a typed location stands for. A leading `/` is
 * absolute as typed; `~` and `~/…` start at the home folder; anything else is
 * relative to the folder being shown. Surrounding quotes from a pasted path
 * are removed.
 */
export function resolvePath(input: string, cwd: string, home: string): string {
  let text = input.trim();
  const quoted = /^(['"])(.*)\1$/.exec(text);
  if (quoted) text = quoted[2];
  if (text.startsWith("/")) return normalizePath(text);
  if (text === "~") return normalizePath(home);
  if (text.startsWith("~/")) return normalizePath(`${home}/${text.slice(2)}`);
  return normalizePath(`${cwd || home}/${text}`);
}

/** The folder that contains `path`; the root is its own parent. */
export function parentPath(path: string): string {
  const trimmed = path.length > 1 ? path.replace(/\/+$/, "") : path;
  const cut = trimmed.lastIndexOf("/");
  return cut <= 0 ? "/" : trimmed.slice(0, cut);
}

/** The last step of `path`, or `/` for the root. */
export function baseName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? "/";
}

/** Where a typed location leads: a folder to show, and the file in it to select. */
export interface Located {
  dir: string;
  entries: RemoteFile[];
  select: string | null;
}

/**
 * Opens a typed location. A folder is listed; a file shows its folder with
 * the file selected, rather than being listed as if it were a folder.
 */
export async function locate(list: (path: string) => Promise<RemoteFile[]>, path: string): Promise<Located> {
  if (path !== "/") {
    const dir = parentPath(path);
    const name = baseName(path);
    // The parent may not be readable even though the target is.
    const siblings = await list(dir).catch(() => null);
    const hit = siblings?.find((entry) => entry.name === name);
    if (siblings && hit && hit.kind !== "directory") {
      // A symbolic link may point at a folder: try it as one first.
      if (hit.kind === "symlink") {
        const inside = await list(path).catch(() => null);
        if (inside) return { dir: path, entries: inside, select: null };
      }
      return { dir, entries: siblings, select: name };
    }
  }
  return { dir: path, entries: await list(path), select: null };
}
