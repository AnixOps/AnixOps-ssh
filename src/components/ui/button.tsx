import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

/* The four emphasis levels of guidelines/actions.md, wrapped once. Screens use
   <Button> and never style a button by hand (A6, section 9).

     E1  primary   the one thing the view is for; at most one per view (A1)
     E2  secondary a real alternative to the primary
     E3  quiet     frequent but minor: text in the accent, icons in label-2
     E4  danger    red text; `confirm` is the filled red button and belongs
                   only inside a confirmation dialog (ConfirmDialog)

   One shape (pill) and the control heights of layout.md: lg 44 (touch, used by
   the mobile screens), md 36, sm 28 (inside tables, rows and compact bars).
   Size never adds a level (A8). */
const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-pill font-medium transition-[color,background-color,box-shadow,transform] duration-micro ease-standard active:scale-[0.97] disabled:pointer-events-none disabled:opacity-40 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:stroke-[1.5]",
  {
    variants: {
      variant: {
        primary: "bg-accent-fill text-on-accent hover:bg-accent-fill-hover",
        secondary: "bg-fill-1 text-label-1 hover:bg-fill-2",
        quiet: "text-(--accent) hover:bg-fill-1",
        danger: "text-danger hover:bg-danger-soft",
        confirm: "bg-danger-fill text-on-danger hover:bg-danger-fill-hover",
      },
      size: {
        lg: "h-11 px-5 text-body",
        md: "h-9 px-4 text-callout",
        sm: "h-7 px-3 text-callout",
        "icon-lg": "size-11",
        icon: "size-9",
        "icon-sm": "size-7",
      },
    },
    compoundVariants: [
      // A3: an icon-only button is always E3, and its icon rests in label-2.
      { variant: "quiet", size: ["icon", "icon-sm", "icon-lg"], className: "text-label-2 hover:text-label-1" },
    ],
    defaultVariants: { variant: "secondary", size: "md" },
  },
);

export type ButtonVariant = NonNullable<VariantProps<typeof buttonVariants>["variant"]>;

export function Button({
  className,
  variant,
  size,
  asChild = false,
  ...props
}: ComponentProps<"button"> & VariantProps<typeof buttonVariants> & { asChild?: boolean }) {
  const Component = asChild ? Slot : "button";
  return (
    <Component
      data-slot="button"
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  );
}
