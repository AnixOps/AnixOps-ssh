/* Whether new host tabs show the server monitor: the last choice, per
   device. Browser storage can be missing (private mode, tests), so every
   access is guarded and the default is "shown". */

const KEY = "anixops-ssh.monitor-open";

export function monitorOpenByDefault(): boolean {
  try {
    return window.localStorage.getItem(KEY) !== "false";
  } catch {
    return true;
  }
}

export function rememberMonitorOpen(open: boolean): void {
  try {
    window.localStorage.setItem(KEY, String(open));
  } catch {
    // Not remembered; the default stays.
  }
}
