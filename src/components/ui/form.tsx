import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A labelled form row: the label is `--type-callout-*` in label-2, the hint
 *  beside it in label-3 (H10). `group` renders a div for button groups, which a
 *  label element would otherwise click when its text is clicked. */
export function Field({
  label,
  hint,
  group = false,
  children,
}: {
  label: string;
  hint?: string;
  group?: boolean;
  children: ReactNode;
}) {
  const Element = group ? "div" : "label";
  return (
    <Element className="block">
      <span className="mb-1.5 flex items-baseline gap-2 text-callout text-label-2">
        {label}
        {hint && <span className="text-label-3">{hint}</span>}
      </span>
      {children}
    </Element>
  );
}

export function Textarea({ className, ...props }: ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "w-full resize-none rounded-sm bg-fill-1 px-3 py-2 text-callout text-label-1 transition-colors duration-micro ease-standard placeholder:text-label-3",
        className,
      )}
      {...props}
    />
  );
}

/** Inline messages (actions.md F2): an error under a field or a section, a
 *  warning or a note, always an icon and words, never colour alone. */
export function Notice({
  tone = "danger",
  children,
  className,
}: {
  tone?: "danger" | "warning" | "info";
  children: ReactNode;
  className?: string;
}) {
  const Icon = tone === "danger" ? CircleAlert : tone === "warning" ? TriangleAlert : Info;
  return (
    <p
      role={tone === "info" ? "status" : "alert"}
      className={cn(
        "flex items-start gap-2 rounded-sm px-3 py-2 text-callout",
        tone === "danger" && "bg-danger-soft text-danger",
        tone === "warning" && "bg-warning-soft text-warning",
        tone === "info" && "bg-fill-1 text-label-2",
        className,
      )}
    >
      <Icon className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} aria-hidden />
      <span className="selectable min-w-0 break-words">{children}</span>
    </p>
  );
}

/** A status pill: a word with a tone, `--type-caption-*` (badges are what
 *  caption is for, N6). */
export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "accent" | "success" | "warning" | "danger";
  children: ReactNode;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center whitespace-nowrap rounded-pill px-2 text-caption",
        tone === "neutral" && "bg-fill-2 text-label-2",
        tone === "accent" && "bg-accent-soft text-(--accent)",
        tone === "success" && "bg-success-soft text-success",
        tone === "warning" && "bg-warning-soft text-warning",
        tone === "danger" && "bg-danger-soft text-danger",
      )}
    >
      {children}
    </span>
  );
}

/** A key cap for a shortcut (K1). */
export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="rounded-xs bg-fill-1 px-1.5 py-0.5 font-sans text-caption text-label-2">{children}</kbd>;
}
