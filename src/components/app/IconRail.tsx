import { ArrowLeftRight, Server, Settings, SquareCode, Users, type LucideIcon } from "lucide-react";
import mark from "@/assets/mark.svg";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { shortcutHint, type ShortcutId } from "@/lib/shortcuts";
import { cn } from "@/lib/utils";

export type NavKey = "hosts" | "forwards" | "snippets" | "team" | "settings";

const primary: { key: NavKey; label: string; icon: LucideIcon }[] = [
  { key: "hosts", label: t.navHosts, icon: Server },
  { key: "forwards", label: t.navForwards, icon: ArrowLeftRight },
  { key: "snippets", label: t.navSnippets, icon: SquareCode },
  { key: "team", label: t.navTeam, icon: Users },
];

export function IconRail({ active, onChange }: { active: NavKey; onChange: (key: NavKey) => void }) {
  return (
    <nav
      aria-label="AnixOps SSH"
      className="material-sidebar flex w-16 shrink-0 flex-col items-center gap-1 border-r py-3"
    >
      <img src={mark} alt="AnixOps SSH" className="mb-3 size-8" draggable={false} />
      {primary.map((item) => (
        <RailButton key={item.key} item={item} active={active === item.key} onChange={onChange} />
      ))}
      <div className="flex-1" />
      <RailButton
        item={{ key: "settings", label: t.navSettings, icon: Settings }}
        active={active === "settings"}
        onChange={onChange}
      />
    </nav>
  );
}

function RailButton({
  item,
  active,
  onChange,
}: {
  item: { key: NavKey; label: string; icon: LucideIcon };
  active: boolean;
  onChange: (key: NavKey) => void;
}) {
  const Icon = item.icon;
  const hint = shortcutHint(item.key === "settings" ? "settings" : (`go-${item.key}` as ShortcutId));
  return (
    <Tooltip label={hint ? `${item.label} (${hint})` : item.label} side="right">
      <button
        type="button"
        aria-label={item.label}
        aria-current={active ? "page" : undefined}
        onClick={() => onChange(item.key)}
        className={cn(
          "flex size-10 items-center justify-center rounded-sm transition-colors duration-micro ease-standard",
          active ? "bg-accent-soft text-(--accent)" : "text-label-2 hover:bg-fill-1 hover:text-label-1",
        )}
      >
        <Icon className="size-5" strokeWidth={1.5} />
      </button>
    </Tooltip>
  );
}
