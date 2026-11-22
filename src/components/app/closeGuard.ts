import { useEffect } from "react";
import { useConfirm } from "@/components/ui/confirm";
import { t } from "@/lib/i18n";
import { terminalFor } from "@/lib/terminals";

/* Closing a tab or pane ends its session. When a command is still running
   there (shell integration reports it) or a full-screen program owns the
   screen, the close asks first, as iTerm2 and Terminal do; an idle shell
   closes at once. */

let ask: ReturnType<typeof useConfirm> | null = null;

/** Lends the confirm dialog to `confirmClose`; render inside ConfirmProvider. */
export function CloseGuardBridge(): null {
  const confirm = useConfirm();
  useEffect(() => {
    ask = confirm;
    return () => {
      if (ask === confirm) ask = null;
    };
  }, [confirm]);
  return null;
}

/** True when the panes may close: nothing runs in them, or the user said so. */
export async function confirmClose(panes: string[]): Promise<boolean> {
  const busy = panes.map((pane) => terminalFor(pane)?.busy() ?? null).filter((what): what is string => what !== null);
  if (busy.length === 0 || !ask) return true;
  return ask({
    title:
      busy.length > 1
        ? t.closeBusyManyTitle.replace("{n}", String(busy.length))
        : busy[0]
          ? t.closeBusyTitle.replace("{what}", busy[0])
          : t.closeBusyProgramTitle,
    body: t.closeBusyBody,
    verb: t.close,
    items: busy.length > 1 ? busy.map((what) => what || t.closeBusyFullScreen) : undefined,
  });
}
