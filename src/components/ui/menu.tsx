import { Check, Ellipsis } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { Button, type ButtonVariant } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { duration, ease } from "@/lib/motion";
import { cn } from "@/lib/utils";
import { nativeMenuAllowed } from "@/lib/webviewChrome";

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
      /** A choice of one among several: shown with a check (role menuitemradio). */
      checked?: boolean;
      /** One line under the label that says what the item does. */
      hint?: string;
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
  touch = false,
  text,
  className,
  wide = false,
  tooltip,
  contextRow = false,
}: {
  /** The accessible name and tooltip of the button, in the words of the labelled action. */
  label?: string;
  icon?: ReactNode;
  variant?: ButtonVariant;
  size?: "icon" | "icon-sm" | "icon-lg";
  align?: "start" | "end";
  items: MenuEntry[];
  disabled?: boolean;
  /** Extra attributes for the trigger, such as `tabIndex: -1` inside a roving list (K7). */
  buttonProps?: Record<string, unknown>;
  /** Rows of 44 px for a finger (layout.md), on the phone. */
  touch?: boolean;
  /** A visible word beside the icon: the trigger becomes a text button. */
  text?: ReactNode;
  className?: string;
  /** A wider list, for items with a hint line. */
  wide?: boolean;
  /** The tooltip, when it should say more than the label (also shown while disabled). */
  tooltip?: string;
  /** A right-click on the enclosing `[data-context-row]` opens the same list at
   *  the pointer, as the row's context menu. */
  contextRow?: boolean;
}) {
  const [open, setOpen] = useState(false);
  // Set while the list was opened by a right-click: where, and what had the focus.
  const [pointer, setPointer] = useState<{ x: number; y: number; from: Element | null } | null>(null);
  const [place, setPlace] = useState<{ top: number; left: number; inModal: boolean } | null>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const id = useId();

  const close = (returnFocus: boolean) => {
    setOpen(false);
    if (!returnFocus) return;
    // A right-click menu gives the focus back to where it was, not to the
    // row's (hidden) menu button.
    if (pointer) {
      if (pointer.from instanceof HTMLElement && pointer.from.isConnected) pointer.from.focus();
    } else buttonRef.current?.focus();
  };

  useLayoutEffect(() => {
    if (!open || !buttonRef.current) return;
    // Inside a dialog the list must sit above the dialog's layer.
    const inModal = buttonRef.current.closest('[aria-modal="true"]') !== null;
    if (pointer) return setPlace({ ...pointerPlace(pointer, listRef.current, items.length), inModal });
    const box = buttonRef.current.getBoundingClientRect();
    setPlace({ top: box.bottom + 6, left: align === "end" ? box.right : box.left, inModal });
  }, [open, align, pointer, items.length]);

  useEffect(() => {
    if (!contextRow || disabled) return;
    const row = buttonRef.current?.closest<HTMLElement>("[data-context-row]");
    if (!row) return;
    const onContext = (event: MouseEvent) => {
      if (event.defaultPrevented || nativeMenuAllowed(event)) return;
      // Text selected in the row (an address, a command) gets the Copy menu instead.
      const selection = window.getSelection();
      if (selection?.toString().trim() && row.contains(selection.anchorNode)) return;
      event.preventDefault();
      setPointer({ x: event.clientX, y: event.clientY, from: document.activeElement });
      setOpen(true);
    };
    row.addEventListener("contextmenu", onContext);
    return () => row.removeEventListener("contextmenu", onContext);
  }, [contextRow, disabled]);

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


  const trigger = (
    <Button
      ref={buttonRef}
      type="button"
      variant={variant}
      size={text !== undefined ? "md" : touch ? "icon-lg" : size}
      className={className}
      aria-label={label}
      aria-haspopup="menu"
      aria-expanded={open}
      aria-controls={open ? id : undefined}
      disabled={disabled}
      {...buttonProps}
      onClick={() => {
        setPointer(null);
        setOpen((current) => !current);
      }}
      onKeyDown={(event: KeyboardEvent) => {
        if (event.key === "ArrowDown" && !open) {
          event.preventDefault();
          setPointer(null);
          setOpen(true);
        }
      }}
    >
      {icon}
      {text}
    </Button>
  );

  return (
    <>
      {touch ? (
        trigger
      ) : disabled ? (
        // A disabled button gets no pointer events: the wrapper carries the tooltip.
        <Tooltip label={tooltip ?? label}>
          <span tabIndex={0} className="inline-flex rounded-sm">
            {trigger}
          </span>
        </Tooltip>
      ) : (
        <Tooltip label={tooltip ?? label}>{trigger}</Tooltip>
      )}
      <MenuList
        id={id}
        listRef={listRef}
        label={label}
        place={open ? place : null}
        align={pointer ? "start" : align}
        touch={touch}
        wide={wide}
        items={items}
        onClose={close}
      />
    </>
  );
}

/** The list of a menu, rendered into <body> at `place` (hidden when null). */
function MenuList({
  id,
  listRef,
  label,
  place,
  align,
  touch,
  wide = false,
  items,
  onClose,
}: {
  id: string;
  wide?: boolean;
  listRef: RefObject<HTMLDivElement | null>;
  label: string;
  /** `inModal`: the list must sit above an open dialog's layer. */
  place: { top: number; left: number; inModal?: boolean } | null;
  align: "start" | "end";
  touch: boolean;
  items: MenuEntry[];
  /** `returnFocus`: Escape or a chosen item, as opposed to Tab away. */
  onClose: (returnFocus: boolean) => void;
}) {
  // The list fits its longest item, so its size is known only once it is
  // drawn: then move it back inside the window if it spills over an edge.
  const [nudge, setNudge] = useState({ x: 0, y: 0 });
  useLayoutEffect(() => {
    const box = place ? listRef.current?.getBoundingClientRect() : undefined;
    if (!box) return setNudge({ x: 0, y: 0 });
    const margin = 4;
    const x = box.right > window.innerWidth - margin ? window.innerWidth - margin - box.right : box.left < margin ? margin - box.left : 0;
    const y = box.bottom > window.innerHeight - margin ? Math.max(margin - box.top, window.innerHeight - margin - box.bottom) : 0;
    setNudge({ x, y });
  }, [place, listRef]);

  useEffect(() => {
    if (place) listRef.current?.querySelector<HTMLElement>('[role^="menuitem"]:not([disabled])')?.focus();
  }, [place, listRef]);

  const keys = (event: KeyboardEvent) => {
    const items = [...(listRef.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]:not([disabled])') ?? [])];
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
      onClose(true);
    } else if (event.key === "Tab") onClose(false);
  };

  return createPortal(
    <AnimatePresence>
      {place && (
        <motion.div
          id={id}
          ref={listRef}
          role="menu"
          aria-label={label}
          onKeyDown={keys}
          style={{ top: place.top + nudge.y, left: place.left + nudge.x, zIndex: place.inModal ? "calc(var(--z-modal) + 1)" : undefined }}
          className={cn(
            "fixed z-dropdown rounded-sm bg-bg-elevated p-1 shadow-2 ring-1 ring-separator",
            wide ? "w-80" : touch ? "w-64" : "w-max min-w-52 max-w-80",
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
                role={entry.checked === undefined ? "menuitem" : "menuitemradio"}
                aria-checked={entry.checked}
                disabled={entry.disabled}
                onClick={() => {
                  onClose(true);
                  entry.onSelect();
                }}
                className={cn(
                  "flex w-full items-center rounded-xs text-left text-callout hover:bg-fill-1 focus-visible:bg-fill-1 focus-visible:outline-none disabled:opacity-40 [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:stroke-[1.5]",
                  entry.hint ? "min-h-8 gap-2 px-2 py-1.5" : touch ? "h-11 gap-3 px-3" : "h-8 gap-2 px-2",
                  entry.danger ? "text-danger" : "text-label-1 [&_svg]:text-label-2",
                )}
              >
                {entry.checked !== undefined && (
                  <Check aria-hidden className={cn("self-start", entry.hint && "mt-0.5", !entry.checked && "invisible")} />
                )}
                {entry.icon}
                {entry.hint ? (
                  <span className="min-w-0 flex-1">
                    <span className="block truncate">{entry.label}</span>
                    <span className="block text-label-2">{entry.hint}</span>
                  </span>
                ) : (
                  <span className="min-w-0 flex-1 truncate">{entry.label}</span>
                )}
                {entry.shortcut && <kbd className={cn("font-sans text-caption text-label-3", entry.hint && "self-start mt-0.5")}>{entry.shortcut}</kbd>}
              </button>
            ),
          )}
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}

/** Where a list opened at the pointer goes: up or to the left near the
 *  window's edges, so that it stays on screen (`w-52`, rows of 32 px). */
function pointerPlace(at: { x: number; y: number }, list: HTMLElement | null, count: number) {
  const width = list?.offsetWidth || 208;
  const height = (list?.offsetHeight ?? 0) || count * 32 + 8;
  return {
    left: Math.max(4, Math.min(at.x, window.innerWidth - width - 4)),
    top: at.y + height > window.innerHeight - 4 ? Math.max(4, at.y - height) : at.y,
  };
}

/** The same list opened at the pointer by a right-click (`at`, in viewport
 *  pixels; null hides it). Clicking elsewhere, blurring the window or Escape
 *  closes it; `onClose` then gives focus back to where it came from. */
export function ContextMenu({
  label,
  at,
  items,
  onClose,
}: {
  label: string;
  at: { x: number; y: number } | null;
  items: MenuEntry[];
  onClose: () => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const id = useId();
  const [place, setPlace] = useState<{ top: number; left: number } | null>(null);

  useLayoutEffect(() => {
    setPlace(at ? pointerPlace(at, listRef.current, items.length) : null);
  }, [at, items.length]);

  useEffect(() => {
    if (!at) return;
    const outside = (event: MouseEvent) => {
      if (!listRef.current?.contains(event.target as Node)) onClose();
    };
    document.addEventListener("mousedown", outside);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("mousedown", outside);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [at, onClose]);

  return (
    <MenuList
      id={id}
      listRef={listRef}
      label={label}
      place={place}
      align="start"
      touch={false}
      items={items}
      onClose={onClose}
    />
  );
}
