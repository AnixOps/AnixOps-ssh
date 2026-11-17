import { CircleCheck, KeyRound, ShieldCheck, Trash2 } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import { aiConfigGet, aiConfigSave, aiKeySet } from "@/lib/aiBackend";
import { isVaultLocked } from "@/lib/backend";
import { DEFAULT_MODEL, needsKey, PROVIDERS, switchProvider, type AiConfig } from "@/lib/ai/providers";
import type { AiProvider } from "@/lib/ai/review";
import { t } from "@/lib/i18n";
import { requestVaultUnlock, vaultErrorText } from "@/lib/vaultUi";

const providerLabel = (provider: AiProvider) =>
  provider === "anthropic" ? t.aiProviderAnthropic : provider === "openai" ? t.aiProviderOpenai : t.aiProviderOllama;

/** Settings → AI assistant: off by default, the provider, the model, and the
 *  API key, which goes straight into the vault and is never shown again. */
export function AiSettings() {
  const [config, setConfig] = useState<AiConfig | null>(null);
  const [draft, setDraft] = useState<AiConfig | null>(null);
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    aiConfigGet()
      .then((loaded) => {
        setConfig(loaded);
        setDraft(loaded);
      })
      .catch((reason) => setError(String(reason)));
  }, []);

  if (!config || !draft) {
    return <p className="px-3 py-2 text-caption font-normal text-label-3">{error ?? "…"}</p>;
  }

  const fail = (reason: unknown) => {
    if (isVaultLocked(reason)) requestVaultUnlock();
    setError(vaultErrorText(reason));
    setNotice(null);
  };

  const saveConfig = async (next: AiConfig) => {
    setError(null);
    setNotice(null);
    try {
      const saved = await aiConfigSave(next);
      setConfig(saved);
      setDraft(saved);
      setNotice(t.aiSaved);
    } catch (reason) {
      fail(reason);
    }
  };

  const changeProvider = (provider: AiProvider) => {
    const next = switchProvider(draft, provider);
    // Another provider has its own key slot.
    setDraft({ ...next, hasKey: false });
    setKey("");
    void saveConfig(next);
  };

  const saveKey = async (value: string | null) => {
    setError(null);
    setNotice(null);
    try {
      const saved = await aiKeySet(draft.provider, value);
      setConfig(saved);
      setDraft((current) => (current ? { ...current, hasKey: saved.hasKey } : current));
      setKey("");
      setNotice(t.aiSaved);
    } catch (reason) {
      fail(reason);
    }
  };

  const dirty =
    draft.model !== config.model ||
    (draft.baseUrl ?? "") !== (config.baseUrl ?? "") ||
    draft.enabled !== config.enabled;
  const keyNeeded = needsKey(draft);
  const hasUrl = draft.provider !== "anthropic";

  return (
    <>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <ShieldCheck className="mt-0.5 size-4 shrink-0 text-(--accent)" strokeWidth={1.5} />
        <p className="text-caption font-normal text-label-2">{t.aiIntro}</p>
      </div>

      <Row label={t.aiEnable} hint={t.aiEnableHint}>
        <Switch
          on={config.enabled}
          label={t.aiEnable}
          onClick={() => void saveConfig({ ...draft, enabled: !config.enabled })}
        />
      </Row>

      <Row label={t.aiProvider}>
        <Segmented
          size="sm"
          label={t.aiProvider}
          value={draft.provider}
          onChange={changeProvider}
          options={PROVIDERS.map((provider) => ({ value: provider, label: providerLabel(provider) }))}
        />
      </Row>

      <div className="space-y-1.5 px-3 py-2">
        <label className="block space-y-1">
          <span className="text-callout text-label-1">{t.aiModel}</span>
          <Input
            value={draft.model}
            placeholder={DEFAULT_MODEL[draft.provider]}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => setDraft({ ...draft, model: event.target.value })}
          />
        </label>
        <p className="text-caption font-normal text-label-3">{t.aiModelHint}</p>
      </div>

      {hasUrl && (
        <div className="space-y-1.5 px-3 py-2">
          <label className="block space-y-1">
            <span className="text-callout text-label-1">{t.aiBaseUrl}</span>
            <Input
              value={draft.baseUrl ?? ""}
              placeholder={draft.provider === "openai" ? "https://api.openai.com/v1" : "http://localhost:11434"}
              spellCheck={false}
              autoComplete="off"
              inputMode="url"
              onChange={(event) => setDraft({ ...draft, baseUrl: event.target.value || null })}
            />
          </label>
          <p className="text-caption font-normal text-label-3">
            {draft.provider === "openai" ? t.aiBaseUrlOpenai : t.aiBaseUrlOllama}
          </p>
        </div>
      )}

      <div className="space-y-2 px-3 py-2">
        <div className="flex items-center gap-1.5">
          <KeyRound className="size-4 text-label-2" strokeWidth={1.5} />
          <span className="text-callout text-label-1">{t.aiApiKey}</span>
        </div>
        {draft.provider === "ollama" ? (
          <p className="text-caption font-normal text-label-3">{t.aiKeyNotNeeded}</p>
        ) : (
          <>
            <p className="flex items-center gap-1.5 text-caption font-normal text-label-2">
              {draft.hasKey && <CircleCheck className="size-3.5 shrink-0 text-success" strokeWidth={1.5} />}
              {draft.hasKey ? t.aiKeySaved : keyNeeded ? t.aiKeyMissing : t.aiKeyOptional}
            </p>
            <form
              className="flex gap-2"
              onSubmit={(event) => {
                event.preventDefault();
                if (key.trim()) void saveKey(key);
              }}
            >
              <Input
                type="password"
                value={key}
                aria-label={t.aiApiKey}
                placeholder={t.aiKeyPlaceholder}
                autoComplete="off"
                spellCheck={false}
                onChange={(event) => setKey(event.target.value)}
              />
              <Button type="submit" variant="secondary" size="sm" className="h-9" disabled={!key.trim()}>
                {t.aiKeySave}
              </Button>
            </form>
            {draft.hasKey && (
              <Button variant="ghost" size="sm" onClick={() => void saveKey(null)}>
                <Trash2 />
                {t.aiKeyRemove}
              </Button>
            )}
          </>
        )}
      </div>

      <div className="flex items-center gap-3 px-3 py-2">
        <Button variant="primary" size="sm" disabled={!dirty} onClick={() => void saveConfig(draft)}>
          {t.aiSave}
        </Button>
        {error && <p role="alert" className="min-w-0 flex-1 text-caption font-normal text-danger">{error}</p>}
        {!error && notice && <p role="status" className="text-caption font-normal text-label-2">{notice}</p>}
      </div>
    </>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex min-h-11 items-center gap-3 px-3 py-1.5">
      <div className="min-w-0 flex-1">
        <p className="text-callout text-label-1">{label}</p>
        {hint && <p className="text-caption font-normal text-label-2">{hint}</p>}
      </div>
      {children}
    </div>
  );
}
