/** Mirrors `LogSettings` in crates/easyssh-sessionlog. */
export interface LogSettings {
  recordByDefault: boolean;
  /** 0 keeps logs forever. */
  retentionDays: number;
  /** Host id to an explicit on or off; a host that is absent follows the default. */
  hosts: Record<string, boolean>;
}

export interface SessionLogEntry {
  id: string;
  host: string;
  startedMs: number;
  durationMs: number;
  size: number;
}

export interface OpenedLog {
  /** Terminal output, escape sequences included. */
  text: string;
  /** False when the log has no authentic final chunk. */
  complete: boolean;
  startedMs: number;
  endedMs: number | null;
}

export const RETENTION_CHOICES = [7, 30, 90, 365, 0];

export type HostChoice = "default" | "on" | "off";

export function hostChoice(settings: LogSettings, hostId: string): HostChoice {
  const value = settings.hosts[hostId];
  return value === undefined ? "default" : value ? "on" : "off";
}

export function choiceValue(choice: HostChoice): boolean | null {
  return choice === "default" ? null : choice === "on";
}

/** Retention choices with the current value included when it is not a standard one. */
export function retentionOptions(current: number): number[] {
  return RETENTION_CHOICES.includes(current)
    ? RETENTION_CHOICES
    : [...RETENTION_CHOICES.filter((d) => d !== 0), current, 0].sort((a, b) => (a || Infinity) - (b || Infinity));
}

export interface DurationUnits {
  sec: string;
  min: string;
  hour: string;
}

const ENGLISH_UNITS: DurationUnits = { sec: "s", min: "min", hour: "h" };

export function formatDuration(ms: number, units: DurationUnits = ENGLISH_UNITS): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds} ${units.sec}`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} ${units.min}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} ${units.hour}` : `${hours} ${units.hour} ${rest} ${units.min}`;
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** A file name for an export: host and start time, safe on every system. */
export function exportName(entry: Pick<SessionLogEntry, "host" | "startedMs">): string {
  const stamp = new Date(entry.startedMs).toISOString().replace(/[:.]/g, "-").slice(0, 19);
  return `${entry.host.replace(/[^\w.-]+/g, "_")}-${stamp}.txt`;
}
