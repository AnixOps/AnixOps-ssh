import { ClipboardPaste, Eye, EyeOff } from "lucide-react";
import { useState, type CSSProperties, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Field, Notice, Textarea } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import {
  isVaultLocked,
  pasteText,
  saveHost,
  secretsSet,
  type SecretsInput,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { setHostAttach, type Attach } from "@/lib/mobilePrefs";
import { vaultErrorText } from "@/lib/vaultUi";

/** What a key field shows while hidden: dots, as a password field would. */
const MASKED = { WebkitTextSecurity: "disc" } as CSSProperties;

/** The smallest host form a terminal needs: name, host, user, port, and the
 *  login to keep in the vault (password, or a private key with its passphrase;
 *  all optional). Full screen on a phone, with the primary action on top and
 *  Cancel below it at full width (actions.md O5). Groups and tags stay on the
 *  desktop. */
export function MobileHostForm({ onCancel, onDone }: { onCancel: () => void; onDone: () => void }) {
  const [name, setName] = useState("");
  const [hostname, setHostname] = useState("");
  const [username, setUsername] = useState("");
  const [port, setPort] = useState("22");
  const [attach, setAttach] = useState<Attach>("off");
  const [password, setPassword] = useState("");
  const [privateKey, setPrivateKey] = useState("");
  const [keyPassphrase, setKeyPassphrase] = useState("");
  const [showKey, setShowKey] = useState(false);
  /** Set once the host is saved, so a retry after a login failure updates it instead of adding another. */
  const [savedId, setSavedId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const paste = () => {
    pasteText()
      .then((text) =>
        text === ""
          ? setError(t.mvPasteEmpty)
          : (setPrivateKey(text), setError(null)),
      )
      .catch((reason) => setError(String(reason)));
  };

  /** The host is saved first, then its login. A login that cannot be kept
   *  (vault locked) leaves the host in place and says so. */
  const keepLogin = async (hostId: string): Promise<string | null> => {
    const input: SecretsInput = {};
    if (password !== "") input.password = password;
    if (privateKey.trim() !== "") input.privateKey = privateKey.trim() + "\n";
    if (keyPassphrase !== "") input.passphrase = keyPassphrase;
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
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) return setError(`${t.port}: 1–65535`);
    setSaving(true);
    saveHost({
      id: savedId,
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
      .then(async (hostId) => {
        setSavedId(hostId);
        // Where the session is kept is a phone-side choice, stored by host id.
        if (attach !== "off") setHostAttach(hostId, attach);
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
        <section
          aria-labelledby="mobile-login-title"
          className="flex flex-col gap-5"
        >
          <div className="flex flex-col gap-1">
            <h3 id="mobile-login-title" className="text-title-3">
              {t.mvCredentials}
            </h3>
            <p className="text-callout text-label-2">{t.mvCredentialsHint}</p>
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
            <Button
              type="button"
              variant="secondary"
              size="lg"
              className="flex-1"
              onClick={paste}
            >
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
