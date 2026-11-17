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

/** One row: a label, an optional one-line hint under it, and one control at the
 *  trailing edge. Comfortable tier: 44 high, body text (N1, N23). */
export function SettingsRow({ label, hint, children }: { label: string; hint?: string; children?: ReactNode }) {
  return (
    <div className="flex min-h-11 items-center gap-3 px-4 py-2">
      <div className="min-w-0 flex-1">
        <p className={cn("text-body text-label-1", !hint && "truncate")}>{label}</p>
        {hint && <p className="text-callout text-label-2">{hint}</p>}
      </div>
      {children}
    </div>
  );
}
