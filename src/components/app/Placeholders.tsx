import { Info, SquareTerminal, TriangleAlert } from "lucide-react";
import mark from "@/assets/mark.svg";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/form";
import type { SshStatus } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { shortcutHint } from "@/lib/shortcuts";

export function EmptyWorkspace({ onOpenLocal, status }: { onOpenLocal: () => void; status: SshStatus | null }) {
  const shortcuts = [
    { label: t.shortcutSearch, keys: shortcutHint("search-hosts") },
    { label: t.shortcutLocal, keys: shortcutHint("new-local") },
    { label: t.shortcutClose, keys: shortcutHint("close-tab") },
    { label: t.shortcutList, keys: shortcutHint("shortcut-list") },
  ];
  return (
    <div className="flex h-full flex-col items-center justify-center p-8 text-center">
      <img src={mark} alt="AnixOps SSH" className="mb-6 size-16" draggable={false} />
      <h1 className="text-title-2">{t.emptyTitle}</h1>
      <p className="mt-2 max-w-sm text-callout text-label-2">{t.emptyHint}</p>
      {status && !status.sshVersion && (
        <div
          role="alert"
          className="mt-6 flex max-w-md items-start gap-3 rounded-md bg-warning-soft px-4 py-3 text-left"
        >
          <TriangleAlert className="mt-0.5 size-5 shrink-0 text-warning" strokeWidth={1.5} />
          <div>
            <p className="text-callout font-medium text-label-1">{t.sshMissingTitle}</p>
            <p className="mt-1 text-callout text-label-2">{t.sshMissingHint}</p>
          </div>
        </div>
      )}
      {status?.sshVersion && status.agent === "unavailable" && (
        <p className="mt-4 flex max-w-md items-start gap-2 text-left text-callout text-label-2">
          <Info className="mt-px size-4 shrink-0" strokeWidth={1.5} />
          {t.agentHint}
        </p>
      )}
      <Button variant="primary" className="mt-6" onClick={onOpenLocal}>
        <SquareTerminal />
        {t.openLocal}
      </Button>
      <dl className="mt-10 grid grid-cols-[auto_auto] gap-x-6 gap-y-2 text-callout">
        {shortcuts.map((shortcut) => (
          <div key={shortcut.label} className="contents">
            <dt className="text-right text-label-2">{shortcut.label}</dt>
            <dd className="text-left">
              <Kbd>{shortcut.keys}</Kbd>
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
