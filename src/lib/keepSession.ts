import { t } from "./i18n";

/* Keep session (desktop): per host, enter a tmux or screen session on the
   host when the shell opens, so a reconnect lands on the same screen. The
   touch app has the same switch (mobilePrefs.attach); on the desktop it is a
   per-host preference of this computer, kept in local storage. */

export type KeepSession = "off" | "tmux" | "screen";

const KEY = "anixops.keepSession.v1";

function readAll(): Record<string, KeepSession> {
  try {
    const data: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    const out: Record<string, KeepSession> = {};
    if (typeof data === "object" && data !== null) {
      for (const [host, value] of Object.entries(data)) {
        if (value === "tmux" || value === "screen") out[host] = value;
      }
    }
    return out;
  } catch {
    return {};
  }
}

export function hostKeepSession(hostId: string): KeepSession {
  return readAll()[hostId] ?? "off";
}

export function setHostKeepSession(hostId: string, value: KeepSession): void {
  const all = readAll();
  if (value === "off") delete all[hostId];
  else all[hostId] = value;
  try {
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // Storage can be missing or full; the setting then simply is not kept.
  }
}

/** The session name of the `slot`-th terminal open on a host: "anixops" for
 *  the first (the name the touch app uses), "anixops-2" for the second. */
export function keepSessionName(slot: number): string {
  return slot <= 1 ? "anixops" : `anixops-${slot}`;
}

const slots = new Map<string, Set<number>>();

/** Takes the lowest free slot of `hostId`; the name stays with the view
 *  across reconnects. Call the returned function when the view goes away. */
export function claimSlot(hostId: string): { slot: number; release: () => void } {
  let used = slots.get(hostId);
  if (!used) slots.set(hostId, (used = new Set()));
  let slot = 1;
  while (used.has(slot)) slot += 1;
  used.add(slot);
  const taken = used;
  return { slot, release: () => void taken.delete(slot) };
}

/** What is typed into the new shell: a leading space keeps it out of the
 *  history. Without the multiplexer on the host the shell stays plain and says so. */
export function attachCommand(kind: Exclude<KeepSession, "off">, name: string): string {
  const run = kind === "tmux" ? `tmux new -A -s ${name}` : `screen -dRR ${name}`;
  const notice = t.keepSessionMissing.replace("{tool}", kind);
  return ` command -v ${kind} >/dev/null 2>&1 && exec ${run} || echo '${notice.replace(/'/g, "")}'\r`;
}

/** The line written into the terminal when a new connection replaces a lost
 *  one: dim, local time, and whether the old programs are still running. */
export function reconnectDivider(now: Date, kept: KeepSession): string {
  const time = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  const text = (kept === "off" ? t.reconnectDividerNew : t.reconnectDividerKept)
    .replace("{time}", time)
    .replace("{tool}", kept);
  return `\x1b[2m${text}\x1b[0m\r\n`;
}
