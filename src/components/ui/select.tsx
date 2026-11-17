import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

/** A native select dressed like <Input>; the platform draws the list. */
export function Select({ className, ...props }: ComponentProps<"select">) {
  return (
    <select
      data-slot="select"
      className={cn(
        "h-9 w-full min-w-0 rounded-sm bg-fill-1 px-3 text-callout text-label-1 outline-none transition-colors duration-micro ease-standard focus-visible:ring-2 focus-visible:ring-(--accent) disabled:opacity-40",
        className,
      )}
      {...props}
    />
  );
}
