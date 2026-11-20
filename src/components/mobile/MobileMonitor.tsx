import { X } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { fillTone } from "@/components/app/MonitorPane";
import { Button } from "@/components/ui/button";
import { monitorFailure, monitorSample, type MonitorFailure } from "@/lib/backend";
import { t } from "@/lib/i18n";
import {
  formatBytes,
  formatUptime,
  parseSample,
  shouldSample,
  summarize,
  view as viewOf,
  type MonitorSummary,
  type RawSample,
} from "@/lib/monitor";
import { cn } from "@/lib/utils";

/** Seconds between samples while the card is open and the app is in front. */
export const MOBILE_MONITOR_INTERVAL_MS = 3000;

/** The simplified server monitor of the phone: CPU, memory, the root disk,
 *  network rate, load and uptime, read every 3 s on the terminal's own
 *  connection (never a new one), only while the card is open and the app is
 *  in front. */
export function MobileMonitor({
  hostId,
  connected,
  onClose,
}: {
  hostId: string;
  /** The terminal has a connection. */
  connected: boolean;
  onClose: () => void;
}) {
  const [summary, setSummary] = useState<MonitorSummary | null>(null);
  const [os, setOs] = useState("Linux");
  const [failure, setFailure] = useState<MonitorFailure | null>(null);
  const [foreground, setForeground] = useState(() => document.visibilityState !== "hidden");
  const prev = useRef<RawSample | null>(null);

  useEffect(() => {
    const onVisibility = () => setForeground(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  }, []);

  const running = shouldSample(true, foreground, connected);
  useEffect(() => {
    // Rates restart from nothing after a pause or a new connection.
    prev.current = null;
    if (!running) return;
    let stopped = false;
    let timer: number | undefined;
    const loop = async () => {
      try {
        const sample = await monitorSample(hostId, false);
        if (stopped) return;
        const raw = parseSample(sample.output);
        const next = viewOf(raw, prev.current);
        prev.current = raw;
        setOs(next.os);
        setSummary(summarize(next));
        setFailure(null);
      } catch (reason) {
        if (stopped) return;
        const failed = monitorFailure(reason);
        if (failed.code === "not_connected") prev.current = null;
        setFailure(failed);
      }
      if (!stopped) timer = window.setTimeout(loop, MOBILE_MONITOR_INTERVAL_MS);
    };
    void loop();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [running, hostId]);

  const waiting = !connected || failure?.code === "not_connected";
  const shown = !waiting && os === "Linux" ? summary : null;

  return (
    <section
      aria-label={t.mobileMonitor}
      data-monitor-card
      className="absolute inset-x-3 top-2 z-10 rounded-md bg-bg-elevated p-3 shadow-3 ring-1 ring-separator"
    >
      <div className="flex items-center gap-2">
        <h3 className="mr-auto text-callout font-medium text-label-1">{t.mobileMonitor}</h3>
        <Button variant="quiet" size="icon-lg" aria-label={t.mobileMonitorClose} onClick={onClose} className="-my-2 -mr-2">
          <X />
        </Button>
      </div>
      {waiting ? (
        <p className="py-2 text-callout text-label-2">{t.mobileMonitorConnectFirst}</p>
      ) : os !== "Linux" ? (
        <p className="py-2 text-callout text-label-2">{t.monitorNotLinux.replace("{os}", os)}</p>
      ) : failure ? (
        <p className="selectable py-2 text-callout text-label-2">
          {t.monitorFailed}: {failure.message}
        </p>
      ) : shown ? (
        <Numbers s={shown} />
      ) : (
        <p className="py-2 text-callout text-label-2">{t.monitorWaiting}</p>
      )}
    </section>
  );
}

function Numbers({ s }: { s: MonitorSummary }) {
  return (
    <div className="grid grid-cols-3 gap-x-3 gap-y-3">
      <Meter label={t.monitorCpu} percent={s.cpu} />
      <Meter
        label={t.monitorMemory}
        percent={s.mem}
        detail={s.memTotal > 0 ? formatBytes(s.memTotal) : undefined}
      />
      <Meter
        label={t.monitorDisks}
        percent={s.diskPercent}
        detail={s.disk ? `${s.disk.mount} ${formatBytes(s.disk.available)}` : undefined}
      />
      <Stat label={t.monitorNetwork}>
        <span className="block">↑ {s.tx === null ? "–" : `${formatBytes(s.tx)}/s`}</span>
        <span className="block">↓ {s.rx === null ? "–" : `${formatBytes(s.rx)}/s`}</span>
      </Stat>
      <Stat label={t.monitorLoad}>{s.load.map((value) => value.toFixed(2)).join(" ")}</Stat>
      <Stat label={t.monitorUptime}>{formatUptime(s.uptime, t.monitorDays, t.monitorHours, t.monitorMinutes)}</Stat>
    </div>
  );
}

function Meter({ label, percent, detail }: { label: string; percent: number | null; detail?: string }) {
  const value = percent === null ? null : Math.min(100, Math.max(0, percent));
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : Math.round(value)}
      aria-valuetext={value === null ? undefined : `${Math.round(value)}%${detail ? ` ${detail}` : ""}`}
      className="min-w-0"
    >
      <div className="text-caption text-label-2">{label}</div>
      <div className="font-mono text-body tabular-nums text-label-1">{value === null ? "–" : `${Math.round(value)}%`}</div>
      <div className="mt-1 h-1 overflow-hidden rounded-pill bg-fill-2">
        <div className={cn("h-full rounded-pill", value === null ? "" : fillTone(value))} style={{ width: `${value ?? 0}%` }} />
      </div>
      {detail && <div className="mt-0.5 truncate text-caption text-label-3">{detail}</div>}
    </div>
  );
}

function Stat({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-caption text-label-2">{label}</div>
      <div className="font-mono text-callout tabular-nums text-label-1">{children}</div>
    </div>
  );
}
