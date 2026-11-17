import type { RemoteFile } from "../backend";
import { rank } from "./rank";
import { escapeWord, type Quote } from "./tokenizer";
import type { Candidate } from "./types";

/** The slice of the SFTP backend that completion needs. */
export interface PathSource {
  home: string;
  list(path: string): Promise<RemoteFile[]>;
}

export interface PathLister {
  home(): Promise<string>;
  /** Entries of `dir`, or null when a newer request replaced this one. */
  list(dir: string): Promise<RemoteFile[] | null>;
}

export interface ListerOptions {
  debounceMs?: number;
  ttlMs?: number;
  now?: () => number;
}

/** Debounces and caches directory listings (5 s by default) for one host. */
export function createPathLister(open: () => Promise<PathSource>, options: ListerOptions = {}): PathLister {
  const debounceMs = options.debounceMs ?? 150;
  const ttlMs = options.ttlMs ?? 5000;
  const now = options.now ?? Date.now;
  let source: Promise<PathSource> | null = null;
  const cache = new Map<string, { at: number; entries: RemoteFile[] }>();
  const inflight = new Map<string, Promise<RemoteFile[]>>();
  let latest = 0;

  const connect = () => {
    source ??= open().catch((error) => {
      source = null;
      throw error;
    });
    return source;
  };

  return {
    async home() {
      return (await connect()).home;
    },
    async list(dir) {
      const hit = cache.get(dir);
      if (hit && now() - hit.at < ttlMs) return hit.entries;
      const mine = (latest += 1);
      await new Promise((resolve) => setTimeout(resolve, debounceMs));
      if (mine !== latest) return null;
      let pending = inflight.get(dir);
      if (!pending) {
        pending = connect()
          .then((opened) => opened.list(dir))
          .catch(() => [] as RemoteFile[])
          .finally(() => inflight.delete(dir));
        inflight.set(dir, pending);
      }
      const entries = await pending;
      cache.set(dir, { at: now(), entries });
      return entries;
    },
  };
}

export interface PathRequest {
  /** The word being completed, quotes resolved. */
  text: string;
  quote: Quote;
  foldersOnly: boolean;
  cwd?: string;
}

function openingQuote(quote: Quote): string {
  return quote === "single" ? "'" : quote === "double" ? '"' : quote === "ansi" ? "$'" : "";
}

function closingQuote(quote: Quote): string {
  return quote === "single" || quote === "ansi" ? "'" : quote === "double" ? '"' : "";
}

/** The raw text that stands for `text` as one shell word, in the quote context being typed. */
export function wordInsert(text: string, quote: Quote, close: boolean): string {
  const tilde = text.startsWith("~/") ? "~/" : "";
  return `${openingQuote(quote)}${tilde}${escapeWord(text.slice(tilde.length), quote)}${close ? closingQuote(quote) : ""}`;
}

/**
 * Candidates for a path being typed. Only absolute, `~/` and (when the shell
 * reports a working directory) relative paths can be listed; others give none.
 * Returns null when a newer request superseded this one.
 */
export async function completePaths(lister: PathLister, request: PathRequest): Promise<Candidate[] | null> {
  const { text, quote, foldersOnly, cwd } = request;
  const slash = text.lastIndexOf("/");
  const typedDir = slash >= 0 ? text.slice(0, slash + 1) : "";
  const base = text.slice(slash + 1);
  let listDir: string;
  if (text === "~") {
    return [{ label: "~/", insert: wordInsert("~/", quote, false), kind: "folder", keepOpen: true }];
  } else if (typedDir.startsWith("~/")) {
    const home = (await lister.home()).replace(/\/+$/, "");
    listDir = `${home}/${typedDir.slice(2)}`;
  } else if (typedDir.startsWith("/")) {
    listDir = typedDir;
  } else if (cwd) {
    listDir = `${cwd.replace(/\/+$/, "")}/${typedDir}`;
  } else {
    // Without the shell's working directory a relative path cannot be listed.
    return [];
  }
  const entries = await lister.list(listDir);
  if (entries === null) return null;
  const candidates: Candidate[] = [];
  for (const entry of entries) {
    // Dot files appear only when a dot is typed, and then only dot files.
    if (entry.name.startsWith(".") !== base.startsWith(".")) continue;
    const directory = entry.kind === "directory" || entry.kind === "symlink";
    if (foldersOnly && !directory) continue;
    const isDirectory = entry.kind === "directory";
    candidates.push({
      label: entry.name,
      insert: wordInsert(`${typedDir}${entry.name}${isDirectory ? "/" : ""}`, quote, !isDirectory),
      kind: isDirectory ? "folder" : "file",
      description: isDirectory ? undefined : entry.permissions,
      keepOpen: isDirectory || undefined,
    });
  }
  candidates.sort((a, b) => Number(b.kind === "folder") - Number(a.kind === "folder") || a.label.localeCompare(b.label));
  return rank(candidates, base, 40);
}
