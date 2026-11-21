import { useCallback, useEffect } from "react";
import { useConfirm } from "@/components/ui/confirm";
import {
  broadcastSnapshot,
  confirmProduction,
  pendingProduction,
  setBroadcastScope,
  toggleGlobalBroadcast,
  toggleTabBroadcast,
  type BroadcastScope,
} from "@/lib/broadcast";
import { nameList } from "@/lib/environment";
import { t } from "@/lib/i18n";

/** The ways to turn broadcast on, each asking once, naming the hosts, when
 *  the scope reaches production terminals. Turning it off never asks. */
export function useBroadcastGuard() {
  const confirm = useConfirm();

  const allowed = useCallback(
    async (tabKey: string, scope: BroadcastScope): Promise<boolean> => {
      const pending = pendingProduction(tabKey, scope);
      if (pending.length === 0) return true;
      const names = [...new Set(pending.map(([, name]) => name))];
      const yes = await confirm({
        title: t.broadcastProdTitle,
        body: t.broadcastProdBody.replace("{hosts}", nameList(names)),
        verb: t.broadcastProdVerb,
      });
      if (yes) confirmProduction(pending.map(([key]) => key));
      return yes;
    },
    [confirm],
  );

  const setScope = useCallback(
    async (tabKey: string, scope: BroadcastScope) => {
      if (await allowed(tabKey, scope)) setBroadcastScope(tabKey, scope);
    },
    [allowed],
  );

  const toggleTab = useCallback(
    async (tabKey: string) => {
      const turningOn = !broadcastSnapshot().tabs.has(tabKey);
      if (!turningOn || (await allowed(tabKey, "tab"))) toggleTabBroadcast(tabKey);
    },
    [allowed],
  );

  const toggleGlobal = useCallback(
    async (tabKey: string) => {
      const turningOn = !broadcastSnapshot().global;
      if (!turningOn || (await allowed(tabKey, "global"))) toggleGlobalBroadcast();
    },
    [allowed],
  );

  return { setScope, toggleTab, toggleGlobal };
}

type Guard = ReturnType<typeof useBroadcastGuard>;
let bridge: Guard | null = null;

/** Rendered inside the confirmation provider, so the keyboard shortcuts
 *  (handled above it) can ask the same way the tab bar does. */
export function BroadcastGuardBridge(): null {
  const guard = useBroadcastGuard();
  useEffect(() => {
    bridge = guard;
    return () => {
      if (bridge === guard) bridge = null;
    };
  }, [guard]);
  return null;
}

export function guardedToggleTab(tabKey: string) {
  void bridge?.toggleTab(tabKey);
}

export function guardedToggleGlobal(tabKey: string) {
  void bridge?.toggleGlobal(tabKey);
}
