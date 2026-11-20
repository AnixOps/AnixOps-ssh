import { motion } from "motion/react";
import { useId } from "react";
import { toggle } from "@/lib/motion";
import { cn } from "@/lib/utils";

/** A pill segmented control whose selection slides between options. Given
 *  more width than it needs (a stacked settings row), the options share it. */
export function Segmented<T extends string | boolean>({
  value,
  options,
  onChange,
  label,
  size = "md",
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
  label?: string;
  size?: "sm" | "md";
}) {
  const id = useId();
  return (
    <div role="radiogroup" aria-label={label} className="inline-flex rounded-pill bg-fill-1 p-0.5">
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <button
            key={String(option.value)}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(option.value)}
            className={cn(
              "relative min-w-0 grow whitespace-nowrap rounded-pill transition-colors duration-toggle ease-standard",
              size === "sm" ? "h-6 px-2 text-callout" : "h-7 px-3 text-callout",
              selected ? "text-label-1" : "font-normal text-label-2 hover:text-label-1",
            )}
          >
            {selected && (
              <motion.span
                layoutId={`segmented-${id}`}
                transition={toggle}
                className="absolute inset-0 rounded-pill bg-bg-elevated shadow-1"
              />
            )}
            <span className="relative block truncate" title={option.label}>{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}
