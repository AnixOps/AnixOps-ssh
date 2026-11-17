import { ChevronRight, CircleAlert, FileText, LoaderCircle, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Overlay } from "@/components/ui/overlay";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import {
  applyImportHosts,
  pickImportFile,
  previewImportHosts,
  type Group,
  type HostsImportApplyResult,
  type HostsImportPreviewResult,
  type ImportDuplicatePolicy,
  type ImportFormat,
  type ImportSource,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import {
  allState,
  buildApply,
  decodeChoice,
  defaultSelection,
  describeGroup,
  describeTarget,
  encodeChoice,
  importCount,
  importable,
  setAll,
  tally,
  toggle,
  type GroupChoice,
} from "@/lib/importSelection";
import { cn } from "@/lib/utils";

const FORMATS: { id: ImportFormat; name: string }[] = [
  { id: "openssh", name: "OpenSSH config" },
  { id: "putty", name: "PuTTY" },
  { id: "securecrt", name: "SecureCRT" },
  { id: "mobaxterm", name: "MobaXterm" },
  { id: "xshell", name: "Xshell" },
  { id: "finalshell", name: "FinalShell" },
  { id: "termius", name: "Termius" },
  { id: "csv", name: "CSV" },
];

const formatName = (id: ImportFormat | null) => FORMATS.find((format) => format.id === id)?.name ?? "";

const sourceName = (source: ImportSource) =>
  source.kind === "ssh_config" ? "~/.ssh/config" : (source.path.split(/[\\/]/).pop() ?? source.path);

/** Imports hosts from another SSH client: pick a source, review the hosts
 *  found, choose where they go and what happens to duplicates, then merge.
 *  Nothing is read until the user picks a file or presses the ~/.ssh/config
 *  button, and nothing that is already stored is ever removed. */
export function ImportDialog({
  groups,
  onChanged,
  onClose,
}: {
  groups: Group[];
  /** Hosts were added or updated: refresh the host list. */
  onChanged: () => void;
  onClose: () => void;
}) {
  const [source, setSource] = useState<ImportSource | null>(null);
  const [preview, setPreview] = useState<HostsImportPreviewResult | null>(null);
  const [override, setOverride] = useState<ImportFormat | null>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [choice, setChoice] = useState<GroupChoice>({ mode: "source" });
  const [policy, setPolicy] = useState<ImportDuplicatePolicy>("skip");
  const [busy, setBusy] = useState<"reading" | "importing" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<HostsImportApplyResult | null>(null);
  const [warningsOpen, setWarningsOpen] = useState(false);
  const firstRef = useRef<HTMLButtonElement>(null);

  useEffect(() => firstRef.current?.focus(), []);

  const load = async (next: ImportSource, format: ImportFormat | null) => {
    setBusy("reading");
    setError(null);
    try {
      const found = await previewImportHosts(next, format);
      setSource(next);
      setOverride(format);
      setPreview(found);
      setSelected(defaultSelection(found.hosts));
      setWarningsOpen(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  };

  const chooseFile = async () => {
    setError(null);
    try {
      const path = await pickImportFile();
      if (path) await load({ kind: "file", path }, null);
    } catch (reason) {
      setError(String(reason));
    }
  };

  const hosts = preview?.hosts ?? [];
  const counts = useMemo(() => tally(hosts, selected, policy), [hosts, selected, policy]);
  const count = importCount(counts);

  const run = async () => {
    if (!preview) return;
    setBusy("importing");
    setError(null);
    try {
      const outcome = await applyImportHosts(buildApply(hosts, selected, choice, policy));
      setResult(outcome);
      onChanged();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  };

  const back = () => {
    setSource(null);
    setPreview(null);
    setOverride(null);
    setError(null);
  };

  return (
    <Overlay labelledBy="import-title" onDismiss={onClose} className="flex max-h-full max-w-3xl flex-col">
      <h2 id="import-title" className="px-6 pt-6 text-title-3">
        {result ? t.importResultTitle : t.importHosts}
      </h2>

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-6 pt-4 pb-2">
        {result ? (
          <Summary result={result} />
        ) : !preview ? (
          <>
            <p className="text-callout font-normal text-label-2">{t.importIntro}</p>
            <div className="flex flex-wrap items-center gap-2">
              <Button ref={firstRef} variant="primary" disabled={busy !== null} onClick={() => void chooseFile()}>
                <FileText />
                {t.importChooseFile}
              </Button>
              <Button variant="secondary" disabled={busy !== null} onClick={() => void load({ kind: "ssh_config" }, null)}>
                {busy === "reading" ? <LoaderCircle className="animate-spin motion-reduce:animate-none" /> : null}
                {t.importSshConfig}
              </Button>
            </div>
            <p className="text-callout text-label-3">{t.importSshConfigHint}</p>
          </>
        ) : (
          <>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
              <span className="flex min-w-0 items-center gap-2 text-callout text-label-1">
                <FileText className="size-4 shrink-0 text-label-2" strokeWidth={1.5} />
                <span className="selectable truncate font-mono">{source ? sourceName(source) : ""}</span>
              </span>
              <span className="text-callout text-label-2">
                {preview.format
                  ? `${t.importDetected}: ${formatName(preview.detected)} · ${hosts.length} ${t.importFound}`
                  : t.importNotDetected}
              </span>
              <label className="ml-auto flex items-center gap-2 text-callout text-label-2">
                {t.importFormat}
                <Select
                  value={override ?? ""}
                  disabled={busy !== null}
                  onChange={(event) => source && void load(source, (event.target.value || null) as ImportFormat | null)}
                  aria-label={t.importFormat}
                  className="h-7 w-44 text-callout"
                >
                  <option value="">{t.importAuto}</option>
                  {FORMATS.map((format) => (
                    <option key={format.id} value={format.id}>
                      {format.name}
                    </option>
                  ))}
                </Select>
              </label>
            </div>

            <HostTable hosts={hosts} selected={selected} onChange={setSelected} />
            {hosts.length === 0 && <p className="text-callout text-label-2">{t.importNothing}</p>}

            {(preview.warnings.length > 0 || preview.skipped > 0) && (
              <div>
                <button
                  type="button"
                  aria-expanded={warningsOpen}
                  onClick={() => setWarningsOpen((open) => !open)}
                  className="flex items-center gap-1 rounded-xs text-callout text-label-2 hover:text-label-1"
                >
                  <ChevronRight
                    className={cn("size-3.5 transition-transform duration-toggle ease-standard", warningsOpen && "rotate-90")}
                    strokeWidth={1.5}
                  />
                  {t.importWarnings} ({preview.warnings.length})
                  {preview.skipped > 0 && (
                    <span className="font-normal text-label-3">
                      · {preview.skipped} {t.importNotImported}
                    </span>
                  )}
                </button>
                {warningsOpen && (
                  <ul className="selectable mt-1.5 max-h-32 space-y-1 overflow-y-auto pl-5 text-callout text-label-2">
                    {preview.warnings.map((warning, index) => (
                      <li key={index} className="break-words">
                        {warning}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label={t.importTargetGroup}>
                <Select value={encodeChoice(choice)} onChange={(event) => setChoice(decodeChoice(event.target.value))}>
                  <option value="source">{t.importGroupSource}</option>
                  <option value="none">{t.importGroupNone}</option>
                  {groups.map((group) => (
                    <option key={group.id} value={encodeChoice({ mode: "existing", groupId: group.id })}>
                      {group.name}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label={t.importDuplicates}>
                <Select value={policy} onChange={(event) => setPolicy(event.target.value as ImportDuplicatePolicy)}>
                  <option value="skip">{t.importPolicySkip}</option>
                  <option value="update">{t.importPolicyUpdate}</option>
                  <option value="keep-both">{t.importPolicyKeepBoth}</option>
                </Select>
              </Field>
            </div>
            <p className="text-callout text-label-3">{t.importDuplicatesHint}</p>
          </>
        )}

        {error && (
          <p role="alert" className="flex items-start gap-2 rounded-sm bg-danger-soft px-3 py-2 text-callout text-danger">
            <CircleAlert className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} />
            <span className="selectable min-w-0 break-words">
              {t.importFailed}: {error}
            </span>
          </p>
        )}
      </div>

      <div className="flex items-center gap-2 px-6 pt-3 pb-6">
        <p className="flex min-w-0 items-center gap-1.5 text-callout text-label-3">
          <ShieldCheck className="size-4 shrink-0" strokeWidth={1.5} />
          <span>{t.importNoSecrets}</span>
        </p>
        <div className="flex-1" />
        {result ? (
          <Button variant="primary" onClick={onClose}>
            {t.importDone}
          </Button>
        ) : (
          <>
            {preview && (
              <>
                <span className="hidden text-callout text-label-2 sm:inline">
                  {t.importCreated} {counts.create} · {t.importUpdated} {counts.update} · {t.skipped} {counts.skip}
                </span>
                <Button variant="quiet" disabled={busy !== null} onClick={back}>
                  {t.importAnother}
                </Button>
              </>
            )}
            <Button variant="secondary" onClick={onClose}>
              {t.cancel}
            </Button>
            {preview && (
              <Button variant="primary" disabled={busy !== null || count === 0} onClick={() => void run()}>
                {busy === "importing" && <LoaderCircle className="animate-spin motion-reduce:animate-none" />}
                {t.importAction} ({count})
              </Button>
            )}
          </>
        )}
      </div>
    </Overlay>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1.5 block text-callout text-label-2">{label}</span>
      {children}
    </label>
  );
}

function HostTable({
  hosts,
  selected,
  onChange,
}: {
  hosts: HostsImportPreviewResult["hosts"];
  selected: Set<number>;
  onChange: (next: Set<number>) => void;
}) {
  const state = allState(selected, hosts);
  const allRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (allRef.current) allRef.current.indeterminate = state === "some";
  }, [state]);

  if (hosts.length === 0) return null;
  return (
    <div className="max-h-[38vh] overflow-y-auto rounded-sm ring-1 ring-separator">
      <table className="w-full table-fixed border-collapse text-left">
        <thead className="sticky top-0 bg-bg-elevated">
          <tr className="text-callout text-label-2">
            <th className="w-10 px-3 py-2">
              <input
                ref={allRef}
                type="checkbox"
                className="size-4 accent-(--accent)"
                aria-label={t.importSelectAll}
                checked={state === "all"}
                onChange={() => onChange(setAll(hosts, state !== "all"))}
              />
            </th>
            <th className="px-2 py-2 font-medium">{t.hostName}</th>
            <th className="px-2 py-2 font-medium">{t.importColHost}</th>
            <th className="hidden w-32 px-2 py-2 font-medium sm:table-cell">{t.importColGroup}</th>
            <th className="w-36 px-2 py-2" />
          </tr>
        </thead>
        <tbody className="divide-y divide-separator">
          {hosts.map((host, index) => {
            const usable = importable(host);
            return (
              <tr key={index} className={cn("h-10", !usable && "opacity-60")}>
                <td className="px-3">
                  <input
                    type="checkbox"
                    className="size-4 accent-(--accent)"
                    aria-label={`${t.importSelectHost}: ${host.label}`}
                    checked={selected.has(index)}
                    disabled={!usable}
                    onChange={() => onChange(toggle(selected, hosts, index))}
                  />
                </td>
                <td className="selectable truncate px-2 text-callout text-label-1" title={host.label}>
                  {host.label}
                </td>
                <td className="selectable truncate px-2 font-mono text-callout text-label-2" title={describeTarget(host)}>
                  {describeTarget(host)}
                </td>
                <td className="hidden truncate px-2 text-callout text-label-2 sm:table-cell" title={describeGroup(host.group_path)}>
                  {describeGroup(host.group_path) || "—"}
                </td>
                <td className="px-2">
                  <span className="flex flex-wrap justify-end gap-1">
                    {host.exists && <Badge tone="neutral">{t.importExists}</Badge>}
                    {(host.openssh_only?.length ?? 0) > 0 && (
                      <Tooltip label={`${t.importOpensshOnlyTip}${host.openssh_only?.join(", ")}`}>
                        <span tabIndex={0}>
                          <Badge tone="accent">{t.importOpensshOnly}</Badge>
                        </span>
                      </Tooltip>
                    )}
                    {host.problem && (
                      <Tooltip label={host.problem}>
                        <span tabIndex={0}>
                          <Badge tone="danger">{t.importProblem}</Badge>
                        </span>
                      </Tooltip>
                    )}
                  </span>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function Badge({ tone, children }: { tone: "neutral" | "accent" | "danger"; children: ReactNode }) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center whitespace-nowrap rounded-pill px-2 text-callout font-medium",
        tone === "neutral" && "bg-fill-2 text-label-2",
        tone === "accent" && "bg-accent-soft text-(--accent)",
        tone === "danger" && "bg-danger-soft text-danger",
      )}
    >
      {children}
    </span>
  );
}

function Summary({ result }: { result: HostsImportApplyResult }) {
  const rows: [string, number][] = [
    [t.importCreated, result.created],
    [t.importUpdated, result.updated],
    [t.importUnchanged, result.unchanged],
    [t.importGroupsCreated, result.groups_created],
    [t.importRejected, result.rejected.length],
  ];
  return (
    <div className="space-y-3" role="status">
      <dl className="grid grid-cols-2 gap-x-6 gap-y-1.5 sm:grid-cols-3">
        {rows.map(([label, value]) => (
          <div key={label} className="flex items-baseline justify-between gap-3 border-b border-separator py-1.5">
            <dt className="text-callout font-normal text-label-2">{label}</dt>
            <dd className="text-callout text-label-1 tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>
      {(result.rejected.length > 0 || result.warnings.length > 0) && (
        <ul className="selectable max-h-40 space-y-1 overflow-y-auto text-callout text-label-2">
          {result.rejected.map((entry, index) => (
            <li key={`r${index}`} className="break-words text-danger">
              {entry.label}: {entry.reason}
            </li>
          ))}
          {result.warnings.map((warning, index) => (
            <li key={`w${index}`} className="break-words">
              {warning}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
