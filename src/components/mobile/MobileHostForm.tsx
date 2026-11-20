import { ClipboardPaste, Eye, EyeOff } from "lucide-react";
import { useEffect, useState, type CSSProperties, type FormEvent } from "react";
import { ProxySection } from "@/components/app/ProxySection";
import { MobileSwitchRow } from "@/components/mobile/MobileSwitchRow";
import { Button } from "@/components/ui/button";
import { Field, Notice, Textarea } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import {
  isVaultLocked,
  pasteText,
  saveHost,
  secretsSet,
  secretsStatus,
  type Group,
  type Host,
  type ProxyFields,
  type SecretsInput,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { hostAttach, setHostAttach, type Attach } from "@/lib/mobilePrefs";
import { cleanProxy, proxyReady } from "@/lib/proxyUi";
import { vaultErrorText } from "@/lib/vaultUi";
import { groupName, jumpHostFor, jumpOptions, jumpText, parseTags, tagsText } from "@/lib/mobileHosts";

/** What a key field shows while hidden: dots, as a password field would. */
const MASKED = { WebkitTextSecurity: "disc" } as CSSProperties;

/** The host form of the phone: name, address, user and port, then group, tags,
 *  favorite and a jump host chosen from the saved hosts. Full screen, with the
 *  primary action on top and Cancel below it at full width (actions.md O5).
 *  The login to keep in the vault (a password, or a private key with its
 *  passphrase; all optional) comes last; what is left empty is not changed. */
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
  const [attach, setAttach] = useState<Attach>(host ? hostAttach(host.id) : "off");
  const [jump, setJump] = useState(host?.fields.proxyJump ?? "");
  const [password, setPassword] = useState("");
  const [privateKey, setPrivateKey] = useState("");
  const [keyPassphrase, setKeyPassphrase] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [proxy, setProxy] = useState<ProxyFields | null>(host?.fields.proxy ?? null);
  const [proxyPassword, setProxyPassword] = useState("");
  /** Whether the host already has a proxy password in the vault (never its value). */
  const [proxyPasswordSaved, setProxyPasswordSaved] = useState(false);
  /** Set once the host is saved, so a retry after a login failure updates it instead of adding another. */
  const [savedId, setSavedId] = useState<string | null>(host?.id ?? null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (host) secretsStatus(host.id).then((status) => setProxyPasswordSaved(status.proxyPassword)).catch(() => {});
  }, [host]);

  const options = jumpOptions(hosts, host?.id ?? null);
  const chosen = jumpHostFor(options, jump);
  // A jump chain written by hand (on the desktop) is kept as it is.
  const custom = jump !== "" && chosen === null;

  const paste = () => {
    pasteText()
      .then((text) => (text === "" ? setError(t.mvPasteEmpty) : (setPrivateKey(text), setError(null))))
      .catch((reason) => setError(String(reason)));
  };

  /** The host is saved first, then its login. A login that cannot be kept
   *  (vault locked) leaves the host in place and says so. */
  const keepLogin = async (hostId: string): Promise<string | null> => {
    const input: SecretsInput = {};
    if (password !== "") input.password = password;
    if (privateKey.trim() !== "") input.privateKey = privateKey.trim() + "\n";
    if (keyPassphrase !== "") input.passphrase = keyPassphrase;
    // A host without a proxy keeps no proxy password; empty keeps the saved one.
    if (!proxy && proxyPasswordSaved) input.proxyPassword = "";
    else if (proxy && proxyPassword !== "") input.proxyPassword = proxyPassword;
    if (Object.keys(input).length === 0) return null;
    try {
      await secretsSet(hostId, input);
      return null;
    } catch (reason) {
      return `${t.mvLoginNotSaved}: ${isVaultLocked(reason) ? t.mvVaultNeedsUnlock : vaultErrorText(reason)}`;
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const portNumber = Number(port);
    if (!hostname.trim()) return setError(`${t.hostname}: ${t.required}`);
    // A phone has no local user to fall back on, so the login name is needed.
    if (!username.trim()) return setError(`${t.username}: ${t.required}`);
    if (!/^\d{1,5}$/.test(port.trim()) || portNumber < 1 || portNumber > 65535) return setError(`${t.port}: 1–65535`);
    if (!proxyReady(proxy)) return setError(`${t.proxyHost}: ${t.required}`);
    setSaving(true);
    saveHost({
      id: savedId,
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
        proxy: cleanProxy(proxy),
      },
    })
      .then(async (hostId) => {
        setSavedId(hostId);
        // Where the session is kept is a phone-side choice, stored by host id.
        setHostAttach(hostId, attach);
        const problem = await keepLogin(hostId);
        if (problem) {
          setError(problem);
          setSaving(false);
          return;
        }
        onDone();
      })
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
        <Field label={t.mobileAttach}>
          <Select className="h-11" value={attach} onChange={(event) => setAttach(event.target.value as Attach)}>
            <option value="off">{t.mobileAttachOff}</option>
            <option value="tmux">tmux</option>
            <option value="screen">screen</option>
          </Select>
          <span className="mt-1 block text-callout text-label-2">{t.mobileAttachHint}</span>
        </Field>
        <MobileSwitchRow label={t.favorite} on={favorite} onChange={setFavorite} />
        <ProxySection
          touch
          proxy={proxy}
          onChange={setProxy}
          password={{
            saved: proxyPasswordSaved,
            value: proxyPassword,
            cleared: false,
            onValue: setProxyPassword,
            onCleared: () => {},
          }}
        />
        <section aria-labelledby="mobile-login-title" className="flex flex-col gap-5">
          <div className="flex flex-col gap-1">
            <h3 id="mobile-login-title" className="text-title-3">
              {t.mvCredentials}
            </h3>
            <p className="text-callout text-label-2">{t.mvCredentialsHint}</p>
            {host && <p className="text-callout text-label-3">{t.secretKeepHint}</p>}
          </div>
          <Field label={t.mvPassword} hint={t.optional}>
            <Input
              className="h-11"
              type="password"
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </Field>
          <Field label={t.mvPrivateKey} hint={t.mvPrivateKeyHint}>
            <Textarea
              rows={4}
              value={privateKey}
              onChange={(event) => setPrivateKey(event.target.value)}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              style={showKey ? undefined : MASKED}
              className="font-mono"
            />
          </Field>
          <div className="flex gap-2">
            <Button type="button" variant="secondary" size="lg" className="flex-1" onClick={paste}>
              <ClipboardPaste />
              {t.mvPasteClipboard}
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="icon-lg"
              aria-label={showKey ? t.mvHideKey : t.mvShowKey}
              aria-pressed={showKey}
              onClick={() => setShowKey(!showKey)}
            >
              {showKey ? <EyeOff /> : <Eye />}
            </Button>
          </div>
          <Field label={t.mvKeyPassphrase} hint={t.optional}>
            <Input
              className="h-11"
              type="password"
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={keyPassphrase}
              onChange={(event) => setKeyPassphrase(event.target.value)}
            />
          </Field>
        </section>
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
