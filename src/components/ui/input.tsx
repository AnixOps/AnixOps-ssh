import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

export function Input({ className, ...props }: ComponentProps<"input">) {
  return (
    <input
      data-slot="input"
      className={cn(
        "h-9 w-full min-w-0 rounded-sm bg-fill-1 px-3 text-callout text-label-1 transition-colors duration-micro ease-standard placeholder:text-label-3",
        className,
      )}
      {...props}
    />
  );
}
