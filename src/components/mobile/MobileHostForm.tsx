import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { saveHost } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { setHostAttach, type Attach } from "@/lib/mobilePrefs";

/** The smallest host form a terminal needs: name, host, user, port. Full
 *  screen on a phone, with the primary action on top and Cancel below it at
 *  full width (actions.md O5). Keys, groups and tags stay on the desktop. */
export function MobileHostForm({ onCancel, onDone }: { onCancel: () => void; onDone: () => void }) {
  const [name, setName] = useState("");
  const [hostname, setHostname] = useState("");
  const [username, setUsername] = useState("");
  const [port, setPort] = useState("22");
  const [attach, setAttach] = useState<Attach>("off");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const portNumber = Number(port);
    if (!hostname.trim()) return setError(`${t.hostname}: ${t.required}`);
    // A phone has no local user to fall back on, so the login name is needed.
    if (!username.trim()) return setError(`${t.username}: ${t.required}`);
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) return setError(`${t.port}: 1–65535`);
    setSaving(true);
    saveHost({
      id: null,
      name: name.trim() || hostname.trim(),
      group: null,
      tags: [],
      favorite: false,
      color: null,
      fields: {
        alias: null,
        hostname: hostname.trim(),
        username: username.trim(),
        port: portNumber,
        proxyJump: null,
        notes: "",
        shellIntegration: false,
      },
    })
      .then((id) => {
        // Where the session is kept is a phone-side choice, stored by host id.
        if (attach !== "off") setHostAttach(id, attach);
        onDone();
      })
      .catch((reason) => {
        setError(String(reason));
        setSaving(false);
      });
  };

  return (
    <form
      onSubmit={submit}
      aria-labelledby="mobile-host-title"
      className="fixed inset-0 z-drawer flex flex-col bg-bg"
    >
      <header className="material-bar pt-safe shrink-0 border-b">
        <h2 id="mobile-host-title" className="px-safe flex h-14 items-center text-title-3">
          {t.mobileAddHost}
        </h2>
      </header>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-5">
        {error && <Notice>{error}</Notice>}
        <Field label={t.hostName} hint={t.optional}>
          <Input
            className="h-11"
            value={name}
            onChange={(event) => setName(event.target.value)}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
        <Field label={t.hostname}>
          <Input
            className="h-11"
            value={hostname}
            onChange={(event) => setHostname(event.target.value)}
            inputMode="url"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            required
          />
        </Field>
        <Field label={t.username}>
          <Input
            className="h-11"
            required
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
        <Field label={t.port}>
          <Input
            className="h-11"
            value={port}
            onChange={(event) => setPort(event.target.value)}
            inputMode="numeric"
          />
        </Field>
        <Field label={t.mobileAttach}>
          <Select className="h-11" value={attach} onChange={(event) => setAttach(event.target.value as Attach)}>
            <option value="off">{t.mobileAttachOff}</option>
            <option value="tmux">tmux</option>
            <option value="screen">screen</option>
          </Select>
          <span className="mt-1 block text-callout text-label-2">{t.mobileAttachHint}</span>
        </Field>
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
