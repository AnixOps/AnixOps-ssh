import { cn } from "@/lib/utils";

/** An on/off switch with a 44 px touch target around the 40 x 24 track
 *  (layout.md: touch targets are never smaller than 44 x 44). */
export function MobileSwitch({ on, label, onToggle }: { on: boolean; label: string; onToggle: () => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={onToggle}
      className="-mr-2 flex h-11 w-14 shrink-0 items-center justify-center"
    >
      <span
        aria-hidden
        className={cn(
          "relative h-6 w-10 rounded-pill transition-colors duration-toggle ease-standard",
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
