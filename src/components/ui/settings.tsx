import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A settings group (density.md N23): a section title in `--type-title-3-*`
 *  over a grouped list on `--bg-elevated`. `action` is at most one E3 icon (S4). */
export function SettingsGroup({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section>
      <div className="flex h-9 items-center px-1">
        <h3 className="flex-1 text-title-3">{title}</h3>
        {action}
      </div>
      <div className="divide-y divide-separator overflow-hidden rounded-md bg-bg-elevated shadow-1 ring-1 ring-separator">
        {children}
      </div>
    </section>
  );
}

/** One row: a label, an optional hint under it, and one control at the
 *  trailing edge. Comfortable tier: 44 high, body text (N1, N23).
 *
 *  Settings live in a 288 px sidebar, so the row never lets the control squeeze
 *  the label: a label with a hint keeps at least `basis-40`, a lone label its
 *  own width, and the control wraps onto its own line, still at the trailing
 *  edge, when both do not fit. A text
 *  field (`stacked`) always goes under its label at full width, the way
 *  platform settings show an editable value in a narrow column. */
export function SettingsRow({
  label,
  hint,
  stacked = false,
  children,
}: {
  label: string;
  hint?: string;
  stacked?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className={cn("flex min-h-11 px-4 py-2", stacked ? "flex-col gap-2" : "flex-wrap items-center gap-x-3 gap-y-2")}>
      {/* Wrapping is decided on the label's basis: its own width for a lone
          label, 10rem when a hint can wrap under it. */}
      <div className={cn("min-w-0", !stacked && (hint ? "grow basis-40" : "grow"))}>
        <p className={cn("text-body text-label-1", !hint && "truncate")}>{label}</p>
        {hint && <p className="text-callout text-label-2">{hint}</p>}
      </div>
      {children != null && children !== false && (
        <div className={cn("flex min-w-0 items-center", stacked ? "w-full *:w-full" : "ml-auto max-w-full")}>{children}</div>
      )}
    </div>
  );
}
