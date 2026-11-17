import { motion } from "motion/react";
import { useEffect, useRef } from "react";
import type { FormEvent, KeyboardEvent, ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";
import { duration, ease, enter, leave } from "@/lib/motion";
import { isMac } from "@/lib/shortcuts";
import { cn } from "@/lib/utils";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** A modal on a dimmed, blurred backdrop, rendered into <body> so frosted
 *  ancestors cannot clip it. Place it inside <AnimatePresence> so it also
 *  animates out. Passing `onSubmit` makes the panel a form.
 *
 *  Keyboard (guidelines/keyboard.md K6, actions.md C6): Tab stays inside the
 *  dialog, Esc dismisses, Mod+S submits a form, and closing returns focus to
 *  the control that opened it. */
export function Overlay({
  children,
  label,
  labelledBy,
  onDismiss,
  onSubmit,
  placement = "center",
  className,
  dismissOnOutside = true,
  role = "dialog",
}: {
  children: ReactNode;
  label?: string;
  labelledBy?: string;
  onDismiss: () => void;
  onSubmit?: () => void;
  placement?: "center" | "top";
  className?: string;
  /** A T2 confirmation closes only by its buttons (D6). */
  dismissOnOutside?: boolean;
  role?: "dialog" | "alertdialog";
}) {
  const panelRef = useRef<HTMLElement>(null);

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    // Autofocus inside the panel wins; otherwise the first control.
    window.requestAnimationFrame(() => {
      if (panel && !panel.contains(document.activeElement)) panel.querySelector<HTMLElement>(FOCUSABLE)?.focus();
    });
    return () => {
      // Back to the opener once the dialog is gone, unless something else took focus.
      window.requestAnimationFrame(() => {
        if (opener?.isConnected && (!document.activeElement || document.activeElement === document.body)) opener.focus();
      });
    };
  }, []);

  const keys = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      onDismiss();
      return;
    }
    if (event.key.toLowerCase() === "s" && (isMac ? event.metaKey : event.ctrlKey) && onSubmit) {
      event.preventDefault();
      onSubmit();
      return;
    }
    if (event.key !== "Tab") return;
    const items = [...(panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])].filter(
      (item) => item.offsetParent !== null,
    );
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  const panel = {
    role,
    "aria-modal": true,
    "aria-label": label,
    "aria-labelledby": labelledBy,
    className: cn("w-full rounded-lg bg-bg-elevated shadow-3", className),
    initial: { opacity: 0, scale: 0.96, y: placement === "top" ? -6 : 8 },
    animate: { opacity: 1, scale: 1, y: 0, transition: enter },
    exit: { opacity: 0, scale: 0.98, y: placement === "top" ? -4 : 4, transition: leave },
    onKeyDown: keys,
  } as const;

  return createPortal(
    <motion.div
      className={cn(
        "fixed inset-0 z-modal flex justify-center bg-(--shadow-color) p-6 backdrop-blur-sm",
        placement === "top" ? "items-start pt-[12vh]" : "items-center",
      )}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1, transition: { duration: duration.overlay, ease: ease.standard } }}
      exit={{ opacity: 0, transition: { duration: duration.toggle, ease: ease.exit } }}
      onMouseDown={(event) => dismissOnOutside && event.target === event.currentTarget && onDismiss()}
    >
      {onSubmit ? (
        <motion.form
          {...panel}
          ref={panelRef as RefObject<HTMLFormElement>}
          onSubmit={(event: FormEvent) => {
            event.preventDefault();
            onSubmit();
          }}
        >
          {children}
        </motion.form>
      ) : (
        <motion.div {...panel} ref={panelRef as RefObject<HTMLDivElement>}>
          {children}
        </motion.div>
      )}
    </motion.div>,
    document.body,
  );
}
