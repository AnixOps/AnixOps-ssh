/** Commands the user ran, per host. Kept in memory and local storage only; never synced. */

export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const STORAGE_KEY = "anixops-ssh:command-history:v1";
const MAX_PER_HOST = 500;
const MAX_LINE = 400;
/** Lines that look like they carry a secret are never remembered. */
const SECRET = /(pass(word|wd)?|secret|token|api[-_]?key|credential)/i;

export function shouldRemember(line: string): boolean {
  if (line.trim() === "" || /^\s/.test(line)) return false;
  if (line.length > MAX_LINE) return false;
  return !SECRET.test(line);
}

export class HistoryStore {
  private hosts = new Map<string, string[]>();
  private loaded = false;

  constructor(
    private readonly storage: KeyValueStore | null,
    private readonly isEnabled: () => boolean = () => true,
  ) {}

  private load() {
    if (this.loaded) return;
    this.loaded = true;
    try {
      const parsed = JSON.parse(this.storage?.getItem(STORAGE_KEY) ?? "{}") as Record<string, unknown>;
      for (const [host, lines] of Object.entries(parsed)) {
        if (Array.isArray(lines)) this.hosts.set(host, lines.filter((line): line is string => typeof line === "string"));
      }
    } catch {
      // Unreadable history is the same as none.
    }
  }

  private save() {
    try {
      this.storage?.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries(this.hosts)));
    } catch {
      // Storage can be full or unavailable; memory still has the history.
    }
  }

  /** Records a line that was run on `host`; the newest comes first. */
  add(host: string, line: string): void {
    if (!this.isEnabled() || !shouldRemember(line)) return;
    this.load();
    const lines = (this.hosts.get(host) ?? []).filter((existing) => existing !== line);
    lines.unshift(line);
    this.hosts.set(host, lines.slice(0, MAX_PER_HOST));
    this.save();
  }

  /** Lines for `host`, newest first. Empty while remembering is turned off. */
  recent(host: string): string[] {
    if (!this.isEnabled()) return [];
    this.load();
    return this.hosts.get(host) ?? [];
  }

  clear(): void {
    this.hosts.clear();
    this.loaded = true;
    try {
      this.storage?.removeItem(STORAGE_KEY);
    } catch {
      // Nothing to remove when storage is unavailable.
    }
  }
}
