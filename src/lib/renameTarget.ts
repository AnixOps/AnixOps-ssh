import { normalizePath, parentPath, resolvePath } from "./remotePath";

/**
 * Where a rename moves an entry. A plain name renames it inside its folder; a
 * name with `/` moves it: relative to the folder being shown, absolute, or
 * from the home folder with `~`. A path ending in `/` names a folder to move
 * into, keeping the name. Nothing is created: whether the target folder
 * exists is checked by the caller against a listing.
 */
export type RenameTarget =
  | { ok: true; to: string; moves: boolean }
  | { ok: false; reason: "empty" | "invalid" | "same" | "inside" };

const joinPath = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

export function resolveRenameTarget(input: string, from: string, cwd: string, home: string): RenameTarget {
  const typed = input.trim();
  if (!typed) return { ok: false, reason: "empty" };
  const source = normalizePath(from);
  const name = source.split("/").pop() ?? "";
  let to: string;
  if (!typed.includes("/") && !typed.startsWith("~")) {
    if (typed === "." || typed === "..") return { ok: false, reason: "invalid" };
    to = joinPath(parentPath(source), typed);
  } else if (typed === "~") {
    // The home folder itself cannot be replaced by an entry.
    return { ok: false, reason: "invalid" };
  } else {
    const resolved = resolvePath(typed, cwd, home);
    to = typed.endsWith("/") ? joinPath(resolved, name) : resolved;
  }
  if (to === "/" || to === source) return { ok: false, reason: to === source ? "same" : "invalid" };
  if (to.startsWith(`${source}/`)) return { ok: false, reason: "inside" };
  return { ok: true, to, moves: parentPath(to) !== parentPath(source) };
}
