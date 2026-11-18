import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { saveTunnel, type Host, type Tunnel } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { checkDraft, draftOf, emptyDraft } from "@/lib/mobileForwards";

/** A local port forward: a port on this phone, and the target a host reaches.
 *  Full screen with the primary action on top of Cancel (actions.md O5). */
export function MobileForwardForm({
  tunnel,
  hosts,
  onCancel,
  onDone,
}: {
  tunnel?: Tunnel;
  hosts: Host[];
  onCancel: () => void;
  onDone: () => void;
}) {
  const choices = hosts.filter((host) => host.fields.serial == null).sort((a, b) => a.name.localeCompare(b.name));
  const [draft, setDraft] = useState(() => (tunnel ? draftOf(tunnel) : emptyDraft(choices[0]?.id ?? "")));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = (change: Partial<typeof draft>) => setDraft((current) => ({ ...current, ...change }));

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const checked = checkDraft(draft);
    if (!checked.ok) {
      const label = {
        host: t.tunnelVia,
        bindPort: t.mobileForwardPhonePort,
        targetHost: t.tunnelTargetHost,
        targetPort: t.tunnelTargetPort,
      }[checked.problem];
      return setError(`${label}: ${checked.problem === "host" ? t.required : t.mobileForwardInvalid}`);
    }
    setSaving(true);
    saveTunnel({ ...checked.input, id: tunnel?.id ?? null })
      .then(onDone)
      .catch((reason) => {
        setError(String(reason));
        setSaving(false);
      });
  };

  return (
    <form onSubmit={submit} aria-labelledby="mobile-forward-title" className="fixed inset-0 z-drawer flex flex-col bg-bg">
      <header className="material-bar pt-safe shrink-0 border-b">
        <h2 id="mobile-forward-title" className="px-safe flex h-14 items-center text-title-3">
          {tunnel ? t.editTunnel : t.newTunnel}
        </h2>
      </header>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-5">
        {error && <Notice>{error}</Notice>}
        <Field label={t.tunnelVia}>
          <Select className="h-11" value={draft.hostId} onChange={(event) => set({ hostId: event.target.value })}>
            {choices.map((host) => (
              <option key={host.id} value={host.id}>
                {host.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t.mobileForwardPhonePort} hint={t.mobileForwardPhonePortHint}>
          <Input
            className="h-11"
            value={draft.bindPort}
            onChange={(event) => set({ bindPort: event.target.value })}
            inputMode="numeric"
            placeholder="8080"
          />
        </Field>
        <Field label={t.tunnelTargetHost} hint={t.mobileForwardTargetHint}>
          <Input
            className="h-11"
            value={draft.targetHost}
            onChange={(event) => set({ targetHost: event.target.value })}
            inputMode="url"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
        <Field label={t.tunnelTargetPort}>
          <Input
            className="h-11"
            value={draft.targetPort}
            onChange={(event) => set({ targetPort: event.target.value })}
            inputMode="numeric"
            placeholder="5432"
          />
        </Field>
        <Field label={t.snippetName} hint={t.optional}>
          <Input
            className="h-11"
            value={draft.name}
            onChange={(event) => set({ name: event.target.value })}
            placeholder={t.tunnelNameExample}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
        <p className="text-callout text-label-3">{t.mobileForwardHint}</p>
      </div>
      <div className="px-safe pb-safe material-bar flex shrink-0 flex-col gap-2 border-t py-3">
        <Button type="submit" variant="primary" size="lg" disabled={saving}>
          {t.save}
        </Button>
        <Button type="button" variant="quiet" size="lg" onClick={onCancel}>
          {t.cancel}
        </Button>
      </div>
    </form>
  );
}
