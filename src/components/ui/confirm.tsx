import { AnimatePresence } from "motion/react";
import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import { t } from "@/lib/i18n";

export interface ConfirmOptions {
  /** The question, naming the object: "删除主机 prod-db？" (D2, C5). */
  title: string;
  /** The consequence in one sentence. */
  body?: string;
  /** The verb on the confirming button; repeats the verb of the title, never "OK" (D2). */
  verb: string;
  /** T1 confirms; T2 also asks the user to type this word or name first (D5). */
  typed?: string;
  /** Names of what is affected, for a bulk action (D7). */
  items?: string[];
  /** False for a confirmation that is not destructive (sign out, export): the button is E1, not red. */
  destructive?: boolean;
}

type Ask = (options: ConfirmOptions) => Promise<boolean>;
const ConfirmContext = createContext<Ask>(() => Promise.resolve(false));

/** `const confirm = useConfirm(); if (await confirm({...})) remove();`
 *  Every destructive action in the app goes through here: the dialog names
 *  the thing, Cancel has the initial focus and answers Enter and Esc, and the
 *  filled red button repeats the verb (actions.md D2, D6). */
export function useConfirm(): Ask {
  return useContext(ConfirmContext);
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<ConfirmOptions | null>(null);
  const resolver = useRef<((answer: boolean) => void) | null>(null);

  const ask = useCallback<Ask>((options) => {
    resolver.current?.(false);
    setPending(options);
    return new Promise<boolean>((resolve) => {
      resolver.current = resolve;
    });
  }, []);

  const answer = (value: boolean) => {
    resolver.current?.(value);
    resolver.current = null;
    setPending(null);
  };

  return (
    <ConfirmContext.Provider value={ask}>
      {children}
      <AnimatePresence>
        {pending && <ConfirmDialog options={pending} onAnswer={answer} />}
      </AnimatePresence>
    </ConfirmContext.Provider>
  );
}

function ConfirmDialog({ options, onAnswer }: { options: ConfirmOptions; onAnswer: (confirmed: boolean) => void }) {
  const [typed, setTyped] = useState("");
  const ready = options.typed === undefined || typed === options.typed;
  const shown = options.items?.slice(0, 3) ?? [];
  const more = (options.items?.length ?? 0) - shown.length;
  return (
    <Overlay
      role="alertdialog"
      labelledBy="confirm-title"
      onDismiss={() => onAnswer(false)}
      dismissOnOutside={options.typed === undefined}
      className="max-w-md p-6"
    >
      <h2 id="confirm-title" className="text-title-3">
        {options.title}
      </h2>
      {options.body && <p className="mt-2 text-callout text-label-2">{options.body}</p>}
      {shown.length > 0 && (
        <p className="selectable mt-2 truncate text-callout text-label-1">
          {shown.join(t.listSeparator)}
          {more > 0 ? t.confirmMore.replace("{n}", String(options.items?.length ?? 0)) : ""}
        </p>
      )}
      {options.typed !== undefined && (
        <label className="mt-4 block text-callout text-label-2">
          {t.confirmTypePrompt.replace("{word}", options.typed)}
          <Input
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
            autoComplete="off"
            spellCheck={false}
            className="mt-1.5 font-mono"
          />
        </label>
      )}
      <div className="mt-6 flex justify-end gap-2">
        <Button type="button" variant="secondary" autoFocus onClick={() => onAnswer(false)}>
          {t.cancel}
        </Button>
        <Button type="button" variant={options.destructive === false ? "primary" : "confirm"} disabled={!ready} onClick={() => onAnswer(true)}>
          {options.verb}
        </Button>
      </div>
    </Overlay>
  );
}
