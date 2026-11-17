import { Fingerprint, KeyRound, ShieldQuestion, Usb } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import { Tooltip } from "@/components/ui/tooltip";
import { onAskpass, onAskpassResolved, replyAskpass, unlockForAskpass, type AskpassPrompt } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { vaultErrorText } from "@/lib/vaultUi";

/** Answers OpenSSH prompts from processes without a terminal (the file pane
 *  and transfers). The answer goes straight back to OpenSSH; nothing is kept. */
export function AskpassDialog() {
  const [queue, setQueue] = useState<AskpassPrompt[]>([]);
  const [value, setValue] = useState("");
  const [remember, setRemember] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const current = queue[0];

  useEffect(() => onAskpass((prompt) => setQueue((items) => [...items, prompt])), []);

  // Another window answered (or the prompt timed out): close it here too.
  useEffect(() => onAskpassResolved((id) => setQueue((items) => items.filter((item) => item.id !== id))), []);

  useEffect(() => {
    setValue("");
    setRemember(false);
    setError(null);
    if (current) window.requestAnimationFrame(() => inputRef.current?.focus());
  }, [current]);

  const answer = (prompt: AskpassPrompt, reply: string | null) => {
    replyAskpass(prompt.id, reply, remember && !!prompt.rememberable && reply !== null);
    setValue("");
    setQueue((items) => items.slice(1));
  };

  // The vault is locked and holds the answer: unlock it and let it answer.
  const unlockAndUse = async (prompt: AskpassPrompt) => {
    setError(null);
    try {
      await unlockForAskpass(prompt.id, value);
      setValue("");
      setQueue((items) => items.filter((item) => item.id !== prompt.id));
    } catch (reason) {
      setError(vaultErrorText(reason));
    }
  };

  // "Touch your security key": nothing to answer. The daemon closes it with
  // askpass.resolved when the key signed; dismissing only hides it here.
  if (current?.kind === "security_key_touch") {
    return (
      <AnimatePresence>
        <Overlay
          key={current.id}
          labelledBy="askpass-title"
          onDismiss={() => answer(current, null)}
          className="max-w-md p-6"
        >
          <div className="flex items-start gap-3" role="status" aria-live="polite">
            <Fingerprint className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            <div className="min-w-0 flex-1">
              <h2 id="askpass-title" className="text-title-3">
                {t.securityKeyTouchTitle}
              </h2>
              <p className="mt-2 text-callout text-label-2">{t.securityKeyTouchBody}</p>
              <p className="selectable mt-2 text-callout break-words text-label-3">{current.host}</p>
            </div>
          </div>
          <div className="mt-6 flex justify-end">
            <Tooltip label={t.securityKeyTouchDismissTip}>
              <Button
                type="button"
                variant="secondary"
                aria-label={t.securityKeyTouchDismiss}
                onClick={() => answer(current, null)}
              >
                {t.securityKeyTouchDismiss}
              </Button>
            </Tooltip>
          </div>
        </Overlay>
      </AnimatePresence>
    );
  }

  const pin = current?.kind === "security_key_pin";

  return (
    <AnimatePresence>
      {current && (
        <Overlay
          key={current.id}
          labelledBy="askpass-title"
          onDismiss={() => answer(current, null)}
          onSubmit={() =>
            current.vaultLocked ? void unlockAndUse(current) : answer(current, current.confirm ? "yes" : value)
          }
          className="max-w-md p-6"
        >
          <div className="flex items-start gap-3">
            {current.confirm ? (
              <ShieldQuestion className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            ) : pin ? (
              <Usb className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            ) : (
              <KeyRound className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            )}
            <div className="min-w-0 flex-1">
              <h2 id="askpass-title" className="text-title-3">
                {pin ? `${t.securityKeyPinTitle} ` : `${current.confirm ? t.askpassConfirmTitle : t.askpassTitle} `}
                {current.host}
              </h2>
              <p className="selectable mt-2 text-callout break-words whitespace-pre-wrap text-label-2">
                {current.prompt.trim()}
              </p>
            </div>
          </div>
          {!current.confirm && (
            <>
              <Input
                ref={inputRef}
                type={current.echo ? "text" : "password"}
                autoComplete="off"
                value={value}
                onChange={(event) => setValue(event.target.value)}
                aria-label={current.vaultLocked ? t.vaultPassphrase : pin ? t.securityKeyPinLabel : current.prompt}
                className="mt-5"
              />
              {current.vaultLocked ? (
                <p className="mt-2 text-callout text-label-3">{t.askpassVaultLocked}</p>
              ) : current.rememberable ? (
                <label className="mt-3 flex items-start gap-2 text-callout text-label-2">
                  <input
                    type="checkbox"
                    checked={remember}
                    onChange={(event) => setRemember(event.target.checked)}
                    className="mt-1 accent-(--accent)"
                  />
                  <span>
                    {t.askpassRemember}
                    <span className="block text-callout text-label-3">{t.askpassRememberHint}</span>
                  </span>
                </label>
              ) : (
                <p className="mt-2 text-callout text-label-3">{pin ? t.securityKeyPinNotSaved : t.askpassNotSaved}</p>
              )}
              {error && (
                <Notice className="mt-2">{error}</Notice>
              )}
            </>
          )}
          <div className="mt-6 flex justify-end gap-2">
            <Button type="button" variant="secondary" onClick={() => answer(current, null)}>
              {t.cancel}
            </Button>
            <Button type="submit" variant="primary">
              {current.confirm ? t.askpassTrust : current.vaultLocked ? t.askpassUnlockUse : t.askpassSubmit}
            </Button>
          </div>
        </Overlay>
      )}
    </AnimatePresence>
  );
}
