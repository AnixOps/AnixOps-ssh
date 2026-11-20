import { ArrowDown, ArrowUp, Check, ChevronRight, Copy, PanelLeftClose, PanelLeftOpen, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { ProcessDialog } from "@/components/app/ProcessDialog";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
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
  summarize,
  view as viewOf,
  type MonitorSummary,
  type MonitorView,
  type RawSample,
} from "@/lib/monitor";
import { MONITOR_WIDTH, monitorTopSort, rememberMonitorTopSort, setMonitorLayout, useMonitorLayout } from "@/lib/monitorPrefs";
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
  const [topBy, setTopBy] = useState<"cpu" | "rss">(monitorTopSort);
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
  const sortTop = (by: "cpu" | "rss") => {
    setTopBy(by);
    rememberMonitorTopSort(by);
  };

  const { width, collapsed } = useMonitorLayout();
  const [dragging, setDragging] = useState(false);
  const linux = current && current.os === "Linux" ? current : null;
  const summary = linux ? summarize(linux) : null;

  return (
    <motion.aside
      aria-label={t.toggleMonitor}
      aria-hidden={!visible}
      className={cn(
        "material-sidebar relative flex shrink-0 flex-col border-r",
        !dragging && "transition-[width] duration-toggle ease-standard",
      )}
      style={{ width: collapsed ? RAIL_WIDTH : width }}
      initial={{ opacity: 0 }}
      animate={
        visible
          ? { opacity: 1, display: "flex", transition: { duration: duration.toggle, ease: ease.standard } }
          : { opacity: 0, transitionEnd: { display: "none" }, transition: { duration: duration.micro, ease: ease.exit } }
      }
    >
      {collapsed ? (
        <Rail summary={summary} swap={linux?.swap && linux.swap.total > 0 ? (100 * linux.swap.used) / linux.swap.total : null} latency={latency} onExpand={() => setMonitorLayout({ collapsed: false })} onClose={onClose} />
      ) : (
        <>
          <div className="flex h-11 shrink-0 items-center gap-0.5 border-b pr-2 pl-4">
            <h2 className="mr-auto text-title-3">{t.monitor}</h2>
            <IconButton label={t.collapseMonitor} onClick={() => setMonitorLayout({ collapsed: true })}>
              <PanelLeftClose />
            </IconButton>
            <IconButton label={t.closeMonitor} onClick={onClose}>
              <X />
            </IconButton>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain text-callout">
            <div className="px-4 py-3">
              <dl className="grid grid-cols-[max-content_minmax(0,1fr)] items-center gap-x-3 gap-y-1.5">
                <dt className="text-label-2">{t.monitorAddress}</dt>
                <dd className="flex min-w-0 items-center gap-1">
                  <MiddleTruncate text={hostAddress(host)} className="selectable font-mono tabular-nums text-label-1" />
                  <IconButton label={copied ? t.monitorCopied : t.monitorCopyAddress} onClick={copyAddress} className="-my-1.5 ml-auto">
                    {copied ? <Check /> : <Copy />}
                  </IconButton>
                </dd>
                {linux && (
                  <>
                    <dt className="text-label-2">{t.monitorHostname}</dt>
                    <dd className="min-w-0">
                      {linux.hostname ? (
                        <MiddleTruncate text={linux.hostname} className="selectable font-mono text-label-1" />
                      ) : (
                        <Absent />
                      )}
                    </dd>
                    <dt className="text-label-2">{t.monitorUptime}</dt>
                    <dd className="truncate text-label-1">
                      {formatUptime(linux.uptime, t.monitorDays, t.monitorHours, t.monitorMinutes)}
                    </dd>
                    <dt className="text-label-2">{t.monitorLoad}</dt>
                    <dd className="selectable truncate font-mono tabular-nums text-label-1" title={t.monitorLoadHint}>
                      {linux.load.map((value) => value.toFixed(2)).join(" · ")}
                    </dd>
                  </>
                )}
              </dl>

              <Status failure={failure} current={current} onRetry={() => setTick((n) => n + 1)} />
            </div>

            {linux && (
              <>
                <Section>
                  <div className="grid grid-cols-[max-content_max-content_minmax(0,1fr)] items-baseline gap-x-3">
                    <Meter label={t.monitorCpu} percent={linux.cpu} detail={`${linux.cpus} ${t.monitorCores}`} />
                    <Meter
                      label={t.monitorMemory}
                      percent={linux.mem ? (100 * linux.mem.used) / linux.mem.total : null}
                      detail={linux.mem ? `${formatBytes(linux.mem.used)} / ${formatBytes(linux.mem.total)}` : ""}
                    />
                    {linux.swap && linux.swap.total > 0 ? (
                      <Meter
                        label={t.monitorSwap}
                        percent={(100 * linux.swap.used) / linux.swap.total}
                        detail={`${formatBytes(linux.swap.used)} / ${formatBytes(linux.swap.total)}`}
                      />
                    ) : (
                      <>
                        <span className="text-label-2">{t.monitorSwap}</span>
                        <span className="col-span-2 text-label-3">{t.monitorNoSwap}</span>
                      </>
                    )}
                  </div>
                </Section>

                <Section title={t.monitorTopProcesses}>
                  <table className="w-full table-fixed">
                    <thead>
                      <tr className="text-label-2">
                        <th className="h-7 text-left font-normal">{t.monitorProcessName}</th>
                        <SortHeader className="w-16" label={t.processCpu} active={topBy === "cpu"} onClick={() => sortTop("cpu")} />
                        <SortHeader className="w-18" label={t.processMemory} active={topBy === "rss"} onClick={() => sortTop("rss")} />
                      </tr>
                    </thead>
                    <tbody>
                      {top.map((proc) => (
                        <tr key={proc.pid} className="h-7 border-t">
                          <td className="truncate pr-2 text-label-1" title={`${proc.name} (PID ${proc.pid})`}>
                            {proc.name}
                          </td>
                          <td className="text-right font-mono tabular-nums text-label-1">{proc.cpu.toFixed(1)}%</td>
                          <td className="text-right font-mono tabular-nums text-label-2">{formatBytes(proc.rss)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <Button variant="quiet" size="sm" className="mt-1 -ml-2.5 text-(--accent)" onClick={openProcesses}>
                    {t.monitorAllProcesses}
                    <ChevronRight />
                  </Button>
                </Section>

                <Section
                  title={t.monitorNetwork}
                  action={
                    linux.nets.length > 0 || iface ? (
                      <Select
                        aria-label={t.monitorInterface}
                        className="h-7 w-auto max-w-32 min-w-0 py-0 pr-7 pl-2 font-mono text-callout"
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
                  <div className="mb-2 grid grid-cols-2 gap-3">
                    <Legend swatch="bg-(--chart-2)" icon={<ArrowUp />} label={t.monitorUpload} value={rate ? `${formatBytes(rate.tx)}/s` : null} />
                    <Legend swatch="bg-(--chart-1)" icon={<ArrowDown />} label={t.monitorDownload} value={rate ? `${formatBytes(rate.rx)}/s` : null} />
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
                      {latency === null ? <Absent /> : `${Math.round(latency)} ms`}
                    </span>
                  }
                >
                  <Chart
                    label={t.monitorLatency}
                    series={[{ name: t.monitorLatency, stroke: "stroke-(--chart-3)", points: history.latency }]}
                    format={(value) => `${Math.round(value)} ms`}
                  />
                </Section>

                <Section title={t.monitorDisks} action={<span className="text-label-2">{t.monitorFreeOfSize}</span>}>
                  <ul className="space-y-2.5">
                    {linux.disks.map((disk) => {
                      const used = disk.size > 0 ? (100 * disk.used) / disk.size : 0;
                      return (
                        <li key={disk.mount} title={`${disk.mount} · ${Math.round(used)}%`}>
                          <div className="flex items-baseline gap-3">
                            <MiddleTruncate text={disk.mount} className="selectable min-w-0 flex-1 font-mono text-label-1" />
                            <span className="shrink-0 font-mono tabular-nums text-label-2">
                              {formatBytes(disk.available)} / {formatBytes(disk.size)}
                            </span>
                          </div>
                          <Bar percent={used} />
                        </li>
                      );
                    })}
                  </ul>
                </Section>
              </>
            )}
          </div>
          <ResizeHandle width={width} onDragging={setDragging} />
        </>
      )}

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

/** Width of the folded monitor: one icon column, like the app's own rail. */
const RAIL_WIDTH = 52;

/** The folded monitor: the few numbers worth a glance, top to bottom. The
 *  whole column unfolds it. */
function Rail({
  summary,
  swap,
  latency,
  onExpand,
  onClose,
}: {
  summary: MonitorSummary | null;
  /** Swap in use, 0 to 100; null without swap. */
  swap: number | null;
  latency: number | null;
  onExpand: () => void;
  onClose: () => void;
}) {
  return (
    <>
      <div className="flex h-11 shrink-0 items-center justify-center border-b">
        <IconButton label={t.expandMonitor} onClick={onExpand} side="right">
          <PanelLeftOpen />
        </IconButton>
      </div>
      <button
        type="button"
        aria-label={t.expandMonitor}
        onClick={onExpand}
        className="flex min-h-0 flex-1 flex-col items-stretch gap-3 overflow-hidden px-1.5 py-3 outline-none hover:bg-fill-1 focus-visible:bg-fill-1"
      >
        <RailStat label={t.monitorCpu} percent={summary?.cpu ?? null} />
        <RailStat label={t.monitorMemory} percent={summary?.mem ?? null} />
        {swap !== null && <RailStat label={t.monitorSwap} percent={swap} />}
        <RailStat label={t.monitorRoot} short={t.monitorDisks} percent={summary?.diskPercent ?? null} />
        <div className="text-center">
          <div className="truncate text-caption text-label-2">{t.monitorLatency}</div>
          <div className="font-mono text-callout tabular-nums text-label-1">
            {latency === null ? <Absent /> : Math.round(latency)}
          </div>
          {latency !== null && <div className="text-caption text-label-3">ms</div>}
        </div>
      </button>
      <div className="flex shrink-0 justify-center border-t py-1.5">
        <IconButton label={t.closeMonitor} onClick={onClose} side="right">
          <X />
        </IconButton>
      </div>
    </>
  );
}

function RailStat({ label, short, percent }: { label: string; short?: string; percent: number | null }) {
  const value = percent === null ? null : Math.min(100, Math.max(0, percent));
  return (
    <div className="text-center" title={value === null ? label : `${label} ${Math.round(value)}%`}>
      <div className="truncate text-caption text-label-2">{short ?? label}</div>
      <div className="font-mono text-callout tabular-nums text-label-1">{value === null ? <Absent /> : `${Math.round(value)}%`}</div>
      <Bar percent={value ?? 0} />
    </div>
  );
}

/** The monitor's trailing edge: drag to resize, double-click for the
 *  default width, arrows from the keyboard. Dragging far past the
 *  narrowest width folds it into the rail. */
function ResizeHandle({ width, onDragging }: { width: number; onDragging: (dragging: boolean) => void }) {
  const start = useRef<{ x: number; width: number } | null>(null);
  const key = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? 64 : 16;
    if (event.key === "ArrowLeft") setMonitorLayout({ width: width - step });
    else if (event.key === "ArrowRight") setMonitorLayout({ width: width + step });
    else if (event.key === "Home") setMonitorLayout({ width: MONITOR_WIDTH.min });
    else if (event.key === "End") setMonitorLayout({ width: MONITOR_WIDTH.max });
    else return;
    event.preventDefault();
  };
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-label={t.resizeMonitor}
      aria-orientation="vertical"
      aria-valuenow={width}
      aria-valuemin={MONITOR_WIDTH.min}
      aria-valuemax={MONITOR_WIDTH.max}
      onPointerDown={(event) => {
        start.current = { x: event.clientX, width };
        event.currentTarget.setPointerCapture(event.pointerId);
        event.preventDefault();
        onDragging(true);
      }}
      onPointerMove={(event) => {
        if (!start.current) return;
        const next = start.current.width + event.clientX - start.current.x;
        if (next < MONITOR_WIDTH.min - COLLAPSE_PAST) {
          start.current = null;
          onDragging(false);
          setMonitorLayout({ collapsed: true });
          return;
        }
        setMonitorLayout({ width: next });
      }}
      onPointerUp={(event) => {
        start.current = null;
        onDragging(false);
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onPointerCancel={() => {
        start.current = null;
        onDragging(false);
      }}
      onDoubleClick={() => setMonitorLayout({ width: MONITOR_WIDTH.default })}
      onKeyDown={key}
      className="group/resize absolute inset-y-0 -right-1.5 z-sticky flex w-3 cursor-col-resize justify-center outline-none"
    >
      <span className="h-full w-px bg-transparent transition-colors duration-micro group-hover/resize:w-0.5 group-hover/resize:bg-(--accent) group-focus-visible/resize:w-0.5 group-focus-visible/resize:bg-(--accent)" />
    </div>
  );
}

/** How far past the narrowest width a drag goes before it folds the monitor. */
const COLLAPSE_PAST = 96;

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


/** A placeholder for a value that is not there (density.md N13). */
function Absent() {
  return <span className="text-label-3">—</span>;
}

/** Hosts and paths cut in the middle so their end stays readable
 *  (density.md N12); the whole value is in the tooltip and copies whole. */
function MiddleTruncate({ text, className }: { text: string; className?: string }) {
  const tail = 10;
  if (text.length <= tail + 6) {
    return (
      <span className={cn("min-w-0 truncate", className)} title={text}>
        {text}
      </span>
    );
  }
  return (
    <span className={cn("flex min-w-0", className)} title={text}>
      <span className="min-w-0 truncate">{text.slice(0, -tail)}</span>
      <span className="shrink-0 whitespace-pre">{text.slice(-tail)}</span>
    </span>
  );
}

function Section({ title, action, children }: { title?: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="border-t px-4 py-3">
      {title && (
        <div className="mb-2 flex min-h-7 items-center gap-2">
          <h3 className="mr-auto font-semibold text-label-1">{title}</h3>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

function SortHeader({ label, active, onClick, className }: { label: string; active: boolean; onClick: () => void; className?: string }) {
  return (
    <th className={cn("h-7 text-right font-normal", className)} aria-sort={active ? "descending" : "none"}>
      <button
        type="button"
        onClick={onClick}
        title={t.processSort}
        className={cn(
          "inline-flex items-center gap-0.5 rounded-xs outline-none hover:text-label-1 focus-visible:text-label-1",
          active ? "font-medium text-label-1" : "text-label-2",
        )}
      >
        {active && <ArrowDown className="size-3.5" />}
        {label}
      </button>
    </th>
  );
}

/** Tone by how full: words carry the number, colour only adds to it. */
export function fillTone(percent: number): string {
  return percent >= 90 ? "bg-danger" : percent >= 80 ? "bg-warning" : "bg-(--accent)";
}

function Bar({ percent }: { percent: number }) {
  const value = Math.min(100, Math.max(0, percent));
  return (
    <div className="mt-1 h-1 overflow-hidden rounded-pill bg-fill-2">
      <div className={cn("h-full rounded-pill transition-[width] duration-toggle", fillTone(value))} style={{ width: `${value}%` }} />
    </div>
  );
}

/** One row of the resource grid: label, percent, detail, and the bar under
 *  all three. The grid lines the labels and numbers up across rows. */
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
      className="col-span-3 grid grid-cols-subgrid items-baseline [&+&]:mt-2.5 [&+span]:mt-2.5 [&+span+span]:mt-2.5"
    >
      <span className="text-label-2">{label}</span>
      <span className="text-right font-mono tabular-nums text-label-1">{value === null ? <Absent /> : `${Math.round(value)}%`}</span>
      <span className="truncate text-right font-mono tabular-nums text-label-2">{detail}</span>
      <div className="col-span-3">
        <Bar percent={value ?? 0} />
      </div>
    </div>
  );
}

function Legend({ swatch, icon, label, value }: { swatch: string; icon: ReactNode; label: string; value: string | null }) {
  return (
    <span className="flex min-w-0 items-center gap-1.5 text-label-2 [&_svg]:size-3.5">
      <span aria-hidden className={cn("h-0.5 w-3 shrink-0 rounded-pill", swatch)} />
      {icon}
      <span className="sr-only">{label}</span>
      <span className="truncate font-mono tabular-nums text-label-1">{value ?? <Absent />}</span>
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
  side,
  className,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  side?: "top" | "right" | "bottom" | "left";
  className?: string;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label} side={side}>
      <Button variant="quiet" size="icon-sm" aria-label={label} onClick={onClick} disabled={disabled} className={className}>
        {children}
      </Button>
    </Tooltip>
  );
}
