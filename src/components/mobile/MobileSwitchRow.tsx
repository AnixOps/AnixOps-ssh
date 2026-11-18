import { cn } from "@/lib/utils";

/** A row with a label and an on/off switch, whole row being the 44 px target
 *  (layout.md): the switch itself is only 24 px tall. `label` is the accessible
 *  name; `hint` is read with it. */
export function MobileSwitchRow({
  label,
  hint,
  on,
  onChange,
  disabled,
  className,
}: {
  label: string;
  hint?: string;
  on: boolean;
  onChange: (on: boolean) => void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={hint ? `${label}. ${hint}` : label}
      disabled={disabled}
      onClick={() => onChange(!on)}
      className={cn(
        "flex min-h-11 w-full items-center justify-between gap-3 text-left disabled:opacity-40",
        className,
      )}
    >
      <span className="min-w-0">
        <span className="block text-body text-label-1">{label}</span>
        {hint && <span className="block text-callout text-label-2">{hint}</span>}
      </span>
      <span
        aria-hidden
        className={cn(
          "relative h-6 w-10 shrink-0 rounded-pill transition-colors duration-toggle ease-standard",
          on ? "bg-accent-fill" : "bg-fill-3",
        )}
      >
        <span
          className={cn(
            "absolute top-0.5 left-0.5 size-5 rounded-full bg-(--on-accent) shadow-1 transition-transform duration-toggle ease-emphasized",
            on && "translate-x-4",
          )}
        />
      </span>
    </button>
  );
}
