import { Eye, EyeOff, Trash2, Undo2 } from "lucide-react";
import { useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** One saved secret in the host editor. The saved value never reaches the
 *  UI: a saved secret shows as such, typing replaces it, and "clear" removes
 *  it when the host is saved. Hidden by default, with show/hide. */
export function SecretField({
  label,
  saved,
  value,
  cleared,
  onValue,
  onCleared,
  multiline = false,
  placeholder,
  actions,
}: {
  label: string;
  saved: boolean;
  value: string;
  cleared: boolean;
  onValue: (value: string) => void;
  onCleared: (cleared: boolean) => void;
  multiline?: boolean;
  placeholder?: string;
  actions?: ReactNode;
}) {
  const [shown, setShown] = useState(false);
  const hasSomething = saved || value !== "";
  const hint = cleared ? t.secretClear : saved && value === "" ? `${t.secretSaved} · ${t.secretKeepHint}` : placeholder;
  const common = {
    value: cleared ? "" : value,
    disabled: cleared,
    placeholder: hint,
    autoComplete: "off",
    spellCheck: false,
    "aria-label": label,
    onChange: (event: { target: { value: string } }) => onValue(event.target.value),
  };
  return (
    <div className="flex items-start gap-1.5">
      <div className="min-w-0 flex-1">
        {multiline ? (
          <textarea
            {...common}
            rows={4}
            className={cn(
              "w-full resize-none rounded-sm bg-fill-1 px-3 py-2 font-mono text-caption text-label-1 outline-none placeholder:text-label-3 focus-visible:ring-2 focus-visible:ring-(--accent) disabled:opacity-60",
              !shown && "[-webkit-text-security:disc]",
            )}
          />
        ) : (
          <Input {...common} type={shown ? "text" : "password"} className="font-mono disabled:opacity-60" />
        )}
        {cleared && <p className="mt-1 text-caption font-normal text-label-3">{t.secretWillClear}</p>}
        {actions}
      </div>
      <Tooltip label={shown ? t.secretHide : t.secretShow}>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label={`${shown ? t.secretHide : t.secretShow}: ${label}`}
          aria-pressed={shown}
          onClick={() => setShown(!shown)}
        >
          {shown ? <EyeOff /> : <Eye />}
        </Button>
      </Tooltip>
      <Tooltip label={cleared ? t.secretUndo : t.secretClear}>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label={`${cleared ? t.secretUndo : t.secretClear}: ${label}`}
          disabled={!hasSomething && !cleared}
          onClick={() => {
            if (cleared) {
              onCleared(false);
            } else {
              onValue("");
              onCleared(saved);
            }
          }}
        >
          {cleared ? <Undo2 /> : <Trash2 />}
        </Button>
      </Tooltip>
    </div>
  );
}
