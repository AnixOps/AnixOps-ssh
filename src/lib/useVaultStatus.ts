import { useCallback, useEffect, useState } from "react";
import { onVaultChanged, vaultStatus, type VaultStatus } from "@/lib/backend";

/** The vault's state, kept current: read on mount, again whenever the backend
 *  says it locked or unlocked, and on demand through `refresh`. `set` takes a
 *  status an action just returned, so the screen does not wait for the event. */
export function useVaultStatus(): {
  status: VaultStatus | null;
  set: (status: VaultStatus) => void;
  refresh: () => void;
} {
  const [status, set] = useState<VaultStatus | null>(null);
  const refresh = useCallback(() => {
    vaultStatus().then(set).catch(console.error);
  }, []);
  useEffect(() => {
    refresh();
    return onVaultChanged(refresh);
  }, [refresh]);
  return { status, set, refresh };
}
