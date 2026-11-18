import { ArrowDown, ArrowUp, ChevronLeft, Keyboard, Plus, Repeat2, Trash2 } from "lucide-react";
import { useId, useState } from "react";
import { itemName } from "@/components/mobile/MobileKeyBar";
import { MobileSwitch } from "@/components/mobile/MobileSwitch";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { SettingsGroup } from "@/components/ui/settings";
import {
  addItem,
  addSnippet,
  defaultLayout,
  MAX_LABEL,
  moveItem,
  moveToRow,
  removeItem,
  unusedItems,
  type BarLayout,
  type BarRow,
} from "@/lib/barLayout";
import { t } from "@/lib/i18n";
import { updateMobilePrefs, useMobilePrefs } from "@/lib/mobilePrefs";

/** Edits the two rows of the key bar: reorder, move between rows, remove, add
 *  a built-in key or a command key. Every control is a 44 px button; there is
 *  no drag and drop, which a screen reader and a thumb both handle badly. */
export function MobileBarEditor({ onBack }: { onBack: () => void }) {
  const { bar } = useMobilePrefs();
  const save = (layout: BarLayout) => updateMobilePrefs({ bar: layout });

  return (
    <div role="dialog" aria-modal="true" aria-labelledby="bar-editor-title" className="fixed inset-0 z-drawer flex flex-col bg-bg-grouped">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="flex h-14 items-center gap-1 pr-safe pl-1">
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileSettingsTitle} onClick={onBack}>
            <ChevronLeft />
          </Button>
          <h2 id="bar-editor-title" className="min-w-0 flex-1 truncate text-title-3">
            {t.mobileBarTitle}
          </h2>
        </div>
      </header>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto py-5 pb-safe">
        <RowEditor row="primary" title={t.mobileBarPrimary} bar={bar} save={save} />
        <RowEditor row="extra" title={t.mobileBarExtraRow} bar={bar} save={save} />
        <AddKey bar={bar} save={save} />
        <Button variant="quiet" size="lg" onClick={() => save(defaultLayout())}>
          {t.mobileBarReset}
        </Button>
      </div>
    </div>
  );
}

function RowEditor({ row, title, bar, save }: { row: BarRow; title: string; bar: BarLayout; save: (layout: BarLayout) => void }) {
  const items = bar[row];
  return (
    <SettingsGroup title={title}>
      {items.length === 0 ? (
        <div className="flex flex-col items-center gap-2 px-4 py-6 text-center">
          <Keyboard className="size-8 text-label-3" strokeWidth={1.5} />
          <p className="text-body font-medium">{t.mobileBarEmptyTitle}</p>
          <p className="max-w-xs text-callout text-label-2">{t.mobileBarEmptyHint}</p>
        </div>
      ) : (
        items.map((id, index) => {
          const name = itemName(id, bar);
          return (
            <div key={id} data-bar-item={id} className="flex min-h-11 items-center gap-1 py-1 pr-1 pl-4">
              <span className="min-w-0 flex-1 truncate text-body">{name}</span>
              <Button variant="quiet" size="icon-lg" aria-label={`${t.mobileMoveUp}: ${name}`} disabled={index === 0} onClick={() => save(moveItem(bar, row, index, -1))}>
                <ArrowUp />
              </Button>
              <Button
                variant="quiet"
                size="icon-lg"
                aria-label={`${t.mobileMoveDown}: ${name}`}
                disabled={index === items.length - 1}
                onClick={() => save(moveItem(bar, row, index, 1))}
              >
                <ArrowDown />
              </Button>
              <Button
                variant="quiet"
                size="icon-lg"
                aria-label={`${t.mobileMoveRow}: ${name}`}
                disabled={row === "primary" && items.length <= 1}
                onClick={() => save(moveToRow(bar, row, index))}
              >
                <Repeat2 />
              </Button>
              <Button
                variant="danger"
                size="icon-lg"
                aria-label={`${t.mobileRemoveKey}: ${name}`}
                disabled={row === "primary" && items.length <= 1}
                onClick={() => save(removeItem(bar, row, index))}
              >
                <Trash2 />
              </Button>
            </div>
          );
        })
      )}
    </SettingsGroup>
  );
}

function AddKey({ bar, save }: { bar: BarLayout; save: (layout: BarLayout) => void }) {
  const unused = unusedItems(bar);
  const pickId = useId();
  const [picked, setPicked] = useState("");
  const [row, setRow] = useState<BarRow>("extra");
  const [label, setLabel] = useState("");
  const [text, setText] = useState("");
  const [run, setRun] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const choice = unused.includes(picked) ? picked : (unused[0] ?? "");

  const addSnippetKey = () => {
    const before = bar;
    const next = addSnippet(bar, row, { id: `s${Date.now().toString(36)}`, label, text, run });
    if (next === before) {
      setError(`${t.mobileSnippetLabel} / ${t.mobileSnippetText}: ${t.required}`);
      return;
    }
    setError(null);
    save(next);
    setLabel("");
    setText("");
  };

  return (
    <>
      <SettingsGroup title={t.mobileAddKey}>
        <div className="flex flex-col gap-3 px-4 py-3">
          {unused.length === 0 ? (
            <p className="text-callout text-label-2">{t.mobileBarNothingToAdd}</p>
          ) : (
            <>
              <Select id={pickId} aria-label={t.mobileAddKey} className="h-11" value={choice} onChange={(event) => setPicked(event.target.value)}>
                {unused.map((id) => (
                  <option key={id} value={id}>
                    {itemName(id, bar)}
                  </option>
                ))}
              </Select>
              <Button variant="secondary" size="lg" onClick={() => save(addItem(bar, row, choice))}>
                <Plus />
                {t.mobileAddKey}
              </Button>
            </>
          )}
          <Select aria-label={t.mobileBarExtraRow} className="h-11" value={row} onChange={(event) => setRow(event.target.value as BarRow)}>
            <option value="primary">{t.mobileBarPrimary}</option>
            <option value="extra">{t.mobileBarExtraRow}</option>
          </Select>
        </div>
      </SettingsGroup>
      <SettingsGroup title={t.mobileAddSnippetKey}>
        <div className="flex flex-col gap-4 px-4 py-4">
          {error && <Notice>{error}</Notice>}
          <Field label={t.mobileSnippetLabel}>
            <Input
              className="h-11"
              value={label}
              maxLength={MAX_LABEL}
              onChange={(event) => setLabel(event.target.value)}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
            />
          </Field>
          <Field label={t.mobileSnippetText}>
            <Input
              className="h-11"
              value={text}
              onChange={(event) => setText(event.target.value)}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
            />
          </Field>
          <div className="flex items-center gap-3">
            <span className="min-w-0 flex-1 text-body">{t.mobileSnippetRun}</span>
            <MobileSwitch on={run} label={t.mobileSnippetRun} onToggle={() => setRun(!run)} />
          </div>
          <Button variant="secondary" size="lg" onClick={addSnippetKey}>
            <Plus />
            {t.mobileAddSnippetKey}
          </Button>
        </div>
      </SettingsGroup>
    </>
  );
}
