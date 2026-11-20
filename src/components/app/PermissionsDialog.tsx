import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import { t } from "@/lib/i18n";
import { formatOctal, formatSymbolic, parseOctal, PERMISSION_BITS, toggleBit } from "@/lib/permissions";

/** Permissions of one or more remote entries: octal text and the nine
 *  read/write/execute boxes, kept in step. `folders` shows the option to
 *  apply to everything inside folders too. */
export function PermissionsDialog({
  title,
  initial,
  folders,
  onApply,
  onClose,
}: {
  title: string;
  initial: number;
  folders: boolean;
  onApply: (mode: number, recursive: boolean) => void;
  onClose: () => void;
}) {
  const [mode, setMode] = useState(initial & 0o777);
  const [text, setText] = useState(formatOctal(initial & 0o777));
  const [recursive, setRecursive] = useState(false);
  // The special bits (setuid, setgid, sticky) are kept when the text has 4 digits.
  const [special, setSpecial] = useState(initial & 0o7000);
  const parsed = parseOctal(text);
  const valid = parsed !== null;

  const setBits = (next: number) => {
    setMode(next);
    setText(formatOctal(next | special));
  };
  const typed = (value: string) => {
    setText(value);
    const bits = parseOctal(value);
    if (bits !== null) {
      setMode(bits & 0o777);
      setSpecial(bits & 0o7000);
    }
  };

  const groups = [
    { label: t.permissionsOwner, keys: ["ur", "uw", "ux"] },
    { label: t.permissionsGroup, keys: ["gr", "gw", "gx"] },
    { label: t.permissionsOthers, keys: ["or", "ow", "ox"] },
  ];
  const kinds = [t.permissionsRead, t.permissionsWrite, t.permissionsExecute];

  return (
    <Overlay
      labelledBy="permissions-title"
      onDismiss={onClose}
      onSubmit={() => valid && onApply(parsed, recursive)}
      className="max-w-sm p-6"
    >
      <h2 id="permissions-title" className="selectable text-title-3 break-all">
        {title}
      </h2>
      <table className="mt-4 w-full text-callout">
        <thead>
          <tr className="text-left text-label-2">
            <th scope="col" className="font-normal" />
            {kinds.map((kind) => (
              <th key={kind} scope="col" className="text-center font-normal">
                {kind}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {groups.map((group) => (
            <tr key={group.label} className="h-8">
              <th scope="row" className="text-left font-normal text-label-1">
                {group.label}
              </th>
              {group.keys.map((key, index) => {
                const bit = PERMISSION_BITS.find((candidate) => candidate.key === key)!.bit;
                return (
                  <td key={key} className="text-center">
                    <input
                      type="checkbox"
                      aria-label={`${group.label} ${kinds[index]}`}
                      checked={(mode & bit) !== 0}
                      onChange={() => setBits(toggleBit(mode, bit))}
                      className="accent-(--accent)"
                    />
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      <label className="mt-3 flex items-center gap-3 text-callout text-label-2">
        <span className="shrink-0">{t.permissionsOctal}</span>
        <Input
          value={text}
          onChange={(event) => typed(event.target.value)}
          aria-invalid={!valid}
          spellCheck={false}
          maxLength={4}
          className="w-24 font-mono"
        />
        <span className="font-mono text-label-3">{formatSymbolic(mode)}</span>
      </label>
      {!valid && <p className="mt-1 text-callout text-danger">{t.permissionsOctalInvalid}</p>}
      {folders && (
        <label className="mt-3 flex items-start gap-2 text-callout text-label-2">
          <input
            type="checkbox"
            checked={recursive}
            onChange={(event) => setRecursive(event.target.checked)}
            className="mt-1 accent-(--accent)"
          />
          <span>{t.permissionsRecursive}</span>
        </label>
      )}
      <div className="mt-6 flex justify-end gap-2">
        <Button type="button" variant="secondary" onClick={onClose}>
          {t.cancel}
        </Button>
        <Button type="submit" disabled={!valid}>
          {t.permissionsApply}
        </Button>
      </div>
    </Overlay>
  );
}
