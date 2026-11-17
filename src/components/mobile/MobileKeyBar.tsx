import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { BarKey } from "@/lib/mobileKeys";
import { t } from "@/lib/i18n";

const KEYS: { key: BarKey; label: string; name: string }[] = [
  { key: "esc", label: "Esc", name: t.mobileKeyEsc },
  { key: "tab", label: "Tab", name: t.mobileKeyTab },
];

const ARROWS: { key: BarKey; Icon: typeof ArrowUp; name: string }[] = [
  { key: "left", Icon: ArrowLeft, name: t.mobileKeyLeft },
  { key: "down", Icon: ArrowDown, name: t.mobileKeyDown },
  { key: "up", Icon: ArrowUp, name: t.mobileKeyUp },
  { key: "right", Icon: ArrowRight, name: t.mobileKeyRight },
];

/** The keys a soft keyboard lacks: Esc, Tab, a sticky Ctrl and the arrows.
 *  Every key is a 44 px touch target (layout.md). Pressing one must not move
 *  focus out of the terminal, or the keyboard would close: the bar swallows
 *  the pointer-down that would blur it. */
export function MobileKeyBar({
  ctrl,
  onToggleCtrl,
  onKey,
}: {
  ctrl: boolean;
  onToggleCtrl: () => void;
  onKey: (key: BarKey) => void;
}) {
  return (
    <div
      role="toolbar"
      aria-label={t.mobileKeys}
      onPointerDown={(event) => event.preventDefault()}
      className="material-bar flex shrink-0 items-center gap-1 overflow-x-auto border-t px-2 py-1 pb-safe"
    >
      {KEYS.map(({ key, label, name }) => (
        <Button
          key={key}
          variant="secondary"
          size="lg"
          aria-label={name}
          className="min-w-11 flex-1 px-0"
          onClick={() => onKey(key)}
        >
          {label}
        </Button>
      ))}
      <Button
        variant="secondary"
        size="lg"
        aria-label={t.mobileKeyCtrl}
        aria-pressed={ctrl}
        className="min-w-11 flex-1 px-0 aria-pressed:bg-accent-fill aria-pressed:text-on-accent"
        onClick={onToggleCtrl}
      >
        Ctrl
      </Button>
      {ARROWS.map(({ key, Icon, name }) => (
        <Button
          key={key}
          variant="secondary"
          size="lg"
          aria-label={name}
          className="min-w-11 flex-1 px-0"
          onClick={() => onKey(key)}
        >
          <Icon />
        </Button>
      ))}
    </div>
  );
}
