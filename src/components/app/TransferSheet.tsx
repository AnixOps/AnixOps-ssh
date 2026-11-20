import { ArrowDown, ArrowUp, Check, CircleAlert, LoaderCircle, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useSyncExternalStore } from "react";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { enter, leave } from "@/lib/motion";
import type { TerminalTransfer, TransferView } from "@/lib/terminalTransfer";
import { cn } from "@/lib/utils";

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

function speed(view: TransferView): string {
  const seconds = (Date.now() - view.startedAt) / 1000;
  return seconds >= 1 && view.done > 0 ? `${formatSize(view.done / seconds)}/s` : "";
}

/** The rz/sz progress, a small card over the bottom of the terminal pane. It
 *  mirrors a row of the files pane's transfer list. */
export function TransferSheet({ transfer }: { transfer: TerminalTransfer }) {
  const view = useSyncExternalStore(transfer.subscribe, transfer.snapshot);
  return (
    <AnimatePresence>
      {view && (
        <div className="pointer-events-none absolute inset-x-0 bottom-6 z-sticky flex justify-center px-4">
          <motion.div
            role="status"
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1, transition: enter }}
            exit={{ opacity: 0, y: 8, transition: leave }}
            className="pointer-events-auto w-full max-w-md rounded-md bg-bg-elevated px-4 py-3 shadow-3"
          >
            <Row view={view} transfer={transfer} />
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}

function Row({ view, transfer }: { view: TransferView; transfer: TerminalTransfer }) {
  const running = view.phase === "starting" || view.phase === "running";
  const progress = view.total ? Math.min(1, view.done / view.total) : null;
  const Direction = view.upload ? ArrowUp : ArrowDown;
  const title = view.upload ? t.transferSending : t.transferReceiving;
  const label = running ? t.cancelTransfer : t.transferDismiss;
  return (
    <>
      <div className="flex items-center gap-2 text-callout">
        <Direction className="size-4 shrink-0 text-label-2" strokeWidth={1.5} aria-label={view.upload ? t.upload : t.download} />
        <span className="min-w-0 flex-1 truncate font-medium text-label-1">{view.name || title}</span>
        <Status view={view} />
        <Tooltip label={label}>
          <Button
            variant="quiet"
            size="icon-sm"
            aria-label={label}
            onClick={() => (running ? transfer.cancel() : transfer.dismiss())}
          >
            <X />
          </Button>
        </Tooltip>
      </div>
      {running && progress !== null && (
        <div className="mt-1.5 h-1 overflow-hidden rounded-pill bg-fill-2">
          <div className="h-full origin-left rounded-pill bg-accent-fill" style={{ transform: `scaleX(${progress})` }} />
        </div>
      )}
      {running && (
        <p className="mt-1.5 flex flex-wrap items-center gap-x-3 text-callout text-label-3">
          <span>{title}</span>
          {view.count !== null && view.count > 1 && (
            <span>{t.transferFileOf.replace("{n}", String(view.index)).replace("{total}", String(view.count))}</span>
          )}
          {view.total !== null && (
            <span className="tabular-nums">
              {formatSize(view.done)} / {formatSize(view.total)}
            </span>
          )}
          <span className="tabular-nums">{speed(view)}</span>
          <span className="ml-auto">{t.transferKeysHint}</span>
        </p>
      )}
      {view.phase === "done" && view.path && !view.upload && (
        <p className="selectable mt-1 truncate text-callout text-label-3" title={view.path}>
          {view.path}
        </p>
      )}
      {view.phase === "failed" && view.message && (
        <p className="selectable mt-1 text-callout break-words text-danger">{view.message}</p>
      )}
    </>
  );
}

function Status({ view }: { view: TransferView }) {
  const base = "flex shrink-0 items-center gap-1 text-callout";
  if (view.phase === "starting" || view.phase === "running") {
    return (
      <span className={cn(base, "text-label-2")}>
        <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
        {view.done > 0 ? formatSize(view.done) : ""}
      </span>
    );
  }
  if (view.phase === "done") {
    return (
      <span className={cn(base, "text-success")}>
        <Check className="size-4" strokeWidth={1.5} />
        {t.transferDone}
      </span>
    );
  }
  if (view.phase === "failed") {
    return (
      <span className={cn(base, "text-danger")}>
        <CircleAlert className="size-4" strokeWidth={1.5} />
        {t.transferFailed}
      </span>
    );
  }
  return <span className={cn(base, "text-label-3")}>{t.transferCancelled}</span>;
}
