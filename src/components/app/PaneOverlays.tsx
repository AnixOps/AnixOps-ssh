import { Radio, TriangleAlert } from "lucide-react";
import { motion } from "motion/react";
import { useEffect } from "react";
import { Button } from "@/components/ui/button";
import type { BroadcastScope } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { enter } from "@/lib/motion";

/** Persistent mark on every pane that receives broadcast input: a ring and a
 *  label, so it never relies on colour alone. */
export function BroadcastBadge({ scope }: { scope: Exclude<BroadcastScope, null> }) {
  return (
    <>
      <div aria-hidden className="pointer-events-none absolute inset-0 z-sticky ring-2 ring-warning ring-inset" />
      <div
        role="status"
        className="pointer-events-none absolute bottom-1.5 left-3 z-sticky flex items-center gap-1.5 rounded-xs bg-warning-soft px-2 py-1 text-caption font-medium text-warning ring-1 ring-warning"
      >
        <Radio className="size-3.5" strokeWidth={1.5} />
        {scope === "global" ? t.broadcastingAll : t.broadcastingTab}
      </div>
    </>
  );
}

/** Asked before a paste with a newline goes to several panes at once.
 *  `onChoose(true)` pastes everywhere, `false` only here, `null` cancels. */
export function PasteConfirm({
  count,
  onChoose,
}: {
  count: number;
  onChoose: (all: boolean | null) => void;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      onChoose(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onChoose]);

  return (
    <div className="absolute inset-x-0 top-6 z-dropdown flex justify-center px-4">
      <motion.div
        role="alertdialog"
        aria-label={t.pasteConfirmTitle.replace("{n}", String(count + 1))}
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0, transition: enter }}
        className="flex max-w-xl flex-wrap items-center gap-3 rounded-md bg-bg-elevated px-4 py-3 shadow-3 ring-1 ring-warning"
      >
        <TriangleAlert className="size-5 shrink-0 text-warning" strokeWidth={1.5} />
        <p className="min-w-0 flex-1 text-callout font-medium text-label-1">
          {t.pasteConfirmTitle.replace("{n}", String(count + 1))}
        </p>
        {/* Cancel has the initial focus: sending a newline to several shells is hard to take back (D6). */}
        <Button variant="secondary" size="sm" autoFocus onClick={() => onChoose(null)}>
          {t.cancel}
        </Button>
        <Button variant="secondary" size="sm" onClick={() => onChoose(false)}>
          {t.pasteConfirmHere}
        </Button>
        <Button variant="primary" size="sm" onClick={() => onChoose(true)}>
          {t.pasteConfirmAll}
        </Button>
      </motion.div>
    </div>
  );
}
