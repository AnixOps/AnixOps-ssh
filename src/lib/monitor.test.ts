import { describe, expect, it } from "vitest";
import {
  defaultInterface,
  formatBytes,
  formatUptime,
  parseSample,
  pushPoint,
  rootDisk,
  shouldSample,
  sortProcesses,
  summarize,
  view,
} from "./monitor";

/** What the sample script prints, trimmed to a few lines per section. */
function sample({ cpu, idle, rx, tx, ticks }: { cpu: number; idle: number; rx: number; tx: number; ticks: number }) {
  return [
    "@os Linux",
    "@hostname",
    "web-01",
    "@uptime",
    "16020.58 60000.00",
    "@loadavg",
    "0.04 0.18 0.26 1/310 4242",
    "@nproc",
    "2",
    "@hz",
    "100",
    "@page",
    "4096",
    "@stat",
    `cpu  ${cpu - idle} 0 0 ${idle} 0 0 0 0 0 0`,
    "@meminfo",
    "MemTotal:        8000000 kB",
    "MemFree:         1000000 kB",
    "MemAvailable:    6000000 kB",
    "Buffers:           10000 kB",
    "Cached:          2000000 kB",
    "SwapTotal:       2000000 kB",
    "SwapFree:        1500000 kB",
    "@netdev",
    `    lo: 999999 10 0 0 0 0 0 0 999999 10 0 0 0 0 0 0`,
    `  eth0: ${rx} 100 0 0 0 0 0 0 ${tx} 80 0 0 0 0 0 0`,
    "@df",
    "Filesystem     1024-blocks     Used Available Capacity Mounted on",
    "/dev/sda1        41152736 20576368  20576368      50% /",
    "tmpfs              409600        0    409600       0% /run/user/1000",
    "/dev/loop3          65536    65536         0     100% /snap/core/123",
    "/dev/sdb1         1048576   524288    524288      50% /mnt/My Disk",
    "@pstat",
    `1 (systemd) S 0 1 1 0 -1 4194560 0 0 0 0 ${ticks} 0 0 0 20 0 1 0 1 0 2000 18446744073709551615`,
    `42 (Web Content (x)) S 1 42 42 0 -1 0 0 0 0 0 10 10 0 0 20 0 1 0 1 0 500 0`,
    "@ps",
    "    1 root          8000 /sbin/init splash",
    "   42 alice         2000 /usr/lib/firefox/firefox -contentproc",
    "@exe",
    "lrwxrwxrwx 1 root root 0 Nov 19 10:00 /proc/1/exe",
    "lrwxrwxrwx 1 alice alice 0 Nov 19 10:00 /proc/42/exe -> /usr/lib/firefox/firefox",
    "@end",
  ].join("\n");
}

const first = parseSample(sample({ cpu: 10000, idle: 9000, rx: 1000, tx: 500, ticks: 100 }));
// 2 CPUs at 100 Hz: 400 jiffies is 2 seconds.
const second = parseSample(sample({ cpu: 10400, idle: 9300, rx: 5000, tx: 2500, ticks: 150 }));

describe("parseSample", () => {
  it("reads the system lines", () => {
    expect(first.os).toBe("Linux");
    expect(first.hostname).toBe("web-01");
    expect(first.uptime).toBeCloseTo(16020.58);
    expect(first.load).toEqual([0.04, 0.18, 0.26]);
    expect(first.cpus).toBe(2);
    expect(first.mem).toEqual({
      total: 8000000 * 1024,
      available: 6000000 * 1024,
      swapTotal: 2000000 * 1024,
      swapFree: 1500000 * 1024,
    });
  });

  it("lists real disks only (no tmpfs), mount points with spaces included", () => {
    expect(first.disks.map((disk) => disk.mount)).toEqual(["/", "/mnt/My Disk"]);
    expect(first.disks[0]).toEqual({ mount: "/", size: 41152736 * 1024, used: 20576368 * 1024, available: 20576368 * 1024 });
  });

  it("reads process names with spaces and parentheses", () => {
    expect(first.procs.map((proc) => proc.name)).toEqual(["systemd", "Web Content (x)"]);
    expect(first.procs[0].rss).toBe(2000 * 4096);
    expect(first.procs[1].ticks).toBe(20);
  });

  it("joins owners, command lines and executables", () => {
    expect(first.details?.get(42)).toEqual({
      user: "alice",
      args: "/usr/lib/firefox/firefox -contentproc",
      exe: "/usr/lib/firefox/firefox",
    });
    // Another user's executable is not readable: no location.
    expect(first.details?.get(1)?.exe).toBeUndefined();
  });

  it("leaves everything empty for a host that is not Linux", () => {
    const other = parseSample("@os Darwin\n");
    expect(other.os).toBe("Darwin");
    expect(other.cpu).toBeNull();
    expect(other.mem).toBeNull();
    expect(other.procs).toEqual([]);
    expect(other.details).toBeNull();
  });
});

describe("view", () => {
  it("has no rates from a single sample", () => {
    const only = view(first, null);
    expect(only.cpu).toBeNull();
    expect(only.nets).toEqual([]);
    expect(only.processes[0].cpu).toBe(0);
  });

  it("turns counters into rates over the server's own clock", () => {
    const next = view(second, first);
    expect(next.cpu).toBeCloseTo(25); // 300 idle of 400
    expect(next.mem).toEqual({ total: 8000000 * 1024, used: 2000000 * 1024 });
    expect(next.swap).toEqual({ total: 2000000 * 1024, used: 500000 * 1024 });
    const eth0 = next.nets.find((net) => net.name === "eth0");
    expect(eth0).toEqual({ name: "eth0", rx: 2000, tx: 1000 });
    // 50 ticks in 2 s at 100 Hz: a quarter of one CPU.
    expect(next.processes.find((proc) => proc.pid === 1)?.cpu).toBeCloseTo(25);
  });

  it("reads a counter that went back as zero", () => {
    const reset = parseSample(sample({ cpu: 10400, idle: 9300, rx: 10, tx: 10, ticks: 150 }));
    expect(view(reset, first).nets.find((net) => net.name === "eth0")).toEqual({ name: "eth0", rx: 0, tx: 0 });
  });
});

describe("helpers", () => {
  it("picks the busiest interface that is not loopback", () => {
    expect(defaultInterface(first.nets)).toBe("eth0");
    expect(defaultInterface([{ name: "lo", rx: 1, tx: 1 }])).toBe("lo");
    expect(defaultInterface([])).toBeNull();
  });

  it("sorts processes by any column, ties by pid", () => {
    const processes = view(second, first).processes;
    expect(sortProcesses(processes, "cpu", true).map((proc) => proc.pid)).toEqual([1, 42]);
    expect(sortProcesses(processes, "rss", false).map((proc) => proc.pid)).toEqual([42, 1]);
    expect(sortProcesses(processes, "user", false).map((proc) => proc.user)).toEqual(["alice", "root"]);
  });

  it("keeps a bounded history", () => {
    expect(pushPoint([1, 2, 3], 4, 3)).toEqual([2, 3, 4]);
  });

  it("formats sizes and uptime", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(121.2 * 1024 ** 3)).toBe("121 GB");
    expect(formatBytes(4.8 * 1024 ** 3)).toBe("4.8 GB");
    expect(formatUptime(4 * 3600 + 44 * 60, "天", "小时", "分")).toBe("4小时 44分");
    expect(formatUptime(3 * 86400 + 7200, "d", "h", "m")).toBe("3d 2h");
  });
});

describe("the phone summary", () => {
  const disks = [
    { mount: "/data", size: 500, used: 400, available: 100 },
    { mount: "/", size: 100, used: 25, available: 75 },
  ];

  it("takes the root disk, or the biggest one without it", () => {
    expect(rootDisk(disks)?.mount).toBe("/");
    expect(rootDisk([disks[0], { mount: "/run", size: 5, used: 1, available: 4 }])?.mount).toBe("/data");
    expect(rootDisk([])).toBeNull();
  });

  it("reduces a view to percentages and summed real interfaces", () => {
    const first = parseSample(sample({ cpu: 1000, idle: 900, rx: 1000, tx: 500, ticks: 0 }));
    const second = parseSample(sample({ cpu: 1200, idle: 1000, rx: 3000, tx: 1500, ticks: 0 }));
    const summary = summarize({ ...view(second, first), disks });
    expect(summary.cpu).toBeCloseTo(50);
    expect(summary.mem).toBeCloseTo(25);
    expect(summary.diskPercent).toBe(25);
    expect(summary.rx).toBeGreaterThan(0);
    expect(summary.tx).toBeGreaterThan(0);
    expect(summary.load[0]).toBeCloseTo(0.04);
    expect(summary.uptime).toBeCloseTo(16020.58);
  });

  it("has no rates or CPU from a single sample", () => {
    const summary = summarize(view(parseSample(sample({ cpu: 1000, idle: 900, rx: 1, tx: 1, ticks: 0 })), null));
    expect(summary.cpu).toBeNull();
    expect(summary.rx).toBeNull();
  });
});

describe("when the phone card reads the server", () => {
  it("needs the card open, the app in front and a connection", () => {
    expect(shouldSample(true, true, true)).toBe(true);
    expect(shouldSample(false, true, true)).toBe(false);
    expect(shouldSample(true, false, true)).toBe(false);
    expect(shouldSample(true, true, false)).toBe(false);
  });
});
