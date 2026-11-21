import { Check, ChevronRight, Copy, ShieldAlert, ShieldQuestion, X } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import {
  copyText,
  onHostKey,
  onHostKeyResolved,
  replyHostKey,
  type HostKeyDecision,
  type HostKeyPrompt,
} from "@/lib/backend";
import { compareFingerprint, keygenCommand } from "@/lib/fingerprint";
import { t } from "@/lib/i18n";

/** Asks whether to trust a host key the built-in engine does not know, or
 *  warns that a known host presents another one. For a changed key the safe
 *  answer is the default: Escape, Enter and the focused button all cancel,
 *  and replacing the recorded key takes its own deliberate button. */
export function HostKeyDialog() {
  const [queue, setQueue] = useState<HostKeyPrompt[]>([]);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const current = queue[0];
  const [pasted, setPasted] = useState("");
  const [verified, setVerified] = useState(false);
  const match = current ? compareFingerprint(current.fingerprint, pasted) : "empty";
  // A match counts as the verification; unticking is not possible while it holds.
  // A pasted fingerprint that differs always blocks replacing, even after a tick.
  const confirmed = match === "match" || (verified && match !== "mismatch");

  // Every prompt starts blank.
  useEffect(() => {
    setPasted("");
    setVerified(false);
  }, [current?.id]);

  useEffect(() => onHostKey((prompt) => setQueue((items) => [...items, prompt])), []);
  // Another window answered, or the prompt timed out.
  useEffect(() => onHostKeyResolved((id) => setQueue((items) => items.filter((item) => item.id !== id))), []);

  useEffect(() => {
    if (current) window.requestAnimationFrame(() => cancelRef.current?.focus());
  }, [current]);

  const answer = (prompt: HostKeyPrompt, decision: HostKeyDecision) => {
    replyHostKey(prompt.id, decision);
    setQueue((items) => items.slice(1));
  };

  const changed = current?.status === "changed";
  // A key of a type not on record yet: asked like a new host, with the
  // host's keys of other types shown.
  const newType = !changed && (current?.expected?.length ?? 0) > 0;
  const where = current ? (current.port === 22 ? current.host : `${current.host}:${current.port}`) : "";

  return (
    <AnimatePresence>
      {current && (
        <Overlay
          key={current.id}
          labelledBy="hostkey-title"
          onDismiss={() => answer(current, "reject")}
          // Enter never accepts a key.
          onSubmit={() => answer(current, "reject")}
          className="max-h-full max-w-lg overflow-y-auto p-6"
        >
          <div className="flex items-start gap-3">
            {changed ? (
              <ShieldAlert className="mt-0.5 size-6 shrink-0 text-danger" strokeWidth={1.5} />
            ) : (
              <ShieldQuestion className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            )}
            <div className="min-w-0 flex-1">
              <h2 id="hostkey-title" className="text-title-3">
                {changed ? t.hostKeyChangedTitle : t.hostKeyNewTitle}: {where}
              </h2>
              <p
                className={
                  changed
                    ? "mt-2 rounded-sm bg-danger-soft px-3 py-2 text-callout text-danger"
                    : "mt-2 text-callout text-label-2"
                }
                role={changed ? "alert" : undefined}
              >
                {changed ? t.hostKeyChangedBody : newType ? t.hostKeyNewTypeBody : t.hostKeyNewBody}
              </p>
              <dl className="selectable mt-4 space-y-2 text-callout">
                <div>
                  <dt className="text-callout text-label-3">{t.hostKeyAlgorithm}</dt>
                  <dd className="font-mono break-all">{current.algorithm}</dd>
                </div>
                <div>
                  <dt className="text-callout text-label-3">{t.hostKeyFingerprint}</dt>
                  <dd className="font-mono break-all">{current.fingerprint}</dd>
                </div>
                {(changed || newType) &&
                  (current.expected ?? []).map((print) => (
                    <div key={print}>
                      <dt className="text-callout text-label-3">
                        {changed ? t.hostKeyRecorded : t.hostKeyOtherRecorded}
                      </dt>
                      <dd className="font-mono break-all">{print}</dd>
                    </div>
                  ))}
              </dl>
              <details className="group mt-4 text-callout">
                <summary className="flex cursor-pointer list-none items-center gap-1 text-label-2 hover:text-label-1">
                  <ChevronRight className="size-4 transition-transform group-open:rotate-90" strokeWidth={1.5} />
                  {t.hostKeyHowTo}
                </summary>
                <div className="mt-2 space-y-2 text-label-2">
                  <p>{t.hostKeyHowToIntro}</p>
                  <ul className="list-disc space-y-1 pl-5">
                    <li>{t.hostKeyHowToAdmin}</li>
                    <li>{t.hostKeyHowToCloud}</li>
                    <li>{t.hostKeyHowToConsole}</li>
                  </ul>
                  <div className="flex items-center gap-2 rounded-sm bg-fill-1 py-1 pl-3 pr-1">
                    <code className="selectable min-w-0 flex-1 break-all font-mono text-label-1">
                      {keygenCommand(current.algorithm)}
                    </code>
                    <Button
                      type="button"
                      variant="quiet"
                      size="icon"
                      aria-label={t.hostKeyCopyCommand}
                      title={t.hostKeyCopyCommand}
                      onClick={() => void copyText(keygenCommand(current.algorithm))}
                    >
                      <Copy strokeWidth={1.5} />
                    </Button>
                  </div>
                  {changed && <p>{t.hostKeyHowToChanged}</p>}
                </div>
              </details>
              <label className="mt-4 block text-callout text-label-3" htmlFor="hostkey-paste">
                {t.hostKeyPasteLabel}
              </label>
              <Input
                id="hostkey-paste"
                value={pasted}
                onChange={(event) => setPasted(event.target.value)}
                // Enter here must not cancel the dialog.
                onKeyDown={(event) => event.key === "Enter" && event.preventDefault()}
                placeholder={t.hostKeyPastePlaceholder}
                spellCheck={false}
                autoComplete="off"
                className="mt-1 font-mono"
              />
              <p
                className={
                  "mt-1 flex items-start gap-1 text-callout " +
                  (match === "match" ? "text-success" : match === "mismatch" ? "text-danger" : "text-label-3")
                }
                role={match === "empty" ? undefined : "status"}
              >
                {match === "match" && <Check className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} />}
                {match === "mismatch" && <X className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} />}
                <span>
                  {match === "match"
                    ? t.hostKeyMatch
                    : match === "mismatch"
                      ? t.hostKeyMismatch
                      : match === "md5"
                        ? t.hostKeyMd5
                        : t.hostKeyPasteHint}
                </span>
              </p>
              {changed && (
                <label className="mt-3 flex items-start gap-2 text-callout text-label-2">
                  <input
                    type="checkbox"
                    checked={confirmed}
                    disabled={match === "match" || match === "mismatch"}
                    onChange={(event) => setVerified(event.target.checked)}
                    className="mt-1 accent-(--accent)"
                  />
                  <span>{t.hostKeyConfirmedChannel}</span>
                </label>
              )}
            </div>
          </div>
          {/* Cancel first and focused; the strongest action is last (O1, D6). Enter never accepts a key. */}
          <div className="mt-6 flex flex-wrap justify-end gap-2">
            <Button ref={cancelRef} type="submit" variant="secondary">
              {t.cancel}
            </Button>
            {changed ? (
              <Button
                type="button"
                variant="confirm"
                disabled={!confirmed}
                onClick={() => answer(current, "replace_and_save")}
              >
                {t.hostKeyReplace}
              </Button>
            ) : (
              <>
                <Button type="button" variant="secondary" onClick={() => answer(current, "accept_once")}>
                  {t.hostKeyAcceptOnce}
                </Button>
                <Button type="button" variant="primary" onClick={() => answer(current, "accept_and_save")}>
                  {t.hostKeyTrustSave}
                </Button>
              </>
            )}
          </div>
        </Overlay>
      )}
    </AnimatePresence>
  );
}
