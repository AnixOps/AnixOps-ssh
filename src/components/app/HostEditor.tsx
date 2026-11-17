import { FileUp, Star, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { HostHistory, HostVault } from "@/components/app/HostSync";
import { SecretField } from "@/components/app/SecretField";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Field, Notice, Textarea } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Menu } from "@/components/ui/menu";
import { Select } from "@/components/ui/select";
import { Overlay } from "@/components/ui/overlay";
import { Segmented } from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import {
  deleteHost,
  hostColor,
  isVaultLocked,
  saveHost,
  secretsSet,
  secretsStatus,
  type EnginePreference,
  type Group,
  type Host,
  type HostFields,
  type SecretsInput,
  type SecretsStatus,
} from "@/lib/backend";
import { logHostSet, logSettingsGet } from "@/lib/aiBackend";
import { t } from "@/lib/i18n";
import { choiceValue, hostChoice, type HostChoice } from "@/lib/sessionLogs";
import { requestVaultUnlock } from "@/lib/vaultUi";
import { cn } from "@/lib/utils";

const COLORS = ["chart-1", "chart-2", "chart-3", "chart-4", "chart-5", "chart-6", "chart-7", "chart-8"];

const emptyFields: HostFields = { alias: null, hostname: "", username: null, port: 22, proxyJump: null, notes: "", shellIntegration: false };

type SecretName = keyof SecretsStatus;
/** What the user did to one saved secret: typed a new value, or marked it for removal. */
type Draft = { value: string; cleared: boolean };
const emptyDraft: Draft = { value: "", cleared: false };
const noSecrets: SecretsStatus = { password: false, privateKey: false, passphrase: false };

/** Create or edit a host. `host` is null for a new one. */
export function HostEditor({
  host,
  groups,
  onDone,
  onCancel,
}: {
  host: Host | null;
  groups: Group[];
  onDone: () => void;
  onCancel: () => void;
}) {
  const saved = host?.source === "saved";
  const [name, setName] = useState(host?.name ?? "");
  const [byAlias, setByAlias] = useState(Boolean(host?.fields.alias));
  const [fields, setFields] = useState<HostFields>(host?.fields ?? emptyFields);
  const [group, setGroup] = useState(groups.find((candidate) => candidate.id === host?.groupId)?.name ?? "");
  const [tags, setTags] = useState(host?.tags.join(", ") ?? "");
  const [color, setColor] = useState<string | null>(host?.color ?? null);
  const [favorite, setFavorite] = useState(host?.favorite ?? false);
  const [error, setError] = useState<string | null>(null);
  const confirm = useConfirm();
  const [busy, setBusy] = useState(false);
  const firstRef = useRef<HTMLInputElement>(null);
  const keyFileRef = useRef<HTMLInputElement>(null);
  // Which secrets are saved (never their values) and what is being changed.
  const [savedSecrets, setSavedSecrets] = useState<SecretsStatus>(noSecrets);
  const [drafts, setDrafts] = useState<Record<SecretName, Draft>>({
    password: emptyDraft,
    privateKey: emptyDraft,
    passphrase: emptyDraft,
  });
  const [keyNote, setKeyNote] = useState<string | null>(null);
  const [logChoice, setLogChoice] = useState<HostChoice>("default");
  const [logLoaded, setLogLoaded] = useState<HostChoice | null>(null);

  useEffect(() => firstRef.current?.focus(), []);
  useEffect(() => {
    if (host && saved) secretsStatus(host.id).then(setSavedSecrets).catch(console.error);
  }, [host, saved]);

  useEffect(() => {
    if (!host || !saved) return;
    logSettingsGet()
      .then((settings) => {
        const current = hostChoice(settings, host.id);
        setLogChoice(current);
        setLogLoaded(current);
      })
      .catch(console.error);
  }, [host, saved]);

  const draft = (name: SecretName, change: Partial<Draft>) =>
    setDrafts((current) => ({ ...current, [name]: { ...current[name], ...change } }));

  /** Reads a key file chosen by the user. Only the content is kept, not the path. */
  const importKey = async (file: File | undefined) => {
    if (!file) return;
    try {
      draft("privateKey", { value: await file.text(), cleared: false });
      setKeyNote(t.keyImported);
    } catch (reason) {
      setError(String(reason));
    }
  };

  /** What to send to the vault: untouched fields are left out so they stay as saved. */
  const secretChanges = (): SecretsInput => {
    const input: SecretsInput = {};
    for (const name of ["password", "privateKey", "passphrase"] as const) {
      const { value, cleared } = drafts[name];
      if (cleared) input[name] = "";
      else if (value !== "") input[name] = value;
    }
    return input;
  };

  const set = <K extends keyof HostFields>(key: K, value: HostFields[K]) =>
    setFields((current) => ({ ...current, [key]: value }));

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const id = await saveHost({
        id: host?.id ?? null,
        name: name.trim() || (byAlias ? fields.alias ?? "" : fields.hostname),
        group: group.trim() || null,
        tags: tags.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
        favorite,
        color,
        fields: byAlias
          ? { ...fields, alias: fields.alias?.trim() || null }
          : { ...fields, alias: null, port: Number(fields.port) || 22 },
      });
      const changes = secretChanges();
      if (Object.keys(changes).length > 0) {
        try {
          await secretsSet(id, changes);
        } catch (reason) {
          // The host itself is saved; only its secrets are not.
          if (isVaultLocked(reason)) requestVaultUnlock();
          setError(`${t.secretsSaveFailed}: ${String(reason)}`);
          setBusy(false);
          return;
        }
      }
      if (logChoice !== (logLoaded ?? "default")) {
        try {
          await logHostSet(id, choiceValue(logChoice));
        } catch (reason) {
          setError(String(reason));
          setBusy(false);
          return;
        }
      }
      onDone();
    } catch (reason) {
      setError(String(reason));
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!host) return;
    const title = t.hostDeleteTitle.replace("{name}", host.name);
    if (!(await confirm({ title, body: t.hostDeleteBody, verb: t.delete }))) return;
    setBusy(true);
    try {
      await deleteHost(host.id);
      onDone();
    } catch (reason) {
      setError(String(reason));
      setBusy(false);
    }
  };

  const title = !host ? t.newHost : saved ? t.editHost : t.saveToHosts;

  return (
    <Overlay
      labelledBy="host-editor-title"
      onDismiss={onCancel}
      onSubmit={() => void submit()}
      className="flex max-h-full max-w-lg flex-col"
    >
        <div className="flex items-center gap-2 px-6 pt-6">
          <h2 id="host-editor-title" className="min-w-0 flex-1 truncate text-title-3">
            {title}
          </h2>
          {saved && (
            <Menu
              label={t.more}
              items={[{ key: "delete", label: t.delete, icon: <Trash2 />, danger: true, onSelect: () => void remove() }]}
              disabled={busy}
            />
          )}
        </div>

        <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-6 pt-5 pb-2">
          <Field label={t.hostName}>
            <Input ref={firstRef} value={name} onChange={(event) => setName(event.target.value)} placeholder="prod-web-01" />
          </Field>

          <Field label={t.connectBy} group>
            <Segmented
              value={byAlias}
              onChange={setByAlias}
              label={t.connectBy}
              options={[
                { value: false, label: t.byAddress },
                { value: true, label: t.byAlias },
              ]}
            />
          </Field>

          {byAlias ? (
            <Field label={t.alias}>
              <Input
                value={fields.alias ?? ""}
                onChange={(event) => set("alias", event.target.value)}
                placeholder="bastion"
                className="font-mono"
                spellCheck={false}
              />
            </Field>
          ) : (
            <>
              <div className="grid grid-cols-[1fr_6rem] gap-3">
                <Field label={t.hostname}>
                  <Input
                    value={fields.hostname}
                    onChange={(event) => set("hostname", event.target.value)}
                    placeholder="10.0.1.11"
                    className="font-mono"
                    spellCheck={false}
                  />
                </Field>
                <Field label={t.port}>
                  <Input
                    type="number"
                    min={1}
                    max={65535}
                    value={fields.port}
                    onChange={(event) => set("port", Number(event.target.value))}
                    className="font-mono"
                  />
                </Field>
              </div>
              <Field label={t.username} hint={t.optional}>
                <Input
                  value={fields.username ?? ""}
                  onChange={(event) => set("username", event.target.value || null)}
                  placeholder="deploy"
                  className="font-mono"
                  spellCheck={false}
                />
              </Field>
            </>
          )}

          <Field label={t.proxyJump} hint={t.optional}>
            <Input
              value={fields.proxyJump ?? ""}
              onChange={(event) => set("proxyJump", event.target.value || null)}
              placeholder="bastion"
              className="font-mono"
              spellCheck={false}
            />
          </Field>

          <Field label={t.hostEngine} hint={t.optional}>
            <Select
              aria-label={t.hostEngine}
              value={fields.engine ?? "auto"}
              onChange={(event) => set("engine", event.target.value as EnginePreference)}
            >
              <option value="auto">{t.engineAuto}</option>
              <option value="russh">{t.engineBuiltin}</option>
              <option value="openssh">{t.engineOpenssh}</option>
            </Select>
          </Field>

          <div className="grid grid-cols-2 gap-3">
            <Field label={t.group} hint={t.optional}>
              <Input value={group} onChange={(event) => setGroup(event.target.value)} list="host-editor-groups" />
              <datalist id="host-editor-groups">
                {groups.map((candidate) => (
                  <option key={candidate.id} value={candidate.name} />
                ))}
              </datalist>
            </Field>
            <Field label={t.tags} hint={t.tagsHint}>
              <Input value={tags} onChange={(event) => setTags(event.target.value)} placeholder="web, nginx" />
            </Field>
          </div>

          <Field label={t.color} group>
            <div className="flex items-center gap-2">
              <Swatch selected={color === null} label={t.noColor} onClick={() => setColor(null)} />
              {COLORS.map((token, index) => (
                <Swatch key={token} color={token} selected={color === token} label={`${t.color} ${index + 1}`} onClick={() => setColor(token)} />
              ))}
            </div>
          </Field>

          <div className="flex items-center justify-between gap-4">
            <p className="flex items-center gap-2 text-callout text-label-2">
              <Star className="size-4" strokeWidth={1.5} />
              {t.favorite}
            </p>
            <Switch on={favorite} label={t.favorite} onClick={() => setFavorite(!favorite)} />
          </div>

          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <p className="text-callout text-label-1">{t.shellIntegration}</p>
              <p className="mt-0.5 text-callout text-label-3">{t.shellIntegrationHint}</p>
            </div>
            <Switch
              on={fields.shellIntegration}
              label={t.shellIntegration}
              onClick={() => set("shellIntegration", !fields.shellIntegration)}
            />
          </div>

          <section aria-labelledby="host-editor-credentials" className="space-y-3 rounded-md bg-fill-1 p-3">
            <div>
              <h3 id="host-editor-credentials" className="text-callout text-label-2">
                {t.savedCredentials}
              </h3>
              <p className="mt-0.5 text-callout text-label-3">{t.credentialsHint}</p>
            </div>
            <Field label={t.savePassword} hint={t.optional} group>
              <SecretField
                label={t.savePassword}
                saved={savedSecrets.password}
                value={drafts.password.value}
                cleared={drafts.password.cleared}
                onValue={(value) => draft("password", { value, cleared: false })}
                onCleared={(cleared) => draft("password", { cleared })}
              />
            </Field>
            <Field label={t.privateKey} hint={t.optional} group>
              <SecretField
                label={t.privateKey}
                multiline
                placeholder={t.keyPaste}
                saved={savedSecrets.privateKey}
                value={drafts.privateKey.value}
                cleared={drafts.privateKey.cleared}
                onValue={(value) => {
                  setKeyNote(null);
                  draft("privateKey", { value, cleared: false });
                }}
                onCleared={(cleared) => draft("privateKey", { cleared })}
                actions={
                  <div className="mt-1.5 flex items-center gap-2">
                    <input
                      ref={keyFileRef}
                      type="file"
                      hidden
                      aria-label={t.keyImport}
                      onChange={(event) => {
                        void importKey(event.target.files?.[0]);
                        event.target.value = "";
                      }}
                    />
                    <Button type="button" variant="secondary" size="sm" onClick={() => keyFileRef.current?.click()}>
                      <FileUp />
                      {t.keyImport}
                    </Button>
                    <span className="text-callout text-label-3">{keyNote ?? t.keyImportHint}</span>
                  </div>
                }
              />
            </Field>
            <Field label={t.keyPassphrase} hint={t.optional} group>
              <SecretField
                label={t.keyPassphrase}
                saved={savedSecrets.passphrase}
                value={drafts.passphrase.value}
                cleared={drafts.passphrase.cleared}
                onValue={(value) => draft("passphrase", { value, cleared: false })}
                onCleared={(cleared) => draft("passphrase", { cleared })}
              />
            </Field>
          </section>

          <Field label={t.notes} hint={t.optional}>
            <Textarea value={fields.notes} onChange={(event) => set("notes", event.target.value)} rows={3} />
          </Field>

          {saved && host && <HostVault hostId={host.id} onMoved={onDone} />}
          {saved && host && <HostHistory hostId={host.id} onRestored={onDone} />}
          <Field label={t.logsHost} hint={t.logsHostHint}>
            <Segmented
              size="sm"
              label={t.logsHost}
              value={logChoice}
              onChange={setLogChoice}
              options={[
                { value: "default", label: t.logsHostFollow },
                { value: "on", label: t.logsHostOn },
                { value: "off", label: t.logsHostOff },
              ]}
            />
          </Field>

          {error && (
            <Notice>
              {t.saveFailed}: {error}
            </Notice>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 px-6 pt-4 pb-6">
          <Button type="button" variant="secondary" onClick={onCancel}>
            {t.cancel}
          </Button>
          <Button type="submit" variant="primary" disabled={busy}>
            {t.save}
          </Button>
        </div>
    </Overlay>
  );
}

function Swatch({
  color,
  selected,
  label,
  onClick,
}: {
  color?: string;
  selected: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={selected}
      onClick={onClick}
      className={cn(
        "flex size-7 items-center justify-center rounded-full ring-offset-2 ring-offset-(--bg-elevated) transition-shadow duration-micro",
        selected ? "ring-2 ring-(--accent)" : "ring-1 ring-separator",
        !color && "bg-fill-1",
      )}
      style={color ? { backgroundColor: hostColor(color) } : undefined}
    />
  );
}
