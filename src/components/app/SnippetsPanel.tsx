import { ClipboardPaste, CornerDownLeft, Pencil, Plus, Radio, SquareCode, Trash2 } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice, Textarea } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Menu } from "@/components/ui/menu";
import { Switch } from "@/components/ui/switch";
import { Tooltip } from "@/components/ui/tooltip";
import { deleteSnippet, loadSnippets, saveSnippet, type Snippet } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { appear } from "@/lib/motion";
import { targetsFor, toggleGlobalBroadcast, useBroadcast } from "@/lib/broadcast";
import { terminalFor } from "@/lib/terminals";

type Draft = { id: string | null; name: string; content: string };

/** Saved commands. Click to run in the active terminal, or broadcast to
 *  every connected terminal. */
export function SnippetsPanel({ activeKey }: { activeKey: string | null }) {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<Draft | null>(null);
  // The same switch as "all panes in all tabs" in the tab bar.
  const broadcast = useBroadcast().global;
  const [error, setError] = useState<string | null>(null);
  const confirm = useConfirm();

  const reload = useCallback(() => {
    loadSnippets()
      .then(setSnippets)
      .catch((reason) => setError(String(reason)));
  }, []);
  useEffect(reload, [reload]);

  const send = (snippet: Snippet, run: boolean) => {
    const source = terminalFor(activeKey);
    // Panes in the active pane's broadcast scope get it too; the active pane goes last, so it keeps the focus.
    const others = activeKey ? targetsFor(activeKey, (id) => terminalFor(id)?.connected() ?? false) : [];
    const targets = [...others.map((id) => terminalFor(id)), source].filter((target) => target !== undefined);
    if (!source) {
      setError(t.noTerminal);
      return;
    }
    setError(null);
    for (const target of targets) target.send(snippet.content, run);
  };

  const save = async () => {
    if (!draft) return;
    try {
      await saveSnippet(draft.id, draft.name, draft.content);
      setDraft(null);
      setError(null);
      reload();
    } catch (reason) {
      setError(String(reason));
    }
  };

  const remove = async (snippet: Snippet) => {
    const title = t.snippetDeleteTitle.replace("{name}", snippet.name);
    if (!(await confirm({ title, body: t.snippetDeleteBody, verb: t.delete }))) return;
    try {
      await deleteSnippet(snippet.id);
      reload();
    } catch (reason) {
      setError(String(reason));
    }
  };

  const normalized = query.trim().toLowerCase();
  const visible = snippets.filter(
    (snippet) =>
      snippet.name.toLowerCase().includes(normalized) || snippet.content.toLowerCase().includes(normalized),
  );

  return (
    <aside className="material-sidebar flex w-72 shrink-0 flex-col border-r">
      <div className="flex items-center gap-1.5 px-3 pt-3 pb-2">
        <Input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t.searchSnippets}
          aria-label={t.searchSnippets}
          className="min-w-0 flex-1"
        />
        <Tooltip label={t.newSnippet}>
          <Button
            variant="secondary"
            size="icon"
            aria-label={t.newSnippet}
            onClick={() => setDraft({ id: null, name: "", content: "" })}
          >
            <Plus />
          </Button>
        </Tooltip>
      </div>

      {error && <Notice className="mx-3 mb-2">{error}</Notice>}

      <div className="min-h-0 flex-1 space-y-1 overflow-y-auto px-2 pb-3">
        <AnimatePresence initial={false}>
        {draft && <DraftEditor key="draft" draft={draft} onChange={setDraft} onSave={() => void save()} onCancel={() => setDraft(null)} />}
        {snippets.length === 0 && !draft && (
          <div key="empty" className="px-3 pt-10 text-center">
            <SquareCode className="mx-auto mb-3 size-12 text-label-3" strokeWidth={1.5} />
            <p className="text-callout font-medium text-label-1">{t.noSnippets}</p>
            <p className="mt-1 text-callout text-label-2">{t.noSnippetsHint}</p>
          </div>
        )}
        {visible.map((snippet) =>
          draft?.id === snippet.id ? null : (
            <motion.div
              key={snippet.id}
              {...appear}
              data-context-row
              className="group relative rounded-sm px-2 py-2 hover:bg-fill-1"
            >
              <button
                type="button"
                onClick={() => send(snippet, true)}
                className="block w-full pr-16 text-left"
                aria-label={`${t.runSnippet}: ${snippet.name}`}
              >
                <span className="block truncate text-callout font-medium text-label-1">{snippet.name}</span>
                <code className="mt-0.5 line-clamp-2 font-mono text-callout break-words text-label-2">
                  {snippet.content}
                </code>
              </button>
              <span className="absolute top-1.5 right-1.5 flex opacity-0 transition-opacity duration-micro group-focus-within:opacity-100 group-hover:opacity-100">
                <SmallButton label={t.pasteSnippet} onClick={() => send(snippet, false)}>
                  <ClipboardPaste />
                </SmallButton>
                <Menu
                  label={t.more}
                  contextRow
                  items={[
                    { key: "edit", label: t.edit, icon: <Pencil />, onSelect: () => setDraft({ ...snippet }) },
                    "separator",
                    { key: "delete", label: t.delete, icon: <Trash2 />, danger: true, onSelect: () => void remove(snippet) },
                  ]}
                />
              </span>
            </motion.div>
          ),
        )}
        </AnimatePresence>
      </div>

      <div className="border-t px-3 py-2.5">
        <div className="flex items-center gap-2 px-2 py-1 text-callout text-label-1">
          <Radio className="size-4 shrink-0 text-label-2" strokeWidth={1.5} />
          <span className="min-w-0 flex-1">{t.broadcastAll}</span>
          <Switch on={broadcast} label={t.broadcastAll} onClick={toggleGlobalBroadcast} />
        </div>
        <p className="mt-1.5 flex items-center gap-1 px-2 text-callout text-label-3">
          <CornerDownLeft className="size-4 shrink-0" strokeWidth={1.5} />
          {t.snippetHint}
        </p>
      </div>
    </aside>
  );
}

function DraftEditor({
  draft,
  onChange,
  onSave,
  onCancel,
}: {
  draft: Draft;
  onChange: (draft: Draft) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return (
    <motion.form
      key="snippet-draft"
      {...appear}
      className="space-y-2 rounded-sm bg-bg-elevated p-2 shadow-1 ring-1 ring-separator"
      onSubmit={(event) => {
        event.preventDefault();
        onSave();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onCancel();
        }
      }}
    >
      <Input
        autoFocus
        value={draft.name}
        onChange={(event) => onChange({ ...draft, name: event.target.value })}
        placeholder={t.snippetName}
        aria-label={t.snippetName}
        className="h-8"
      />
      <Textarea
        value={draft.content}
        onChange={(event) => onChange({ ...draft, content: event.target.value })}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) onSave();
        }}
        placeholder="df -h"
        aria-label={t.snippetCommand}
        rows={4}
        spellCheck={false}
        className="resize-y font-mono"
      />
      <div className="flex justify-end gap-1.5">
        <Button type="button" variant="quiet" size="sm" onClick={onCancel}>
          {t.cancel}
        </Button>
        <Button type="submit" variant="primary" size="sm">
          {t.save}
        </Button>
      </div>
    </motion.form>
  );
}

function SmallButton({ label, onClick, children }: { label: string; onClick: () => void; children: ReactNode }) {
  return (
    <Tooltip label={label}>
      <Button variant="quiet" size="icon-sm" aria-label={label} onClick={onClick}>
        {children}
      </Button>
    </Tooltip>
  );
}
