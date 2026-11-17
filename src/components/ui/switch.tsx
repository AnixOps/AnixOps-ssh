import { cn } from "@/lib/utils";

/** An on/off switch; `label` is its accessible name. */
export function Switch({ on, label, onClick }: { on: boolean; label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={onClick}
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
    </button>
  );
}
