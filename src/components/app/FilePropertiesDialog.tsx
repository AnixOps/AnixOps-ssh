import { useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import type { RemoteFile } from "@/lib/backend";
import {
  BITS,
  WHO,
  formatOctal,
  hasBit,
  parseModeString,
  parseOctal,
  setBit,
  type Bit,
  type Who,
} from "@/lib/filePermissions";
import { t } from "@/lib/i18n";
import { formatSize } from "@/lib/remotePath";

const KIND = {
  file: () => t.propKindFile,
  directory: () => t.propKindFolder,
  symlink: () => t.propKindLink,
  other: () => t.propKindOther,
} as const;

const ROLE: Record<Who, () => string> = {
  user: () => t.propRoleUser,
  group: () => t.propRoleGroup,
  other: () => t.propRoleOther,
};

const BIT_LABEL: Record<Bit, () => string> = {
  read: () => t.propRead,
  write: () => t.propWrite,
  execute: () => t.propExecute,
};

/** Size, owner, times and the permission bits of one entry, with the bits editable. */
export function FilePropertiesDialog({
  entry,
  dir,
  onApply,
  onClose,
}: {
  entry: RemoteFile;
  /** The folder the entry is listed in. */
  dir: string;
  onApply: (mode: number, recursive: boolean) => Promise<void>;
  onClose: () => void;
}) {
  const initial = parseModeString(entry.permissions);
  const [mode, setMode] = useState(initial ?? 0);
  const [text, setText] = useState(formatOctal(initial ?? 0));
  const [recursive, setRecursive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const editable = entry.kind !== "symlink" && initial !== null;
  const typed = parseOctal(text);
  const changed = mode !== initial;

  const choose = (next: number) => {
    setMode(next);
    setText(formatOctal(next));
  };

  const submit = async () => {
    if (!editable || typed === null || busy || (!changed && !recursive)) return;
    setBusy(true);
    setError(null);
    try {
      await onApply(typed, recursive);
      onClose();
    } catch (reason) {
      setError(String(reason));
      setBusy(false);
    }
  };

  return (
    <Overlay labelledBy="properties-title" onDismiss={onClose} onSubmit={() => void submit()} className="max-w-md p-6">
      <h2 id="properties-title" className="truncate text-title-3" title={entry.name}>
        {t.properties}: {entry.name}
      </h2>
      <dl className="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-callout">
        <Row label={t.propKind}>{KIND[entry.kind]()}</Row>
        <Row label={t.propLocation} mono>
          {dir}
        </Row>
        {entry.kind !== "directory" && (
          <Row label={t.propSize}>
            {formatSize(entry.size)} ({entry.size.toLocaleString()} B)
          </Row>
        )}
        {entry.modified && <Row label={t.propModified}>{entry.modified}</Row>}
        {entry.owner && <Row label={t.propOwner}>{entry.owner}</Row>}
        {entry.group && <Row label={t.propGroup}>{entry.group}</Row>}
        <Row label={t.propPermissions} mono>
          {entry.permissions}
        </Row>
      </dl>

      <fieldset disabled={!editable || busy} className="mt-4 disabled:opacity-60">
        <table className="w-full border-collapse text-callout">
          <thead>
            <tr className="text-left text-label-2">
              <th className="py-1 font-medium" />
              {BITS.map((bit) => (
                <th key={bit} className="py-1 font-medium">
                  {BIT_LABEL[bit]()}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {WHO.map((who) => (
              <tr key={who} className="h-7">
                <th scope="row" className="text-left font-normal text-label-2">
                  {ROLE[who]()}
                </th>
                {BITS.map((bit) => (
                  <td key={bit}>
                    <input
                      type="checkbox"
                      className="size-4 accent-(--accent)"
                      aria-label={`${ROLE[who]()}: ${BIT_LABEL[bit]()}`}
                      checked={hasBit(mode, who, bit)}
                      onChange={(event) => choose(setBit(mode, who, bit, event.target.checked))}
                    />
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>

        <label className="mt-3 flex items-center gap-3 text-callout text-label-2">
          {t.propOctal}
          <Input
            value={text}
            onChange={(event) => {
              setText(event.target.value);
              const next = parseOctal(event.target.value);
              if (next !== null) setMode(next);
            }}
            inputMode="numeric"
            maxLength={4}
            spellCheck={false}
            autoComplete="off"
            aria-invalid={typed === null}
            className="h-8 w-24 font-mono"
          />
        </label>
        {typed === null && <p className="mt-1 text-callout text-danger">{t.propOctalInvalid}</p>}

        {entry.kind === "directory" && (
          <label className="mt-3 flex items-start gap-2 text-callout text-label-2">
            <input
              type="checkbox"
              checked={recursive}
              onChange={(event) => setRecursive(event.target.checked)}
              className="mt-1 accent-(--accent)"
            />
            <span>
              {t.propApplyContents}
              <span className="block text-callout text-label-3">{t.propApplyContentsHint}</span>
            </span>
          </label>
        )}
      </fieldset>

      {error && <Notice className="mt-3 break-all">{error}</Notice>}

      <div className="mt-6 flex justify-end gap-2">
        <Button type="button" variant="secondary" onClick={onClose}>
          {t.cancel}
        </Button>
        <Button type="submit" variant="primary" disabled={!editable || typed === null || busy || (!changed && !recursive)}>
          {t.apply}
        </Button>
      </div>
    </Overlay>
  );
}

function Row({ label, mono, children }: { label: string; mono?: boolean; children: ReactNode }) {
  return (
    <>
      <dt className="text-label-2">{label}</dt>
      <dd className={mono ? "selectable break-all font-mono text-label-1" : "selectable text-label-1"}>{children}</dd>
    </>
  );
}
