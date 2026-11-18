import { useState, type FormEvent } from "react";
import { MobileSwitchRow } from "@/components/mobile/MobileSwitchRow";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { saveHost, type Group, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { groupName, jumpHostFor, jumpOptions, jumpText, parseTags, tagsText } from "@/lib/mobileHosts";

/** The host form of the phone: name, address, user and port, then group, tags,
 *  favorite and a jump host chosen from the saved hosts. Full screen, with the
 *  primary action on top and Cancel below it at full width (actions.md O5).
 *  Keys and passwords are the vault's business, not this form's. */
export function MobileHostForm({
  host,
  hosts,
  groups,
  onCancel,
  onDone,
}: {
  /** The host being edited; none for a new one. */
  host?: Host;
  hosts: Host[];
  groups: Group[];
  onCancel: () => void;
  onDone: () => void;
}) {
  const [name, setName] = useState(host?.name ?? "");
  const [hostname, setHostname] = useState(host?.fields.hostname ?? "");
  const [username, setUsername] = useState(host?.fields.username ?? "");
  const [port, setPort] = useState(String(host?.fields.port ?? 22));
  const [group, setGroup] = useState(host ? (groupName(host, groups) ?? "") : "");
  const [tags, setTags] = useState(tagsText(host?.tags ?? []));
  const [favorite, setFavorite] = useState(host?.favorite ?? false);
  const [jump, setJump] = useState(host?.fields.proxyJump ?? "");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const options = jumpOptions(hosts, host?.id ?? null);
  const chosen = jumpHostFor(options, jump);
  // A jump chain written by hand (on the desktop) is kept as it is.
  const custom = jump !== "" && chosen === null;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const portNumber = Number(port);
    if (!hostname.trim()) return setError(`${t.hostname}: ${t.required}`);
    // A phone has no local user to fall back on, so the login name is needed.
    if (!username.trim()) return setError(`${t.username}: ${t.required}`);
    if (!/^\d{1,5}$/.test(port.trim()) || portNumber < 1 || portNumber > 65535) return setError(`${t.port}: 1–65535`);
    setSaving(true);
    saveHost({
      id: host?.id ?? null,
      name: name.trim() || hostname.trim(),
      group: group.trim() || null,
      tags: parseTags(tags),
      favorite,
      color: host?.color ?? null,
      fields: {
        alias: null,
        notes: "",
        shellIntegration: false,
        ...host?.fields,
        hostname: hostname.trim(),
        username: username.trim(),
        port: portNumber,
        proxyJump: jump.trim() || null,
      },
    })
      .then(onDone)
      .catch((reason) => {
        setError(String(reason));
        setSaving(false);
      });
  };

  const title = host ? t.editHost : t.mobileAddHost;

  return (
    <form
      onSubmit={submit}
      aria-labelledby="mobile-host-title"
      className="fixed inset-0 z-drawer flex flex-col bg-bg"
    >
      <header className="material-bar pt-safe shrink-0 border-b">
        <h2 id="mobile-host-title" className="px-safe flex h-14 items-center text-title-3">
          {title}
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
        <Field label={t.group} hint={t.optional}>
          <Input
            className="h-11"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
            list="mobile-groups"
            autoCapitalize="words"
            autoCorrect="off"
            spellCheck={false}
          />
          <datalist id="mobile-groups">
            {groups.map((entry) => (
              <option key={entry.id} value={entry.name} />
            ))}
          </datalist>
        </Field>
        <Field label={t.tags} hint={t.tagsHint}>
          <Input
            className="h-11"
            value={tags}
            onChange={(event) => setTags(event.target.value)}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </Field>
        <Field label={t.proxyJump} hint={t.optional}>
          <Select className="h-11" value={custom ? "custom" : (chosen?.id ?? "")} onChange={(event) => {
            const picked = options.find((option) => option.id === event.target.value);
            if (event.target.value !== "custom") setJump(picked ? jumpText(picked) : "");
          }}>
            <option value="">{t.mobileNoJump}</option>
            {custom && <option value="custom">{jump}</option>}
            {options.map((option) => (
              <option key={option.id} value={option.id}>
                {option.name}
              </option>
            ))}
          </Select>
        </Field>
        <MobileSwitchRow label={t.favorite} on={favorite} onChange={setFavorite} />
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
