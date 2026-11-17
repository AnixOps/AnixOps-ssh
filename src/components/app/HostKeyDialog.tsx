import { ShieldAlert, ShieldQuestion } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Overlay } from "@/components/ui/overlay";
import {
  onHostKey,
  onHostKeyResolved,
  replyHostKey,
  type HostKeyDecision,
  type HostKeyPrompt,
} from "@/lib/backend";
import { t } from "@/lib/i18n";

/** Asks whether to trust a host key the built-in engine does not know, or
 *  warns that a known host presents another one. For a changed key the safe
 *  answer is the default: Escape, Enter and the focused button all cancel,
 *  and replacing the recorded key takes its own deliberate button. */
export function HostKeyDialog() {
  const [queue, setQueue] = useState<HostKeyPrompt[]>([]);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const current = queue[0];

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
          className="max-w-lg p-6"
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
                {changed ? t.hostKeyChangedBody : t.hostKeyNewBody}
              </p>
              <dl className="selectable mt-4 space-y-2 text-callout">
                <div>
                  <dt className="text-caption font-normal text-label-3">{t.hostKeyAlgorithm}</dt>
                  <dd className="font-mono break-all">{current.algorithm}</dd>
                </div>
                <div>
                  <dt className="text-caption font-normal text-label-3">{t.hostKeyFingerprint}</dt>
                  <dd className="font-mono break-all">{current.fingerprint}</dd>
                </div>
                {changed &&
                  current.expected.map((print) => (
                    <div key={print}>
                      <dt className="text-caption font-normal text-label-3">{t.hostKeyRecorded}</dt>
                      <dd className="font-mono break-all">{print}</dd>
                    </div>
                  ))}
              </dl>
            </div>
          </div>
          <div className="mt-6 flex flex-wrap justify-end gap-2">
            {changed ? (
              <Button type="button" variant="destructive" onClick={() => answer(current, "replace_and_save")}>
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
            <Button ref={cancelRef} type="submit" variant={changed ? "primary" : "secondary"}>
              {t.cancel}
            </Button>
          </div>
        </Overlay>
      )}
    </AnimatePresence>
  );
}
