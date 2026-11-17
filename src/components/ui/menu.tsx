import { Ellipsis } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Button, type ButtonVariant } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { duration, ease } from "@/lib/motion";
import { cn } from "@/lib/utils";

export type MenuEntry =
  | "separator"
  | {
      key: string;
      label: string;
      icon?: ReactNode;
      /** Shown at the trailing edge (K1: a shortcut is written down somewhere). */
      shortcut?: string;
      /** E4: red text. Destructive items go last, after a separator (O3). */
      danger?: boolean;
      disabled?: boolean;
      onSelect: () => void;
    };

/** The "more" menu of actions.md (S3, O3): a button that opens a list of
 *  actions. Arrow keys move, Enter runs, Esc closes and returns focus to the
 *  button, Tab leaves. Rendered into <body> so scrolling lists do not clip it. */
export function Menu({
  label = t.more,
  icon = <Ellipsis />,
  variant = "quiet",
  size = "icon-sm",
  align = "end",
  items,
  disabled,
  buttonProps,
}: {
  /** The accessible name and tooltip of the button, in the words of the labelled action. */
  label?: string;
  icon?: ReactNode;
  variant?: ButtonVariant;
  size?: "icon" | "icon-sm";
  align?: "start" | "end";
  items: MenuEntry[];
  disabled?: boolean;
  /** Extra attributes for the trigger, such as `tabIndex: -1` inside a roving list (K7). */
  buttonProps?: Record<string, unknown>;
}) {
  const [open, setOpen] = useState(false);
  const [place, setPlace] = useState<{ top: number; left: number } | null>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const id = useId();

  const close = (returnFocus: boolean) => {
    setOpen(false);
    if (returnFocus) buttonRef.current?.focus();
  };

  useLayoutEffect(() => {
    if (!open || !buttonRef.current) return;
    const box = buttonRef.current.getBoundingClientRect();
    setPlace({ top: box.bottom + 6, left: align === "end" ? box.right : box.left });
  }, [open, align]);

  useEffect(() => {
    if (!open) return;
    const outside = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!listRef.current?.contains(target) && !buttonRef.current?.contains(target)) setOpen(false);
    };
    const away = () => setOpen(false);
    document.addEventListener("mousedown", outside);
    window.addEventListener("blur", away);
    window.addEventListener("resize", away);
    return () => {
      document.removeEventListener("mousedown", outside);
      window.removeEventListener("blur", away);
      window.removeEventListener("resize", away);
    };
  }, [open]);

  useEffect(() => {
    if (open && place) listRef.current?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')?.focus();
  }, [open, place]);

  const keys = (event: KeyboardEvent) => {
    const items = [...(listRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    const go = (next: number) => {
      event.preventDefault();
      items[(next + items.length) % items.length]?.focus();
    };
    if (event.key === "ArrowDown") go(index + 1);
    else if (event.key === "ArrowUp") go(index - 1);
    else if (event.key === "Home") go(0);
    else if (event.key === "End") go(items.length - 1);
    else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") close(false);
  };

  return (
    <>
      <Tooltip label={label}>
        <Button
          ref={buttonRef}
          type="button"
          variant={variant}
          size={size}
          aria-label={label}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-controls={open ? id : undefined}
          disabled={disabled}
          {...buttonProps}
          onClick={() => setOpen((current) => !current)}
          onKeyDown={(event: KeyboardEvent) => {
            if (event.key === "ArrowDown" && !open) {
              event.preventDefault();
              setOpen(true);
            }
          }}
        >
          {icon}
        </Button>
      </Tooltip>
      {createPortal(
        <AnimatePresence>
          {open && place && (
            <motion.div
              id={id}
              ref={listRef}
              role="menu"
              aria-label={label}
              onKeyDown={keys}
              style={{ top: place.top, left: place.left }}
              className={cn(
                "fixed z-dropdown w-52 rounded-sm bg-bg-elevated p-1 shadow-2 ring-1 ring-separator",
                align === "end" ? "origin-top-right -translate-x-full" : "origin-top-left",
              )}
              initial={{ opacity: 0 }}
              animate={{ opacity: 1, transition: { duration: duration.micro, ease: ease.standard } }}
              exit={{ opacity: 0, transition: { duration: duration.micro, ease: ease.exit } }}
            >
              {items.map((entry, index) =>
                entry === "separator" ? (
                  <div key={`sep-${index}`} role="separator" className="my-1 h-px bg-separator" />
                ) : (
                  <button
                    key={entry.key}
                    type="button"
                    role="menuitem"
                    disabled={entry.disabled}
                    onClick={() => {
                      close(true);
                      entry.onSelect();
                    }}
                    className={cn(
                      "flex h-8 w-full items-center gap-2 rounded-xs px-2 text-left text-callout hover:bg-fill-1 focus-visible:bg-fill-1 focus-visible:outline-none disabled:opacity-40 [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:stroke-[1.5]",
                      entry.danger ? "text-danger" : "text-label-1 [&_svg]:text-label-2",
                    )}
                  >
                    {entry.icon}
                    <span className="min-w-0 flex-1 truncate">{entry.label}</span>
                    {entry.shortcut && <kbd className="font-sans text-caption text-label-3">{entry.shortcut}</kbd>}
                  </button>
                ),
              )}
            </motion.div>
          )}
        </AnimatePresence>,
        document.body,
      )}
    </>
  );
}
