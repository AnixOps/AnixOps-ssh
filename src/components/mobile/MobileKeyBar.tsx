import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, ClipboardPaste, Lock } from "lucide-react";
import { useEffect, useRef, type ComponentProps, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { resolveItem, type BarLayout, type SnippetKey } from "@/lib/barLayout";
import { t } from "@/lib/i18n";
import type { BarKey, ModifierState } from "@/lib/mobileKeys";
import { cn } from "@/lib/utils";

export type ModifierName = "ctrl" | "alt";

/** What the bar tells the terminal. */
export interface KeyBarActions {
  key: (key: BarKey) => void;
  char: (text: string) => void;
  snippet: (snippet: SnippetKey) => void;
  paste: () => void;
  modifier: (name: ModifierName) => void;
}

const REPEAT_DELAY_MS = 380;
const REPEAT_INTERVAL_MS = 65;

/** Keys that repeat while held (cursor movement, paging, delete). */
const REPEATING = new Set<BarKey>(["up", "down", "left", "right", "pgup", "pgdn", "del"]);

const ARROW_ICONS = { up: ArrowUp, down: ArrowDown, left: ArrowLeft, right: ArrowRight };

const KEY_LABELS: Partial<Record<BarKey, { label: string; name: string }>> = {
  esc: { label: "Esc", name: t.mobileKeyEsc },
  tab: { label: "Tab", name: t.mobileKeyTab },
  home: { label: "Home", name: t.mobileKeyHome },
  end: { label: "End", name: t.mobileKeyEnd },
  pgup: { label: "PgUp", name: t.mobileKeyPgUp },
  pgdn: { label: "PgDn", name: t.mobileKeyPgDn },
  ins: { label: "Ins", name: t.mobileKeyIns },
  del: { label: "Del", name: t.mobileKeyDel },
};

const ARROW_NAMES = { up: t.mobileKeyUp, down: t.mobileKeyDown, left: t.mobileKeyLeft, right: t.mobileKeyRight };

/** The accessible name of an item, for the bar and for the editor. */
export function itemName(id: string, layout: Pick<BarLayout, "snippets">): string {
  const item = resolveItem(id, layout);
  if (!item) return id;
  switch (item.kind) {
    case "key": {
      if (item.key === "up" || item.key === "down" || item.key === "left" || item.key === "right") return ARROW_NAMES[item.key];
      if (/^f\d+$/.test(item.key)) return item.key.toUpperCase();
      return KEY_LABELS[item.key]?.name ?? item.key;
    }
    case "modifier":
      return item.modifier === "ctrl" ? t.mobileKeyCtrl : t.mobileKeyAlt;
    case "paste":
      return t.mobileKeyPaste;
    case "char":
      return item.text;
    case "snippet":
      return `${item.snippet.label}: ${item.snippet.text}`;
  }
}

/** Fires once when pressed, then repeats while held. A click without a
 *  preceding press (a screen reader's double tap, the keyboard) fires once. */
function useRepeat(fire: () => void, repeat: boolean) {
  const fireRef = useRef(fire);
  fireRef.current = fire;
  const delay = useRef<number | null>(null);
  const interval = useRef<number | null>(null);
  const lastUp = useRef(Number.NEGATIVE_INFINITY);

  const stop = () => {
    if (delay.current !== null) window.clearTimeout(delay.current);
    if (interval.current !== null) window.clearInterval(interval.current);
    delay.current = null;
    interval.current = null;
  };
  useEffect(() => stop, []);

  const props: Pick<ComponentProps<"button">, "onPointerDown" | "onPointerUp" | "onPointerCancel" | "onPointerLeave" | "onClick" | "onContextMenu"> = {
    onPointerDown: (event) => {
      if (event.button !== 0 && event.pointerType === "mouse") return;
      fireRef.current();
      if (repeat) {
        stop();
        delay.current = window.setTimeout(() => {
          interval.current = window.setInterval(() => fireRef.current(), REPEAT_INTERVAL_MS);
        }, REPEAT_DELAY_MS);
      }
    },
    onPointerUp: () => {
      stop();
      lastUp.current = performance.now();
    },
    onPointerCancel: stop,
    onPointerLeave: stop,
    onClick: () => {
      // A click right after a pointer release is the press that already fired;
      // one without it (a screen reader's double tap, a key) fires here.
      if (performance.now() - lastUp.current > 300) fireRef.current();
    },
    // A long press must not open the system menu.
    onContextMenu: (event) => event.preventDefault(),
  };
  return props;
}

function BarButton({
  name,
  repeat = false,
  onPress,
  className,
  children,
  ...rest
}: {
  name: string;
  repeat?: boolean;
  onPress: () => void;
  children: ReactNode;
} & Omit<ComponentProps<typeof Button>, "onClick" | "children" | "aria-label">) {
  const props = useRepeat(onPress, repeat);
  return (
    <Button
      variant="secondary"
      size="lg"
      aria-label={name}
      className={cn("min-w-11 touch-manipulation select-none px-0", className)}
      {...props}
      {...rest}
    >
      {children}
    </Button>
  );
}

function ModifierButton({
  name,
  shown,
  state,
  onPress,
  className,
}: {
  name: string;
  shown: string;
  state: ModifierState;
  onPress: () => void;
  className?: string;
}) {
  const label = state.mode === "armed" ? `${name}, ${t.mobileModArmed}` : state.mode === "locked" ? `${name}, ${t.mobileModLocked}` : name;
  return (
    <Button
      variant="secondary"
      size="lg"
      aria-label={label}
      aria-pressed={state.mode !== "off"}
      data-mode={state.mode}
      className={cn(
        "min-w-11 touch-manipulation select-none gap-0.5 px-0 data-[mode=armed]:bg-accent-fill data-[mode=armed]:text-on-accent data-[mode=locked]:bg-accent-fill data-[mode=locked]:text-on-accent",
        className,
      )}
      onClick={onPress}
    >
      {shown}
      {state.mode === "locked" && <Lock className="size-3" />}
    </Button>
  );
}

function ItemButton({
  id,
  layout,
  actions,
  ctrl,
  alt,
  className,
}: {
  id: string;
  layout: BarLayout;
  actions: KeyBarActions;
  ctrl: ModifierState;
  alt: ModifierState;
  className?: string;
}) {
  const item = resolveItem(id, layout);
  const name = itemName(id, layout);
  if (!item) return null;
  switch (item.kind) {
    case "modifier":
      return (
        <ModifierButton
          name={name}
          shown={item.modifier === "ctrl" ? "Ctrl" : "Alt"}
          state={item.modifier === "ctrl" ? ctrl : alt}
          onPress={() => actions.modifier(item.modifier)}
          className={className}
        />
      );
    case "key": {
      const key = item.key;
      const Icon = key === "up" || key === "down" || key === "left" || key === "right" ? ARROW_ICONS[key] : null;
      const label = KEY_LABELS[key]?.label ?? key.toUpperCase();
      return (
        <BarButton name={name} repeat={REPEATING.has(key)} onPress={() => actions.key(key)} className={className}>
          {Icon ? <Icon /> : label}
        </BarButton>
      );
    }
    case "paste":
      return (
        <BarButton name={name} onPress={actions.paste} className={className}>
          <ClipboardPaste />
        </BarButton>
      );
    case "char":
      return (
        <BarButton name={name} onPress={() => actions.char(item.text)} className={className}>
          {item.text}
        </BarButton>
      );
    case "snippet":
      return (
        <BarButton name={name} onPress={() => actions.snippet(item.snippet)} className={cn("px-2", className)}>
          {item.snippet.label}
        </BarButton>
      );
  }
}

/** The keys a soft keyboard lacks. The first row is always there: Esc, Tab,
 *  sticky Ctrl and Alt (tap to arm for one key, double tap to lock), and the
 *  arrows, which repeat while held. The second row swipes sideways and holds
 *  Home, End, paging, symbols, F1 to F12 and the person's own command keys.
 *  Both rows come from the saved layout (lib/barLayout.ts).
 *
 *  Every key is a 44 px touch target (layout.md). Pressing one must not move
 *  focus out of the terminal, or the keyboard would close: the bar swallows
 *  the pointer-down that would blur it. */
export function MobileKeyBar({
  layout,
  showExtra,
  ctrl,
  alt,
  actions,
}: {
  layout: BarLayout;
  showExtra: boolean;
  ctrl: ModifierState;
  alt: ModifierState;
  actions: KeyBarActions;
}) {
  return (
    <div
      onPointerDown={(event) => event.preventDefault()}
      className="material-bar flex shrink-0 flex-col gap-0.5 border-t px-1 py-1 pb-safe"
    >
      <div role="toolbar" aria-label={t.mobileKeys} className="flex items-center gap-0.5 overflow-x-auto">
        {layout.primary.map((id) => (
          <ItemButton key={id} id={id} layout={layout} actions={actions} ctrl={ctrl} alt={alt} className="flex-1" />
        ))}
      </div>
      {showExtra && layout.extra.length > 0 && (
        <div
          role="toolbar"
          aria-label={t.mobileExtraRow}
          data-extra-row
          className="flex snap-x snap-proximity items-center gap-0.5 overflow-x-auto overscroll-x-contain"
          style={{ scrollbarWidth: "none" }}
        >
          {layout.extra.map((id) => (
            <ItemButton key={id} id={id} layout={layout} actions={actions} ctrl={ctrl} alt={alt} className="shrink-0 snap-start px-2.5" />
          ))}
        </div>
      )}
    </div>
  );
}
