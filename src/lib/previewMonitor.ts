/* Monitor samples for the browser preview (`npm run dev`, screenshots): the
   text the sample script would print on a small Linux server, with counters
   that move a little on every call. Deterministic, so screenshots are stable. */

let calls = 0;
const killed = new Set<number>();

const processes: { pid: number; user: string; name: string; rssKb: number; cpu: number; args: string; exe?: string }[] = [
  { pid: 1, user: "root", name: "systemd", rssKb: 12_400, cpu: 0.1, args: "/sbin/init" },
  { pid: 612, user: "root", name: "containerd", rssKb: 78_000, cpu: 6.2, args: "/usr/bin/containerd" },
  { pid: 701, user: "root", name: "dockerd", rssKb: 175_000, cpu: 2.7, args: "/usr/bin/dockerd -H fd://" },
  { pid: 845, user: "postgres", name: "postgres", rssKb: 312_000, cpu: 4.1, args: "postgres: main: checkpointer" },
  { pid: 902, user: "www-data", name: "nginx", rssKb: 9_600, cpu: 0.7, args: "nginx: worker process" },
  { pid: 1204, user: "deploy", name: "node", rssKb: 268_000, cpu: 11.4, args: "node /srv/app/server.js --port 3000", exe: "/usr/bin/node" },
  { pid: 1388, user: "deploy", name: "gunicorn", rssKb: 96_000, cpu: 1.6, args: "gunicorn api.wsgi --workers 4", exe: "/usr/local/bin/python3.12" },
  { pid: 2051, user: "root", name: "sshd", rssKb: 8_200, cpu: 0.2, args: "sshd: deploy [priv]" },
  { pid: 2077, user: "deploy", name: "bash", rssKb: 5_100, cpu: 0, args: "-bash", exe: "/usr/bin/bash" },
  { pid: 2140, user: "deploy", name: "htop", rssKb: 4_300, cpu: 0.3, args: "htop", exe: "/usr/bin/htop" },
  { pid: 3, user: "root", name: "kworker/0:1-events", rssKb: 0, cpu: 0, args: "[kworker/0:1-events]" },
  { pid: 15, user: "root", name: "rcu_preempt", rssKb: 0, cpu: 0.1, args: "[rcu_preempt]" },
];

/** A smooth wobble in [0, 1] so the charts have something to draw. */
const wave = (n: number, phase: number) => 0.5 + 0.5 * Math.sin(n / 3 + phase);

function sampleText(n: number, detail: boolean): string {
  const hz = 100;
  const cpus = 4;
  // Each call is three seconds of server time.
  const elapsed = n * 3 * hz;
  const busy = 0.12 + 0.18 * wave(n, 0);
  const total = 50_000_000 + elapsed * cpus;
  const idle = 44_000_000 + Math.round(elapsed * cpus * (1 - busy));
  const rx = 9_000_000_000 + Math.round(n * 3 * (900_000 + 700_000 * wave(n, 1)));
  const tx = 2_000_000_000 + Math.round(n * 3 * (120_000 + 90_000 * wave(n, 2)));
  const live = processes.filter((proc) => !killed.has(proc.pid));
  const lines = [
    "@os Linux",
    "@hostname",
    "web-01",
    "@uptime",
    `${1_296_000 + n * 3}.42 4000000.00`,
    "@loadavg",
    `${(0.4 + wave(n, 0)).toFixed(2)} 0.62 0.55 2/318 4242`,
    "@nproc",
    String(cpus),
    "@hz",
    String(hz),
    "@page",
    "4096",
    "@stat",
    `cpu  ${total - idle} 0 0 ${idle} 0 0 0 0 0 0`,
    "@meminfo",
    "MemTotal:        8007680 kB",
    "MemFree:          912000 kB",
    `MemAvailable:    ${5_100_000 - Math.round(80_000 * wave(n, 3))} kB`,
    "Buffers:          120000 kB",
    "Cached:          3900000 kB",
    "SwapTotal:       2097148 kB",
    "SwapFree:        1980000 kB",
    "@netdev",
    `    lo: 81234567 40000 0 0 0 0 0 0 81234567 40000 0 0 0 0 0 0`,
    `  eth0: ${rx} 900000 0 0 0 0 0 0 ${tx} 700000 0 0 0 0 0 0`,
    `docker0: 1200000 900 0 0 0 0 0 0 3400000 1200 0 0 0 0 0 0`,
    "@df",
    "Filesystem     1024-blocks      Used Available Capacity Mounted on",
    "/dev/vda1        82437508  31204512  47852000      40% /",
    "/dev/vda15         106858      6186    100672       6% /boot/efi",
    "/dev/vdb1       524288000 402653184 121634816      77% /data",
    "tmpfs              800768      1024    799744       1% /run",
    "tmpfs             4003840         0   4003840       0% /dev/shm",
    "@pstat",
    ...live.map((proc) => {
      const ticks = Math.round((proc.cpu / 100) * n * 3 * hz) + proc.pid;
      return `${proc.pid} (${proc.name}) S 1 ${proc.pid} ${proc.pid} 0 -1 0 0 0 0 0 ${ticks} 0 0 0 20 0 1 0 100 0 ${Math.round(proc.rssKb / 4)} 0`;
    }),
  ];
  if (detail) {
    lines.push(
      "@ps",
      ...live.map((proc) => `${String(proc.pid).padStart(5)} ${proc.user.padEnd(12)} ${proc.rssKb} ${proc.args}`),
      "@exe",
      ...live.map((proc) => `lrwxrwxrwx 1 ${proc.user} ${proc.user} 0 Nov 19 10:00 /proc/${proc.pid}/exe${proc.exe ? ` -> ${proc.exe}` : ""}`),
    );
  }
  lines.push("@end");
  return lines.join("\n");
}

export async function previewMonitorSample(detail: boolean): Promise<{ output: string; latency_ms: number }> {
  calls += 1;
  return { output: sampleText(calls, detail), latency_ms: 38 + Math.round(14 * wave(calls, 4)) };
}

export async function previewMonitorSignal(pid: number): Promise<void> {
  if (pid === 1) throw { code: "signal_failed", message: "kill: (1) - Operation not permitted" };
  killed.add(pid);
}
