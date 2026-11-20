import { parentPath, resolvePath } from "./remotePath";

/**
 * Where a rename to `typed` leads, like `sftp rename`: a name stays in the
 * folder; text with a `/` is a path, absolute or relative to the folder
 * (`../old/a.txt`, `~/b`); a trailing `/` or `.` / `..` means "into that
 * folder, keeping the name". Null when the text names nothing.
 */
export function renameTarget(typed: string, from: string, cwd: string, home: string): string | null {
  const text = typed.trim();
  if (!text) return null;
  const source = resolvePath(from, cwd, home);
  const name = source.slice(source.lastIndexOf("/") + 1);
  if (!text.includes("/") && text !== "." && text !== "..") {
    const parent = parentPath(source);
    return text === name ? null : parent === "/" ? `/${text}` : `${parent}/${text}`;
  }
  const target = resolvePath(text, parentPath(source), home);
  const intoFolder = text.endsWith("/") || text.endsWith("/.") || text.endsWith("/..") || text === "." || text === "..";
  const result = intoFolder ? resolvePath(`${target}/${name}`, "/", home) : target;
  return result === source ? null : result;
}

/** Whether renaming `from` to `to` moves it to another folder. */
export function isMove(from: string, to: string): boolean {
  return parentPath(from) !== parentPath(to);
}

/**
 * An error for the user that names the operation and the path, unless the
 * server's message already carries the path.
 */
export function describeError(operation: string, path: string, reason: unknown): string {
  const raw = reason instanceof Error ? reason.message : String(reason);
  const detail = raw.replace(/^Error:\s*/, "");
  return detail.includes(path) ? `${operation}: ${detail}` : `${operation} ${path}: ${detail}`;
}
