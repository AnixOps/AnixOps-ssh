import { X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/form";
import { Overlay } from "@/components/ui/overlay";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { isMac } from "@/lib/shortcuts";
import { shortcutTable, type ShortcutRow } from "@/lib/shortcutTable";

const groups: { id: ShortcutRow["group"]; title: () => string }[] = [
  { id: "app", title: () => t.shortcutsApp },
  { id: "terminal", title: () => t.shortcutsTerminal },
  { id: "list", title: () => t.shortcutsList },
];

/** The list of every shortcut, opened with Mod+/ (keyboard.md K9). It reads the
 *  same table the code and the tooltips use. */
export function ShortcutList({ onClose }: { onClose: () => void }) {
  return (
    <Overlay labelledBy="shortcut-list-title" onDismiss={onClose} className="flex max-h-full max-w-xl flex-col">
      <div className="flex items-center gap-2 px-6 pt-6">
        <h2 id="shortcut-list-title" className="flex-1 text-title-3">
          {t.shortcuts}
        </h2>
        <Tooltip label={`${t.close} (Esc)`}>
          <Button variant="quiet" size="icon-sm" aria-label={t.close} onClick={onClose}>
            <X />
          </Button>
        </Tooltip>
      </div>
      <div className="min-h-0 flex-1 space-y-6 overflow-y-auto px-6 pt-4 pb-6">
        {groups.map((group) => (
          <section key={group.id} aria-label={group.title()}>
            <h3 className="mb-2 text-callout text-label-2">{group.title()}</h3>
            <dl className="divide-y divide-separator">
              {shortcutTable
                .filter((row) => row.group === group.id)
                .map((row) => (
                  <div key={`${row.group}-${row.id}`} className="flex items-center gap-3 py-2 text-callout">
                    <dt className="min-w-0 flex-1 truncate text-label-1">{row.title()}</dt>
                    <dd className="flex shrink-0 items-center gap-2">
                      <Kbd>{isMac ? row.mac : row.pc}</Kbd>
                      {!isMac && row.pcInTerminal && (
                        <span className="flex items-center gap-1 text-label-3">
                          {t.shortcutsInTerminal}
                          <Kbd>{row.pcInTerminal}</Kbd>
                        </span>
                      )}
                    </dd>
                  </div>
                ))}
            </dl>
          </section>
        ))}
      </div>
    </Overlay>
  );
}
