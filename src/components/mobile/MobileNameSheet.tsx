import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";
import { validName } from "@/lib/mobileFiles";

/** Asks for one file name (a new folder, a rename), full screen like the other
 *  forms of the phone: the primary action on top of Cancel, both full width
 *  (actions.md O5). */
export function MobileNameSheet({
  title,
  label,
  initial,
  verb,
  onCancel,
  onDone,
}: {
  title: string;
  label: string;
  initial: string;
  verb: string;
  onCancel: () => void;
  onDone: (name: string) => void;
}) {
  const [name, setName] = useState(initial);
  const [error, setError] = useState<string | null>(null);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!validName(name)) return setError(t.mobileNameInvalid);
    onDone(name.trim());
  };

  return (
    <form onSubmit={submit} aria-labelledby="mobile-name-title" className="fixed inset-0 z-drawer flex flex-col bg-bg">
      <header className="material-bar pt-safe shrink-0 border-b">
        <h2 id="mobile-name-title" className="px-safe flex h-14 items-center truncate text-title-3">
          {title}
        </h2>
      </header>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-5">
        {error && <Notice>{error}</Notice>}
        <Field label={label}>
          <Input
            className="h-11"
            value={name}
            onChange={(event) => setName(event.target.value)}
            autoFocus
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
      </div>
      <div className="px-safe pb-safe material-bar flex shrink-0 flex-col gap-2 border-t py-3">
        <Button type="submit" variant="primary" size="lg">
          {verb}
        </Button>
        <Button type="button" variant="quiet" size="lg" onClick={onCancel}>
          {t.cancel}
        </Button>
      </div>
    </form>
  );
}
