import { ArrowDown, ArrowUp, Check, Copy, ListTree, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type PointerEvent, type ReactNode } from "react";
import { ProcessDialog } from "@/components/app/ProcessDialog";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { Segmented } from "@/components/ui/segmented";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import { copyText, monitorFailure, monitorSample, type Host, type MonitorFailure } from "@/lib/backend";
import { t } from "@/lib/i18n";
import {
  defaultInterface,
  formatBytes,
  formatUptime,
  parseSample,
  pushPoint,
  sortProcesses,
  view as viewOf,
  type MonitorView,
  type RawSample,
} from "@/lib/monitor";
import { duration, ease } from "@/lib/motion";
import { cn } from "@/lib/utils";

/** Seconds between samples while the pane is on screen. */
const INTERVAL_MS = 3000;
const HISTORY = 60;
const TOP = 5;

export interface MonitorHistory {
  rx: number[];
  tx: number[];
  latency: (number | null)[];
}

const emptyHistory: MonitorHistory = { rx: [], tx: [], latency: [] };

/** The address people connect to: the host name, or the alias's target. */
export function hostAddress(host: Host): string {
  return host.fields.hostname || host.target;
}

/** The server monitor beside a host's terminal, like the status column of
 *  common SSH clients: address, uptime, load, CPU, memory and swap, the
 *  busiest processes, network and latency over time, and disks. It reads
 *  the server every few seconds on the terminal's own connection, only
 *  while it is on screen. */
export function MonitorPane({
  host,
  visible,
  active,
  onClose,
}: {
  host: Host;
  visible: boolean;
  /** The tab is in front: sample only then. */
  active: boolean;
  onClose: () => void;
}) {
  const [current, setCurrent] = useState<MonitorView | null>(null);
  const [history, setHistory] = useState<MonitorHistory>(emptyHistory);
  const [latency, setLatency] = useState<number | null>(null);
  const [failure, setFailure] = useState<MonitorFailure | null>(null);
  const [iface, setIface] = useState<string | null>(null);
  const [topBy, setTopBy] = useState<"cpu" | "rss">("cpu");
  const [processesOpen, setProcessesOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const prev = useRef<RawSample | null>(null);
  const ifaceRef = useRef<string | null>(null);
  const detailRef = useRef(false);
  detailRef.current = processesOpen;
  const [tick, setTick] = useState(0);

  const sampleNow = useCallback(async (): Promise<boolean> => {
    try {
      const sample = await monitorSample(host.id, detailRef.current);
      const raw = parseSample(sample.output);
      const next = viewOf(raw, prev.current);
      prev.current = raw;
      if (!ifaceRef.current || !raw.nets.some((net) => net.name === ifaceRef.current)) {
        ifaceRef.current = defaultInterface(raw.nets);
        setIface(ifaceRef.current);
      }
      const rate = next.nets.find((net) => net.name === ifaceRef.current);
      setHistory((old) => ({
        rx: rate ? pushPoint(old.rx, rate.rx, HISTORY) : old.rx,
        tx: rate ? pushPoint(old.tx, rate.tx, HISTORY) : old.tx,
        latency: [...old.latency, sample.latency_ms].slice(-HISTORY),
      }));
      setLatency(sample.latency_ms);
      setCurrent(next);
      setFailure(null);
      return true;
    } catch (reason) {
      const failed = monitorFailure(reason);
      setFailure(failed);
      // Without a connection the counters restart from nothing.
      if (failed.code === "not_connected") prev.current = null;
      // OpenSSH hosts never get a connection to read through: stop asking.
      return failed.code !== "wrong_engine";
    }
  }, [host.id]);

  const running = visible && active;
  useEffect(() => {
    if (!running) return;
    let stopped = false;
    let timer: number | undefined;
    const loop = async () => {
      const again = await sampleNow();
      if (!stopped && again) timer = window.setTimeout(loop, INTERVAL_MS);
    };
    void loop();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [running, sampleNow, tick]);

  const openProcesses = () => {
    setProcessesOpen(true);
    // Owners and command lines come with a detailed sample: get one now.
    detailRef.current = true;
    void sampleNow();
  };

  const chooseInterface = (name: string) => {
    ifaceRef.current = name;
    setIface(name);
    // The old line was another interface's.
    setHistory((old) => ({ ...old, rx: [], tx: [] }));
  };

  const copyAddress = () => {
    void copyText(hostAddress(host)).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };

  const rate = current?.nets.find((net) => net.name === iface);
  const top = current ? sortProcesses(current.processes, topBy, true).slice(0, TOP) : [];

  return (
    <motion.aside
      aria-label={t.toggleMonitor}
      aria-hidden={!visible}
      className="material-sidebar flex w-72 shrink-0 flex-col border-r"
      initial={{ opacity: 0 }}
      animate={
        visible
          ? { opacity: 1, display: "flex", transition: { duration: duration.toggle, ease: ease.standard } }
          : { opacity: 0, transitionEnd: { display: "none" }, transition: { duration: duration.micro, ease: ease.exit } }
      }
    >
      <div className="flex h-11 shrink-0 items-center gap-0.5 border-b pr-2 pl-4">
        <h2 className="mr-auto text-title-3">{t.monitor}</h2>
        <IconButton label={t.monitorAllProcesses} onClick={openProcesses} disabled={!current || current.os !== "Linux"}>
          <ListTree />
        </IconButton>
        <IconButton label={t.closeMonitor} onClick={onClose}>
          <X />
        </IconButton>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 pt-3 pb-4 text-callout">
        <div className="flex items-center gap-2">
          <span className="text-label-2">{t.monitorAddress}</span>
          <span className="selectable min-w-0 flex-1 truncate font-mono tabular-nums text-label-1" title={hostAddress(host)}>
            {hostAddress(host)}
          </span>
          <IconButton label={copied ? t.monitorCopied : t.monitorCopyAddress} onClick={copyAddress}>
            {copied ? <Check /> : <Copy />}
          </IconButton>
        </div>

        <Status failure={failure} current={current} onRetry={() => setTick((n) => n + 1)} />

        {current && current.os === "Linux" && (
          <>
            {current.hostname && <Row label={t.monitorHostname}>{current.hostname}</Row>}
            <Row label={t.monitorUptime}>{formatUptime(current.uptime, t.monitorDays, t.monitorHours, t.monitorMinutes)}</Row>
            <Row label={t.monitorLoad}>
              <span className="font-mono tabular-nums">{current.load.map((value) => value.toFixed(2)).join(", ")}</span>
            </Row>

            <div className="mt-3 space-y-2">
              <Meter label={t.monitorCpu} percent={current.cpu} detail={`${current.cpus} ${t.monitorCores}`} />
              <Meter
                label={t.monitorMemory}
                percent={current.mem ? (100 * current.mem.used) / current.mem.total : null}
                detail={current.mem ? `${formatBytes(current.mem.used)}/${formatBytes(current.mem.total)}` : ""}
              />
              {current.swap && current.swap.total > 0 ? (
                <Meter
                  label={t.monitorSwap}
                  percent={(100 * current.swap.used) / current.swap.total}
                  detail={`${formatBytes(current.swap.used)}/${formatBytes(current.swap.total)}`}
                />
              ) : (
                <Row label={t.monitorSwap}>
                  <span className="text-label-3">{t.monitorNoSwap}</span>
                </Row>
              )}
            </div>

            <Section
              title={t.monitorTopProcesses}
              action={
                <Segmented
                  size="sm"
                  label={t.monitorTopProcesses}
                  value={topBy}
                  onChange={setTopBy}
                  options={[
                    { value: "cpu", label: t.monitorByCpu },
                    { value: "rss", label: t.monitorByMemory },
                  ]}
                />
              }
            >
              <table className="w-full table-fixed">
                <thead className="sr-only">
                  <tr>
                    <th>{t.processCpu}</th>
                    <th>{t.processMemory}</th>
                    <th>{t.processCommand}</th>
                  </tr>
                </thead>
                <tbody>
                  {top.map((proc) => (
                    <tr key={proc.pid} className="h-6">
                      <td className="w-12 pr-2 text-right font-mono tabular-nums text-label-2">{proc.cpu.toFixed(1)}%</td>
                      <td className="w-14 pr-2 text-right font-mono tabular-nums text-label-2">{formatBytes(proc.rss)}</td>
                      <td className="truncate text-label-1" title={`${proc.name} (PID ${proc.pid})`}>
                        {proc.name}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <Button variant="quiet" size="sm" className="mt-1 -ml-3" onClick={openProcesses}>
                <ListTree />
                {t.monitorAllProcesses}
              </Button>
            </Section>

            <Section
              title={t.monitorNetwork}
              action={
                current.nets.length > 0 || iface ? (
                  <Select
                    aria-label={t.monitorInterface}
                    className="h-7 w-28 px-2"
                    value={iface ?? ""}
                    onChange={(event) => chooseInterface(event.target.value)}
                  >
                    {(prev.current?.nets ?? []).map((net) => (
                      <option key={net.name} value={net.name}>
                        {net.name}
                      </option>
                    ))}
                  </Select>
                ) : null
              }
            >
              <div className="mb-1 flex gap-4">
                <Legend swatch="bg-(--chart-2)" icon={<ArrowUp />} label={t.monitorUpload} value={rate ? `${formatBytes(rate.tx)}/s` : "–"} />
                <Legend swatch="bg-(--chart-1)" icon={<ArrowDown />} label={t.monitorDownload} value={rate ? `${formatBytes(rate.rx)}/s` : "–"} />
              </div>
              <Chart
                label={t.monitorNetwork}
                series={[
                  { name: t.monitorDownload, stroke: "stroke-(--chart-1)", points: history.rx },
                  { name: t.monitorUpload, stroke: "stroke-(--chart-2)", points: history.tx },
                ]}
                format={(value) => `${formatBytes(value)}/s`}
              />
            </Section>

            <Section
              title={t.monitorLatency}
              action={
                <span className="font-mono tabular-nums text-label-1" title={t.monitorLatencyHint}>
                  {latency === null ? "–" : `${Math.round(latency)} ms`}
                </span>
              }
            >
              <Chart
                label={t.monitorLatency}
                series={[{ name: t.monitorLatency, stroke: "stroke-(--chart-3)", points: history.latency }]}
                format={(value) => `${Math.round(value)} ms`}
              />
            </Section>

            <Section title={t.monitorDisks}>
              <table className="w-full table-fixed">
                <thead>
                  <tr className="text-left text-label-3">
                    <th className="pb-1 font-normal">{t.monitorMount}</th>
                    <th className="w-24 pb-1 text-right font-normal">{t.monitorFreeOfSize}</th>
                  </tr>
                </thead>
                <tbody>
                  {current.disks.map((disk) => {
                    const used = disk.size > 0 ? (100 * disk.used) / disk.size : 0;
                    return (
                      <tr key={disk.mount} title={`${disk.mount}: ${Math.round(used)}%`}>
                        <td className="py-0.5 pr-2">
                          <div className="truncate text-label-1">{disk.mount}</div>
                          <Bar percent={used} />
                        </td>
                        <td className="py-0.5 text-right align-top font-mono tabular-nums text-label-2">
                          {formatBytes(disk.available)}/{formatBytes(disk.size)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </Section>
          </>
        )}
      </div>

      <AnimatePresence>
        {processesOpen && current && (
          <ProcessDialog
            hostId={host.id}
            hostName={host.name}
            current={current}
            onRefresh={() => void sampleNow()}
            onClose={() => setProcessesOpen(false)}
          />
        )}
      </AnimatePresence>
    </motion.aside>
  );
}

function Status({
  failure,
  current,
  onRetry,
}: {
  failure: MonitorFailure | null;
  current: MonitorView | null;
  onRetry: () => void;
}) {
  if (failure?.code === "wrong_engine") {
    return (
      <Notice tone="info" className="mt-3">
        {t.monitorWrongEngine}
      </Notice>
    );
  }
  if (failure?.code === "not_connected") {
    return (
      <Notice tone="info" className="mt-3">
        {t.monitorNotConnected}
      </Notice>
    );
  }
  if (failure) {
    return (
      <div className="mt-3">
        <Notice>
          {t.monitorFailed}: {failure.message}
        </Notice>
        <Button variant="secondary" size="sm" className="mt-2" onClick={onRetry}>
          {t.retry}
        </Button>
      </div>
    );
  }
  if (!current) return <p className="mt-3 text-label-2">{t.monitorWaiting}</p>;
  if (current.os !== "Linux") {
    return (
      <Notice tone="info" className="mt-3">
        {t.monitorNotLinux.replace("{os}", current.os || "?")}
      </Notice>
    );
  }
  return null;
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="mt-1 flex items-baseline gap-2">
      <span className="w-12 shrink-0 text-label-2">{label}</span>
      <span className="selectable min-w-0 flex-1 truncate text-label-1">{children}</span>
    </div>
  );
}

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="mt-4 border-t pt-3">
      <div className="mb-2 flex min-h-7 items-center gap-2">
        <h3 className="mr-auto font-medium text-label-1">{title}</h3>
        {action}
      </div>
      {children}
    </section>
  );
}

/** Tone by how full: words carry the number, colour only adds to it. */
export function fillTone(percent: number): string {
  return percent >= 90 ? "bg-danger" : percent >= 80 ? "bg-warning" : "bg-(--accent)";
}

function Bar({ percent }: { percent: number }) {
  const value = Math.min(100, Math.max(0, percent));
  return (
    <div className="mt-0.5 h-1 overflow-hidden rounded-pill bg-fill-2">
      <div className={cn("h-full rounded-pill transition-[width] duration-toggle", fillTone(value))} style={{ width: `${value}%` }} />
    </div>
  );
}

function Meter({ label, percent, detail }: { label: string; percent: number | null; detail: string }) {
  const value = percent === null ? null : Math.min(100, Math.max(0, percent));
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : Math.round(value)}
      aria-valuetext={value === null ? undefined : `${Math.round(value)}% ${detail}`}
    >
      <div className="flex items-baseline gap-2">
        <span className="w-12 shrink-0 text-label-2">{label}</span>
        <span className="font-mono tabular-nums text-label-1">{value === null ? "–" : `${Math.round(value)}%`}</span>
        <span className="ml-auto font-mono tabular-nums text-label-2">{detail}</span>
      </div>
      <Bar percent={value ?? 0} />
    </div>
  );
}

function Legend({ swatch, icon, label, value }: { swatch: string; icon: ReactNode; label: string; value: string }) {
  return (
    <span className="flex items-center gap-1 text-label-2 [&_svg]:size-4">
      <span aria-hidden className={cn("h-0.5 w-3 rounded-pill", swatch)} />
      {icon}
      <span className="sr-only">{label}</span>
      <span className="font-mono tabular-nums text-label-1">{value}</span>
    </span>
  );
}

interface Series {
  name: string;
  /** A stroke utility on a chart token. */
  stroke: string;
  points: (number | null)[];
}

const CHART_W = 240;
const CHART_H = 56;

/** A small line chart over the last samples, newest at the right, with a
 *  crosshair and the values under the pointer. */
function Chart({ label, series, format }: { label: string; series: Series[]; format: (value: number) => string }) {
  const [hover, setHover] = useState<number | null>(null);
  const length = Math.max(...series.map((line) => line.points.length), 0);
  const values = series.flatMap((line) => line.points.filter((value): value is number => value !== null));
  const max = Math.max(1, ...values) * 1.15;
  // The first points spread over a third of the history so a fresh chart
  // reads as a line; past that the chart fills up to the full history.
  const step = CHART_W / (Math.min(HISTORY, Math.max(length, HISTORY / 3)) - 1);
  const x = (index: number, count: number) => CHART_W - (count - 1 - index) * step;
  const y = (value: number) => CHART_H - 2 - (value / max) * (CHART_H - 4);

  const path = (points: (number | null)[]) => {
    let d = "";
    let pen = false;
    points.forEach((value, index) => {
      if (value === null) {
        pen = false;
        return;
      }
      d += `${pen ? "L" : "M"}${x(index, points.length).toFixed(1)},${y(value).toFixed(1)}`;
      pen = true;
    });
    return d;
  };

  const move = (event: PointerEvent<SVGSVGElement>) => {
    if (length === 0) return;
    const box = event.currentTarget.getBoundingClientRect();
    const px = ((event.clientX - box.left) / box.width) * CHART_W;
    const index = Math.round(length - 1 - (CHART_W - px) / step);
    setHover(index >= 0 && index < length ? index : null);
  };

  const latest = series.map((line) => line.points[line.points.length - 1]);
  const at = hover ?? null;

  return (
    <div className="relative">
      <svg
        role="img"
        aria-label={`${label}: ${series.map((line, i) => `${line.name} ${latest[i] == null ? "–" : format(latest[i] as number)}`).join(", ")}`}
        viewBox={`0 0 ${CHART_W} ${CHART_H}`}
        preserveAspectRatio="none"
        className="block h-14 w-full rounded-sm bg-fill-1"
        onPointerMove={move}
        onPointerLeave={() => setHover(null)}
      >
        <line x1={0} x2={CHART_W} y1={CHART_H / 2} y2={CHART_H / 2} className="stroke-separator" strokeWidth={1} vectorEffect="non-scaling-stroke" />
        {series.map((line) => (
          <path
            key={line.name}
            d={path(line.points)}
            fill="none"
            className={line.stroke}
            strokeWidth={2}
            strokeLinejoin="round"
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
          />
        ))}
        {at !== null && (
          <line
            x1={x(at, length)}
            x2={x(at, length)}
            y1={0}
            y2={CHART_H}
            className="stroke-label-3"
            strokeWidth={1}
            vectorEffect="non-scaling-stroke"
          />
        )}
      </svg>
      <span className="pointer-events-none absolute top-0.5 left-1.5 font-mono tabular-nums text-label-3">{format(max / 1.15)}</span>
      {at !== null && (
        <div className="pointer-events-none absolute right-1 bottom-full mb-1 rounded-xs bg-bg-elevated px-2 py-1 shadow-2">
          {series.map((line) => {
            const value = line.points[at];
            return (
              <div key={line.name} className="flex items-center gap-1.5 whitespace-nowrap">
                <span aria-hidden className={cn("h-0.5 w-3 rounded-pill", line.stroke.replace("stroke-", "bg-"))} />
                <span className="text-label-2">{line.name}</span>
                <span className="font-mono tabular-nums text-label-1">{value == null ? "–" : format(value)}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function IconButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label}>
      <Button variant="quiet" size="icon-sm" aria-label={label} onClick={onClick} disabled={disabled}>
        {children}
      </Button>
    </Tooltip>
  );
}
