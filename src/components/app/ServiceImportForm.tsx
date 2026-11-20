import { CircleCheck, LoaderCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { SecretField } from "@/components/app/SecretField";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  serviceRemembered,
  testService,
  type ServiceForm,
  type ServiceId,
  type ServiceRequest,
} from "@/lib/backend";
import { t } from "@/lib/i18n";

/** The form of "import from a service": address, credential (sealed in the
 *  vault, never shown again), default user, optional key file. "Test
 *  connection" makes one small request; "Preview" hands the request to the
 *  dialog, which fetches the hosts and shows them like any import. */
export function ServiceImportForm({
  service,
  busy,
  onPreview,
  onBack,
  firstRef,
}: {
  service: ServiceId;
  busy: boolean;
  onPreview: (request: ServiceRequest) => void;
  onBack: () => void;
  firstRef: React.Ref<HTMLInputElement>;
}) {
  const [form, setForm] = useState<ServiceForm>({
    url: "",
    default_user: "root",
    identity_file: null,
    access_key_id: "",
    org_id: "",
    gateway: true,
    gateway_host: "",
    gateway_port: 2222,
    gateway_user: "",
  });
  const jumpserver = service === "jumpserver";
  const [saved, setSaved] = useState(false);
  const [secret, setSecret] = useState("");
  const [cleared, setCleared] = useState(false);
  const [testing, setTesting] = useState(false);
  const [total, setTotal] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void serviceRemembered(service)
      .then((remembered) => {
        if (!alive) return;
        setForm(remembered.form);
        setSaved(remembered.has_secret);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [service]);

  const request = (): ServiceRequest => ({
    service,
    form: { ...form, identity_file: form.identity_file?.trim() || null },
    secret: secret.trim() || null,
    clear_secret: cleared,
  });
  const ready =
    form.url.trim() !== "" &&
    (secret.trim() !== "" || (saved && !cleared)) &&
    (!jumpserver || (form.access_key_id.trim() !== "" && (!form.gateway || form.gateway_user.trim() !== "")));
  const edit = (patch: Partial<ServiceForm>) => {
    setForm({ ...form, ...patch });
    setTotal(null);
  };

  const test = async () => {
    setTesting(true);
    setError(null);
    setTotal(null);
    try {
      setTotal(await testService(request()));
      // The credential just typed is saved now.
      if (secret.trim()) {
        setSaved(true);
        setSecret("");
        setCleared(false);
      }
    } catch (reason) {
      setError(String(reason instanceof Error ? reason.message : reason));
    } finally {
      setTesting(false);
    }
  };

  return (
    <form
      className="space-y-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (ready && !busy) onPreview(request());
      }}
    >
      <p className="text-callout font-normal text-label-2">{jumpserver ? t.importJmsIntro : t.importControlIntro}</p>
      <Field label={t.importServiceUrl} hint={t.importServiceHttpsOnly}>
        <Input
          ref={firstRef}
          value={form.url}
          onChange={(event) => edit({ url: event.target.value })}
          placeholder={t.importServiceUrlPlaceholder}
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          className="font-mono"
        />
      </Field>
      {jumpserver && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label={t.importJmsKeyId} hint={t.importJmsKeyIdHint}>
            <Input
              value={form.access_key_id}
              onChange={(event) => edit({ access_key_id: event.target.value })}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
          </Field>
          <Field label={t.importJmsOrg} hint={t.importJmsOrgHint}>
            <Input
              value={form.org_id}
              onChange={(event) => edit({ org_id: event.target.value })}
              placeholder="00000000-0000-0000-0000-000000000002"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
          </Field>
        </div>
      )}
      <Field
        label={jumpserver ? t.importJmsSecret : t.importControlToken}
        hint={jumpserver ? undefined : t.importControlTokenHint}
        group
      >
        <SecretField
          label={jumpserver ? t.importJmsSecret : t.importControlToken}
          saved={saved}
          value={secret}
          cleared={cleared}
          placeholder={jumpserver ? undefined : t.importControlTokenPlaceholder}
          onValue={(value) => {
            setSecret(value);
            setTotal(null);
          }}
          onCleared={setCleared}
        />
      </Field>
      {jumpserver && (
        <div className="space-y-3">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <p className="text-callout text-label-1">{t.importGateway}</p>
              <p className="mt-0.5 text-callout text-label-3">{t.importGatewayHint}</p>
            </div>
            <Switch on={form.gateway} label={t.importGateway} onClick={() => edit({ gateway: !form.gateway })} />
          </div>
          {form.gateway && (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
              <Field label={t.importGatewayUser}>
                <Input
                  value={form.gateway_user}
                  onChange={(event) => edit({ gateway_user: event.target.value })}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                />
              </Field>
              <Field label={t.importGatewayHost} hint={t.optional}>
                <Input
                  value={form.gateway_host}
                  onChange={(event) => edit({ gateway_host: event.target.value })}
                  placeholder={t.importGatewayHostHint}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                />
              </Field>
              <Field label={t.importGatewayPort}>
                <Input
                  value={String(form.gateway_port)}
                  onChange={(event) => edit({ gateway_port: Number(event.target.value.replace(/\D/g, "").slice(0, 5)) || 0 })}
                  inputMode="numeric"
                  autoComplete="off"
                  className="font-mono"
                />
              </Field>
            </div>
          )}
        </div>
      )}
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field
          label={jumpserver && form.gateway ? t.importGatewayAccount : t.importDefaultUser}
          hint={jumpserver && form.gateway ? t.importGatewayAccountHint : undefined}
        >
          <Input
            value={form.default_user}
            onChange={(event) => edit({ default_user: event.target.value })}
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </Field>
        <Field label={t.importServiceKey} hint={t.optional}>
          <Input
            value={form.identity_file ?? ""}
            onChange={(event) => edit({ identity_file: event.target.value })}
            placeholder="~/.ssh/id_ed25519"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </Field>
      </div>
      <p className="text-callout text-label-3">{t.importServiceKeyHint}</p>

      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" variant="secondary" disabled={!ready || testing || busy} onClick={() => void test()}>
          {testing && <LoaderCircle className="animate-spin motion-reduce:animate-none" />}
          {t.importTestConnection}
        </Button>
        {total !== null && (
          <span role="status" className="flex items-center gap-1.5 text-callout text-success">
            <CircleCheck className="size-4 shrink-0" strokeWidth={1.5} aria-hidden />
            {jumpserver ? t.importTestOkJms : t.importTestOk} {total} {jumpserver ? t.importTestOkAssets : t.importTestOkNodes}
          </span>
        )}
        <div className="flex-1" />
        <Button type="button" variant="quiet" disabled={busy} onClick={onBack}>
          {t.importServiceBack}
        </Button>
        <Button type="submit" variant="primary" disabled={!ready || busy || testing}>
          {busy && <LoaderCircle className="animate-spin motion-reduce:animate-none" />}
          {t.importServicePreview}
        </Button>
      </div>
      {error && <Notice>{error}</Notice>}
    </form>
  );
}
