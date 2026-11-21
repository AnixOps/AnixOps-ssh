import { Flag, ShieldAlert } from "lucide-react";
import { Tooltip } from "@/components/ui/tooltip";
import type { HostEnvironment } from "@/lib/backend";
import { environmentName, environmentTone } from "@/lib/environment";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** The environment of a host as a small badge: an icon and its name, so it
 *  never relies on colour. Production is red, staging amber, the others quiet
 *  and plain. With `shrink` the name drops out below the container's 42rem
 *  (icon, tooltip and accessible name stay), for the tab strip in a narrow window. */
export function EnvBadge({
  environment,
  shrink = false,
  className,
}: {
  environment: HostEnvironment | null | undefined;
  shrink?: boolean;
  className?: string;
}) {
  if (!environment) return null;
  const name = environmentName(environment);
  const tone = environmentTone(environment);
  const Icon = environment === "production" ? ShieldAlert : Flag;
  return (
    <Tooltip label={name}>
      <span
        role="img"
        aria-label={t.envBadgeAria.replace("{env}", name)}
        data-env={environment}
        className={cn(
          "inline-flex h-5 shrink-0 items-center gap-1 rounded-xs px-1 text-caption font-medium",
          tone === "danger" && "bg-danger-soft text-danger",
          tone === "warning" && "bg-warning-soft text-warning",
          tone === null && "bg-fill-1 text-label-2",
          className,
        )}
      >
        <Icon className="size-3.5" strokeWidth={1.5} aria-hidden />
        <span className={shrink ? "hidden @2xl:inline" : undefined}>{name}</span>
      </span>
    </Tooltip>
  );
}
