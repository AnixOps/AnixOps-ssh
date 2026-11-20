import { useEffect, useRef } from "react";
import { useConfirm } from "@/components/ui/confirm";
import { loadHosts, onDeepLink, type Host } from "@/lib/backend";
import { parseLink, resolveLink } from "@/lib/deepLink";
import { t } from "@/lib/i18n";
import { formatTarget, type Target } from "@/lib/quickConnect";

/** Acts on `anixops-ssh://` links. A saved host opens directly; an address
 *  that is not saved is only opened after the user confirms it by user, host
 *  and port. Renders nothing. */
export function DeepLinkHandler({
  onOpenHost,
  onQuickConnect,
  onError,
}: {
  onOpenHost: (host: Host) => void;
  onQuickConnect: (target: Target) => void;
  onError: (message: string) => void;
}) {
  const confirm = useConfirm();
  const latest = useRef({ confirm, onOpenHost, onQuickConnect, onError });
  latest.current = { confirm, onOpenHost, onQuickConnect, onError };

  useEffect(() => {
    // One link at a time, so two confirmations never replace each other.
    let queue: Promise<void> = Promise.resolve();
    const handle = async (url: string) => {
      const { confirm, onOpenHost, onQuickConnect, onError } = latest.current;
      const request = parseLink(url);
      if (!request) return onError(t.linkInvalid);
      const outcome = resolveLink(request, (await loadHosts()).hosts);
      if (outcome.action === "open") return onOpenHost(outcome.host);
      if (outcome.action === "unknown") return onError(t.linkUnknownHost);
      const { target } = outcome;
      const accepted = await confirm({
        title: t.linkConfirmTitle.replace("{target}", formatTarget(target)),
        body: t.linkConfirmBody
          .replace("{user}", target.user ?? t.linkDefaultUser)
          .replace("{host}", target.host)
          .replace("{port}", String(target.port)),
        verb: t.linkConfirmVerb,
        destructive: false,
      });
      if (accepted) onQuickConnect(target);
    };
    return onDeepLink((urls) => {
      for (const url of urls) {
        queue = queue.then(() => handle(url)).catch((reason) => latest.current.onError(String(reason)));
      }
    });
  }, []);

  return null;
}
