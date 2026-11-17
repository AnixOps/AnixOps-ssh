import { useEffect, useState } from "react";
import { FirstRunCard } from "@/components/app/FirstRunCard";
import { telemetryStatus } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { onSettings, settings, updateSettings } from "@/lib/settings";
import { needsFirstRun, type TelemetryStatus } from "@/lib/telemetry";

/** Asks once about crash reports, on the first start of a build that can send them. */
export function FirstRunPrompt() {
  const [current, setCurrent] = useState(settings());
  const [status, setStatus] = useState<TelemetryStatus | null>(null);

  useEffect(() => onSettings(setCurrent), []);
  useEffect(() => {
    telemetryStatus().then(setStatus).catch(console.error);
  }, []);

  if (!needsFirstRun(current, status)) return null;
  return (
    <FirstRunCard
      strings={{ title: t.firstRunTitle, body: t.firstRunBody, allow: t.firstRunAllow, deny: t.firstRunDeny }}
      onAnswer={(allow) => updateSettings({ crashReports: allow }).catch(console.error)}
    />
  );
}
