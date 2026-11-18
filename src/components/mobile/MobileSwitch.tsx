import { cn } from "@/lib/utils";

/** The switch of the settings rows, with a 44 x 44 touch target around the
 *  usual 40 x 24 track (layout.md: touch targets are never smaller than 44).
 *  `label` is its accessible name. */
export function MobileSwitch({
  on,
  label,
  onClick,
  disabled,
}: {
  on: boolean;
  label: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
      className="flex size-11 shrink-0 items-center justify-center disabled:opacity-40"
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
