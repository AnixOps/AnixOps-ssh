import { Network } from "lucide-react";
import { SecretField } from "@/components/app/SecretField";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Select } from "@/components/ui/select";
import type { ProxyFields, ProxyKind } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { ANIXOPS_NET_PROXY, isAnixopsNet, newProxy, withKind } from "@/lib/proxyUi";
import { cn } from "@/lib/utils";

type Kind = ProxyKind | "none";

/** The proxy password being edited: whether one is saved (never its value), what was typed, and
 *  whether the saved one is to be removed. */
export interface ProxyPasswordDraft {
  saved: boolean;
  value: string;
  cleared: boolean;
  onValue: (value: string) => void;
  onCleared: (cleared: boolean) => void;
}

/** The outbound proxy of a host: none, SOCKS5 or HTTP, its address, an optional login, and a
 *  one-click preset for the local AnixOps Net proxy. Shared by the desktop host editor and the
 *  phone's host form (`touch`: 44 px controls, and a select instead of the segmented control). */
export function ProxySection({
  proxy,
  onChange,
  password,
  openssh = false,
  touch = false,
}: {
  proxy: ProxyFields | null;
  onChange: (proxy: ProxyFields | null) => void;
  password: ProxyPasswordDraft;
  /** The host is set to the OpenSSH engine, which cannot use a proxy. */
  openssh?: boolean;
  touch?: boolean;
}) {
  const kind: Kind = proxy?.kind ?? "none";
  const pick = (next: Kind) => {
    if (next === "none") onChange(null);
    else onChange(proxy ? withKind(proxy, next) : newProxy(next));
  };
  const set = <K extends keyof ProxyFields>(key: K, value: ProxyFields[K]) => {
    if (proxy) onChange({ ...proxy, [key]: value });
  };
  const control = touch ? "h-11" : undefined;
  const kinds: { value: Kind; label: string }[] = [
    { value: "none", label: t.proxyNone },
    { value: "socks5", label: t.proxySocks5 },
    { value: "http", label: t.proxyHttp },
  ];
  return (
    <section aria-labelledby="host-proxy-title" className={cn("rounded-md bg-fill-1 p-3", touch ? "space-y-5" : "space-y-3")}>
      <div>
        <h3 id="host-proxy-title" className={touch ? "text-title-3" : "text-callout text-label-2"}>
          {t.proxySection}
        </h3>
        <p className={cn("mt-0.5 text-callout", touch ? "text-label-2" : "text-label-3")}>{t.proxySectionHint}</p>
      </div>

      {touch ? (
        <Field label={t.proxyKind}>
          <Select className="h-11" value={kind} onChange={(event) => pick(event.target.value as Kind)}>
            {kinds.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </Select>
        </Field>
      ) : (
        <Field label={t.proxyKind} group>
          <Segmented value={kind} onChange={pick} label={t.proxyKind} options={kinds} />
        </Field>
      )}

      {proxy && (
        <>
          <div className="grid grid-cols-[1fr_6rem] gap-3">
            <Field label={t.proxyHost}>
              <Input
                className={cn("font-mono", control)}
                value={proxy.host}
                onChange={(event) => set("host", event.target.value)}
                placeholder="127.0.0.1"
                inputMode={touch ? "url" : undefined}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
              />
            </Field>
            <Field label={t.proxyPort}>
              <Input
                className={cn("font-mono", control)}
                type="number"
                min={1}
                max={65535}
                value={proxy.port}
                onChange={(event) => set("port", Number(event.target.value))}
              />
            </Field>
          </div>
          <Field label={t.proxyUsername} hint={t.optional}>
            <Input
              className={cn("font-mono", control)}
              value={proxy.username ?? ""}
              onChange={(event) => set("username", event.target.value || null)}
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
            />
          </Field>
          <Field label={t.proxyPassword} hint={t.optional} group={!touch}>
            {touch ? (
              <Input
                className="h-11"
                type="password"
                autoComplete="off"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                value={password.value}
                placeholder={password.saved ? `${t.secretSaved} · ${t.secretKeepHint}` : undefined}
                onChange={(event) => password.onValue(event.target.value)}
              />
            ) : (
              <SecretField
                label={t.proxyPassword}
                saved={password.saved}
                value={password.value}
                cleared={password.cleared}
                onValue={password.onValue}
                onCleared={password.onCleared}
              />
            )}
          </Field>
          <p className="text-callout text-label-3">{t.proxyAuthHint}</p>
        </>
      )}

      <div className="space-y-1.5">
        <Button
          type="button"
          variant="secondary"
          size={touch ? "lg" : "md"}
          className={cn("h-auto whitespace-normal py-2 text-center", touch ? "min-h-11" : "min-h-9")}
          aria-pressed={isAnixopsNet(proxy)}
          onClick={() => onChange({ ...ANIXOPS_NET_PROXY, username: null })}
        >
          <Network />
          {t.proxyAnixopsNet}
        </Button>
        <p className="text-callout text-label-3">{t.proxyAnixopsNetHint}</p>
      </div>

      {proxy && openssh && <Notice tone="warning">{t.proxyNeedsBuiltin}</Notice>}
    </section>
  );
}
