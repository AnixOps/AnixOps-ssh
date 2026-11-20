/* The server monitor: parses what the read-only sample script prints
   (crates/easyssh-engine/src/monitor.rs) and turns two samples' counters into
   rates. Pure, so it is tested without a server. */

export interface CpuTimes {
  /** All jiffies since boot, summed over every CPU. */
  total: number;
  /** Idle and I/O wait jiffies. */
  idle: number;
}

export interface NetCounters {
  name: string;
  rx: number;
  tx: number;
}

export interface Disk {
  mount: string;
  /** Bytes. */
  size: number;
  used: number;
  available: number;
}

/** One process as /proc/<pid>/stat has it. */
export interface ProcStat {
  pid: number;
  name: string;
  /** User + system jiffies. */
  ticks: number;
  /** Resident memory, bytes. */
  rss: number;
}

/** What `ps` and `/proc/<pid>/exe` add for the full process list. */
export interface ProcDetail {
  user: string;
  args: string;
  exe?: string;
}

export interface RawSample {
  os: string;
  hostname: string;
  uptime: number;
  load: [number, number, number];
  cpus: number;
  hz: number;
  cpu: CpuTimes | null;
  mem: { total: number; available: number; swapTotal: number; swapFree: number } | null;
  nets: NetCounters[];
  disks: Disk[];
  procs: ProcStat[];
  /** Present when the sample asked for detail. */
  details: Map<number, ProcDetail> | null;
}

function sections(output: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  let current: string[] | null = null;
  for (const line of output.split("\n")) {
    const match = /^@(\w+)(?: (.*))?$/.exec(line.trimEnd());
    if (match) {
      current = [];
      out.set(match[1], current);
      if (match[2] !== undefined) current.push(match[2]);
      continue;
    }
    current?.push(line);
  }
  return out;
}

const num = (text: string | undefined): number => {
  const value = Number(text);
  return Number.isFinite(value) ? value : 0;
};

const firstLine = (lines: string[] | undefined) => (lines ?? []).find((line) => line.trim() !== "")?.trim() ?? "";

function parseMem(lines: string[]): RawSample["mem"] {
  const kb = new Map<string, number>();
  for (const line of lines) {
    const match = /^(\w+):\s+(\d+)/.exec(line);
    if (match) kb.set(match[1], Number(match[2]) * 1024);
  }
  const total = kb.get("MemTotal");
  if (!total) return null;
  const available =
    kb.get("MemAvailable") ?? (kb.get("MemFree") ?? 0) + (kb.get("Buffers") ?? 0) + (kb.get("Cached") ?? 0);
  return { total, available, swapTotal: kb.get("SwapTotal") ?? 0, swapFree: kb.get("SwapFree") ?? 0 };
}

function parseNet(lines: string[]): NetCounters[] {
  const out: NetCounters[] = [];
  for (const line of lines) {
    const colon = line.indexOf(":");
    if (colon < 0) continue;
    const name = line.slice(0, colon).trim();
    const fields = line.slice(colon + 1).trim().split(/\s+/);
    if (!name || fields.length < 9) continue;
    out.push({ name, rx: num(fields[0]), tx: num(fields[8]) });
  }
  return out;
}

/** Mount points that are noise in a disk list: container layers, snaps,
 *  kernel file systems. */
function hiddenMount(mount: string): boolean {
  return (
    mount.startsWith("/snap/") ||
    mount.startsWith("/proc") ||
    (mount.startsWith("/sys/") && !mount.startsWith("/sys/firmware/efi")) ||
    mount.startsWith("/var/lib/docker/") ||
    mount.startsWith("/var/lib/containers/") ||
    mount.startsWith("/run/containerd/") ||
    mount.startsWith("/run/docker/")
  );
}

function parseDf(lines: string[]): Disk[] {
  const out: Disk[] = [];
  for (const line of lines) {
    const fields = line.trim().split(/\s+/);
    if (fields.length < 6 || !/^\d+$/.test(fields[1])) continue;
    const size = num(fields[1]) * 1024;
    const mount = fields.slice(5).join(" ");
    if (size === 0 || hiddenMount(mount)) continue;
    out.push({ mount, size, used: num(fields[2]) * 1024, available: num(fields[3]) * 1024 });
  }
  return out;
}

function parseProcStats(lines: string[], page: number): ProcStat[] {
  const out: ProcStat[] = [];
  for (const line of lines) {
    // pid (comm) state ...: comm may hold spaces and parentheses.
    const open = line.indexOf("(");
    const close = line.lastIndexOf(")");
    if (open < 0 || close < open) continue;
    const pid = Number(line.slice(0, open).trim());
    if (!Number.isInteger(pid) || pid <= 0) continue;
    const rest = line.slice(close + 2).split(" ");
    // rest[0] is field 3 (state); utime is field 14, stime 15, rss 24.
    const utime = num(rest[11]);
    const stime = num(rest[12]);
    const rss = num(rest[21]) * page;
    out.push({ pid, name: line.slice(open + 1, close), ticks: utime + stime, rss });
  }
  return out;
}

function parseDetails(ps: string[], exe: string[] | undefined): Map<number, ProcDetail> {
  const out = new Map<number, ProcDetail>();
  for (const line of ps) {
    const match = /^\s*(\d+)\s+(\S+)\s+\d+\s?(.*)$/.exec(line);
    if (match) out.set(Number(match[1]), { user: match[2], args: match[3] });
  }
  for (const line of exe ?? []) {
    const match = /\/proc\/(\d+)\/exe -> (.+)$/.exec(line);
    if (!match) continue;
    const pid = Number(match[1]);
    const detail = out.get(pid);
    if (detail) detail.exe = match[2];
  }
  return out;
}

/** Parses one sample. `os` is the only field a non-Linux host fills in. */
export function parseSample(output: string): RawSample {
  const parts = sections(output);
  const load = firstLine(parts.get("loadavg")).split(/\s+/);
  const page = num(firstLine(parts.get("page"))) || 4096;
  const stat = firstLine(parts.get("stat")).split(/\s+/).slice(1).map(num);
  // user nice system idle iowait irq softirq steal (guest is inside user).
  const cpu =
    stat.length >= 4
      ? { total: stat.slice(0, 8).reduce((a, b) => a + b, 0), idle: stat[3] + (stat[4] ?? 0) }
      : null;
  const ps = parts.get("ps");
  return {
    os: firstLine(parts.get("os")),
    hostname: firstLine(parts.get("hostname")),
    uptime: num(firstLine(parts.get("uptime")).split(/\s+/)[0]),
    load: [num(load[0]), num(load[1]), num(load[2])],
    cpus: Math.max(1, num(firstLine(parts.get("nproc")))),
    hz: num(firstLine(parts.get("hz"))) || 100,
    cpu,
    mem: parseMem(parts.get("meminfo") ?? []),
    nets: parseNet(parts.get("netdev") ?? []),
    disks: parseDf(parts.get("df") ?? []),
    procs: parseProcStats(parts.get("pstat") ?? [], page),
    details: ps ? parseDetails(ps, parts.get("exe")) : null,
  };
}

export interface Process {
  pid: number;
  name: string;
  /** Percent of one CPU, as `top` shows it (can pass 100 on many cores). */
  cpu: number;
  rss: number;
  user?: string;
  args?: string;
  exe?: string;
}

export interface NetRate {
  name: string;
  /** Bytes per second. */
  rx: number;
  tx: number;
}

export interface MonitorView {
  os: string;
  hostname: string;
  uptime: number;
  load: [number, number, number];
  cpus: number;
  /** 0 to 100; null until two samples exist. */
  cpu: number | null;
  mem: { total: number; used: number } | null;
  swap: { total: number; used: number } | null;
  /** Per interface; empty until two samples exist. */
  nets: NetRate[];
  disks: Disk[];
  processes: Process[];
}

/** The view of `next`, with rates against `prev` (the sample before). */
export function view(next: RawSample, prev: RawSample | null): MonitorView {
  // Elapsed time from the server's own CPU clock: immune to network delay.
  const cpuDelta = prev?.cpu && next.cpu ? next.cpu.total - prev.cpu.total : 0;
  const seconds = cpuDelta > 0 ? cpuDelta / next.cpus / next.hz : 0;
  const cpu =
    prev?.cpu && next.cpu && cpuDelta > 0
      ? clamp(100 * (1 - (next.cpu.idle - prev.cpu.idle) / cpuDelta), 0, 100)
      : null;

  const before = new Map(prev?.procs.map((proc) => [proc.pid, proc]) ?? []);
  const processes = next.procs.map((proc): Process => {
    const old = before.get(proc.pid);
    const ticks = old && old.name === proc.name ? proc.ticks - old.ticks : 0;
    const detail = next.details?.get(proc.pid);
    return {
      pid: proc.pid,
      name: proc.name,
      cpu: seconds > 0 ? Math.max(0, (100 * ticks) / next.hz / seconds) : 0,
      rss: proc.rss,
      user: detail?.user,
      args: detail?.args,
      exe: detail?.exe,
    };
  });

  const oldNets = new Map(prev?.nets.map((net) => [net.name, net]) ?? []);
  const nets: NetRate[] = [];
  if (seconds > 0) {
    for (const net of next.nets) {
      const old = oldNets.get(net.name);
      if (!old) continue;
      nets.push({
        name: net.name,
        // A counter that went back (wrap or reset) reads as zero, not negative.
        rx: Math.max(0, net.rx - old.rx) / seconds,
        tx: Math.max(0, net.tx - old.tx) / seconds,
      });
    }
  }

  const mem = next.mem;
  return {
    os: next.os,
    hostname: next.hostname,
    uptime: next.uptime,
    load: next.load,
    cpus: next.cpus,
    cpu,
    mem: mem ? { total: mem.total, used: mem.total - mem.available } : null,
    swap: mem ? { total: mem.swapTotal, used: mem.swapTotal - mem.swapFree } : null,
    nets,
    disks: next.disks,
    processes,
  };
}

const clamp = (value: number, low: number, high: number) => Math.min(high, Math.max(low, value));

/** The interface to show first: the busiest one that is not loopback. */
export function defaultInterface(nets: NetCounters[]): string | null {
  const real = nets.filter((net) => net.name !== "lo");
  const pool = real.length > 0 ? real : nets;
  let best: NetCounters | null = null;
  for (const net of pool) if (!best || net.rx + net.tx > best.rx + best.tx) best = net;
  return best?.name ?? null;
}

/** The numbers the phone's monitor card shows. */
export interface MonitorSummary {
  /** 0 to 100; null until two samples exist. */
  cpu: number | null;
  /** 0 to 100. */
  mem: number | null;
  memTotal: number;
  /** The root file system (or the biggest one when there is no `/`). */
  disk: Disk | null;
  /** 0 to 100. */
  diskPercent: number | null;
  /** Bytes per second over all real interfaces; null until two samples exist. */
  rx: number | null;
  tx: number | null;
  load: [number, number, number];
  uptime: number;
}

/** The root disk: `/` when listed, otherwise the largest. */
export function rootDisk(disks: Disk[]): Disk | null {
  const root = disks.find((disk) => disk.mount === "/");
  if (root) return root;
  let best: Disk | null = null;
  for (const disk of disks) if (!best || disk.size > best.size) best = disk;
  return best;
}

/** Reduces a view to the few numbers of the phone card. */
export function summarize(v: MonitorView): MonitorSummary {
  const disk = rootDisk(v.disks);
  const real = v.nets.filter((net) => net.name !== "lo");
  const pool = real.length > 0 ? real : v.nets;
  return {
    cpu: v.cpu,
    mem: v.mem && v.mem.total > 0 ? clamp((100 * v.mem.used) / v.mem.total, 0, 100) : null,
    memTotal: v.mem?.total ?? 0,
    disk,
    diskPercent: disk && disk.size > 0 ? clamp((100 * disk.used) / disk.size, 0, 100) : null,
    rx: pool.length > 0 ? pool.reduce((sum, net) => sum + net.rx, 0) : null,
    tx: pool.length > 0 ? pool.reduce((sum, net) => sum + net.tx, 0) : null,
    load: v.load,
    uptime: v.uptime,
  };
}

/** Whether the card should read the server now: it is open, the app is in
 *  front, and the terminal holds a connection to read through. */
export function shouldSample(open: boolean, foreground: boolean, connected: boolean): boolean {
  return open && foreground && connected;
}

export type ProcessSort = "cpu" | "rss" | "pid" | "user" | "name";

export function sortProcesses(processes: Process[], key: ProcessSort, descending: boolean): Process[] {
  const sign = descending ? -1 : 1;
  const text = (proc: Process) => (key === "user" ? proc.user ?? "" : proc.args || proc.name);
  return [...processes].sort((a, b) => {
    const order =
      key === "user" || key === "name" ? text(a).localeCompare(text(b)) : (a[key] as number) - (b[key] as number);
    return order !== 0 ? sign * order : a.pid - b.pid;
  });
}

/** Keeps the last `limit` points of a series. */
export function pushPoint(series: number[], value: number, limit = 60): number[] {
  const next = [...series, value];
  return next.length > limit ? next.slice(next.length - limit) : next;
}

/** 1.5K, 23.4M, 1.7T: the units `top` and `df -h` use (powers of 1024). */
export function formatBytes(bytes: number): string {
  const units = ["B", "K", "M", "G", "T", "P"];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) return `${Math.round(value)}B`;
  const digits = value >= 100 ? 0 : 1;
  return `${value.toFixed(digits).replace(/\.0$/, "")}${units[unit]}`;
}

/** Uptime the way people read it: 5d 3h, 4h 12m, 7m. */
export function formatUptime(seconds: number, days: string, hours: string, minutes: string): string {
  const total = Math.floor(seconds / 60);
  const d = Math.floor(total / 1440);
  const h = Math.floor((total % 1440) / 60);
  const m = total % 60;
  if (d > 0) return `${d}${days} ${h}${hours}`;
  if (h > 0) return `${h}${hours} ${m}${minutes}`;
  return `${m}${minutes}`;
}
