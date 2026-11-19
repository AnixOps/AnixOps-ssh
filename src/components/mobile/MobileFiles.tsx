import {
  ChevronLeft,
  ChevronRight,
  Download,
  File as FileIcon,
  Folder,
  FolderPlus,
  LoaderCircle,
  Pencil,
  RefreshCw,
  Save,
  Share2,
  SquareTerminal,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { MobileNameSheet } from "@/components/mobile/MobileNameSheet";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { Menu } from "@/components/ui/menu";
import {
  cancelTransfer,
  openFiles,
  startTransfer,
  type FilesHandle,
  type Host,
  type RemoteFile,
  type TransferEvent,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { crumbs, formatSize, joinPath, parentPath, sortFiles } from "@/lib/mobileFiles";
import { deliver, forget, shareOptions, type ShareChoice } from "@/lib/mobileShare";
import { discardStaged, stageFile } from "@/lib/mobileStage";
import { cn } from "@/lib/utils";

type Phase = "preparing" | "running" | "done" | "failed" | "cancelled";

interface Transfer {
  key: number;
  name: string;
  upload: boolean;
  total: number;
  bytes: number;
  phase: Phase;
  message?: string;
  /** The engine's id while it runs. */
  id?: string;
  /** A finished download's file on the phone. */
  local?: string;
  /** Stops a file that is still being handed to the engine. */
  abort?: AbortController;
  /** What happened to a finished download: "saved", "shared" or the error. */
  handed?: string;
}

type Sheet = { kind: "mkdir" } | { kind: "rename"; entry: RemoteFile } | null;

/** The files of one host on the phone: a folder at a time, with a breadcrumb
 *  row, uploads from the system file picker and downloads that end in the
 *  share sheet or the Downloads folder. Sharing the host's connection with the
 *  terminal (one sign-in) is the engine's work; this screen only asks. */
export function MobileFiles({
  host,
  onBack,
  onTerminal,
}: {
  host: Host;
  onBack: () => void;
  /** Opens a terminal on the host; absent when one is already open. */
  onTerminal?: () => void;
}) {
  const handleRef = useRef<FilesHandle | null>(null);
  const pathRef = useRef("/");
  const requestRef = useRef(0);
  const pickerRef = useRef<HTMLInputElement>(null);
  const keyRef = useRef(0);
  const loadedRef = useRef(false);
  const confirm = useConfirm();

  const [path, setPath] = useState("/");
  const [entries, setEntries] = useState<RemoteFile[]>([]);
  const [state, setState] = useState<"connecting" | "loading" | "ready" | "failed">("connecting");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sheet, setSheet] = useState<Sheet>(null);
  const [transfers, setTransfers] = useState<Transfer[]>([]);

  const update = useCallback((key: number, change: Partial<Transfer>) => {
    setTransfers((current) => current.map((item) => (item.key === key ? { ...item, ...change } : item)));
  }, []);

  const go = useCallback((to: string, notFolder?: () => void) => {
    const handle = handleRef.current;
    if (!handle) return;
    const request = ++requestRef.current;
    setState("loading");
    setNotice(null);
    handle
      .list(to)
      .then((listing) => {
        if (request !== requestRef.current) return;
        pathRef.current = to;
        setPath(to);
        setEntries(sortFiles(listing));
        loadedRef.current = true;
        setState("ready");
      })
      .catch((reason) => {
        if (request !== requestRef.current) return;
        if (notFolder) {
          // A symbolic link that cannot be listed most likely points at a file.
          setState("ready");
          notFolder();
        } else if (loadedRef.current) {
          // A folder that cannot be read leaves the person where they were.
          setNotice(String(reason));
          setState("ready");
        } else {
          setError(String(reason));
          setState("failed");
        }
      });
  }, []);

  const connect = useCallback(() => {
    setState("connecting");
    setError(null);
    loadedRef.current = false;
    let closed = false;
    openFiles(host.id)
      .then((handle) => {
        if (closed) return handle.close();
        handleRef.current = handle;
        go(handle.home);
      })
      .catch((reason) => {
        if (closed) return;
        setError(String(reason));
        setState("failed");
      });
    return () => {
      closed = true;
      handleRef.current?.close();
      handleRef.current = null;
    };
  }, [host.id, go]);
  useEffect(connect, [connect]);

  const run = (action: Promise<void>, then?: () => void) =>
    action
      .then(() => {
        setNotice(null);
        then?.();
        go(pathRef.current);
      })
      .catch((reason) => setNotice(String(reason)));

  // ---- transfers --------------------------------------------------------

  const track = (key: number, event: TransferEvent, folder: string | null) => {
    if (event.kind === "progress") update(key, { bytes: event.bytes });
    else if (event.kind === "done") {
      update(key, { phase: "done" });
      if (folder === pathRef.current) go(folder);
    } else if (event.kind === "failed") update(key, { phase: "failed", message: event.message });
    else update(key, { phase: "cancelled" });
  };

  const upload = async (files: File[]) => {
    const folder = pathRef.current;
    for (const file of files) {
      const key = ++keyRef.current;
      const abort = new AbortController();
      setTransfers((current) => [
        ...current,
        { key, name: file.name, upload: true, total: file.size, bytes: 0, phase: "preparing", abort },
      ]);
      let staged: string;
      try {
        staged = await stageFile(file, (bytes) => update(key, { bytes }), abort.signal);
      } catch (reason) {
        update(key, abort.signal.aborted ? { phase: "cancelled" } : { phase: "failed", message: String(reason) });
        continue;
      }
      try {
        update(key, { phase: "running", bytes: 0, abort: undefined });
        const started = await startTransfer(
          { hostId: host.id, upload: true, remote: folder, local: staged },
          (event) => track(key, event, folder),
        );
        update(key, { id: started.id });
      } catch (reason) {
        discardStaged(staged);
        update(key, { phase: "failed", message: String(reason) });
      }
    }
  };

  const download = async (entry: RemoteFile) => {
    const key = ++keyRef.current;
    setTransfers((current) => [
      ...current,
      { key, name: entry.name, upload: false, total: entry.size, bytes: 0, phase: "running" },
    ]);
    try {
      const started = await startTransfer(
        { hostId: host.id, upload: false, remote: joinPath(pathRef.current, entry.name) },
        (event) => track(key, event, null),
      );
      update(key, { id: started.id, local: started.local });
    } catch (reason) {
      update(key, { phase: "failed", message: String(reason) });
    }
  };

  const stop = (item: Transfer) => {
    item.abort?.abort();
    if (item.id) void cancelTransfer(item.id).catch(() => {});
  };

  const dismiss = (item: Transfer) => {
    if (item.local) forget(item.local);
    setTransfers((current) => current.filter((other) => other.key !== item.key));
  };

  const hand = async (item: Transfer, choice: ShareChoice) => {
    if (!item.local) return;
    try {
      await deliver(item.local, choice);
      update(item.key, { handed: choice === "save" ? t.mobileSaved : t.mobileShared });
    } catch (reason) {
      update(item.key, { handed: String(reason instanceof Error ? reason.message : reason) });
    }
  };

  // ---- file actions -----------------------------------------------------

  const remove = async (entry: RemoteFile) => {
    const directory = entry.kind === "directory";
    const ok = await confirm({
      title: t.fileDeleteTitle.replace("{name}", entry.name),
      body: directory ? t.folderDeleteBody : t.fileDeleteBody,
      verb: t.delete,
    });
    const handle = handleRef.current;
    if (ok && handle) void run(handle.remove(joinPath(path, entry.name), directory));
  };

  const open = (entry: RemoteFile) => {
    if (entry.kind === "directory") go(joinPath(path, entry.name));
    // The link's own size is not the file's, so that download has no total.
    else if (entry.kind === "symlink") go(joinPath(path, entry.name), () => void download({ ...entry, size: 0 }));
    else void download(entry);
  };

  const options = shareOptions();
  const busy = state === "connecting" || state === "loading";

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col bg-bg">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="flex h-14 items-center gap-1 pr-safe pl-1">
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileBack} onClick={onBack} className="md:hidden">
            <ChevronLeft />
          </Button>
          <div className="min-w-0 flex-1 pl-2 md:pl-4">
            <h2 className="truncate text-body font-medium">{t.files}</h2>
            <p className="truncate text-callout text-label-2">{host.name}</p>
          </div>
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={t.upload}
            disabled={state !== "ready"}
            onClick={() => pickerRef.current?.click()}
          >
            <Upload />
          </Button>
          <Menu
            touch
            label={t.more}
            items={[
              {
                key: "folder",
                label: t.newFolder,
                icon: <FolderPlus />,
                disabled: state !== "ready",
                onSelect: () => setSheet({ kind: "mkdir" }),
              },
              { key: "refresh", label: t.refresh, icon: <RefreshCw />, disabled: busy, onSelect: () => go(pathRef.current) },
              ...(onTerminal
                ? [{ key: "terminal", label: t.mobileOpenTerminal, icon: <SquareTerminal />, onSelect: onTerminal }]
                : []),
            ]}
          />
        </div>
        <nav aria-label={t.path} className="px-safe flex overflow-x-auto pb-1">
          {crumbs(path).map((crumb, index, all) => (
            <button
              key={crumb.path}
              type="button"
              aria-current={index === all.length - 1 ? "location" : undefined}
              onClick={() => go(crumb.path)}
              className={cn(
                "flex min-h-11 min-w-11 shrink-0 items-center justify-center gap-1 px-1 text-callout transition-colors duration-micro ease-standard",
                index === all.length - 1 ? "text-label-1" : "text-label-2",
              )}
            >
              {index > 0 && <ChevronRight className="size-3.5 text-label-3" strokeWidth={1.5} />}
              {crumb.name}
            </button>
          ))}
        </nav>
      </header>

      <input
        ref={pickerRef}
        type="file"
        multiple
        hidden
        aria-label={t.upload}
        onChange={(event) => {
          const files = [...(event.target.files ?? [])];
          event.target.value = "";
          if (files.length > 0) void upload(files);
        }}
      />

      {notice && (
        <Notice className="mx-4 mt-3" tone="danger">
          {notice}
        </Notice>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto">
        {state === "failed" ? (
          <div className="px-safe flex h-full flex-col items-center justify-center gap-2 text-center">
            <p className="text-body font-medium">{t.filesFailed}</p>
            <p className="selectable max-w-xs break-words text-callout text-label-2">{error}</p>
            <p className="max-w-xs text-callout text-label-3">{t.filesAuthHint}</p>
            <Button variant="primary" size="lg" className="mt-4" onClick={connect}>
              <RefreshCw />
              {t.retry}
            </Button>
          </div>
        ) : state === "connecting" ? (
          <p role="status" className="flex h-full items-center justify-center gap-2 text-callout text-label-2">
            <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
            {t.filesConnecting}
          </p>
        ) : (
          <ul className={cn("pb-2", state === "loading" && "opacity-60")}>
            {path !== "/" && (
              <li className="border-b">
                <button
                  type="button"
                  onClick={() => go(parentPath(path))}
                  className="px-safe flex min-h-14 w-full items-center gap-3 text-left active:bg-fill-1"
                >
                  <Folder className="size-5 shrink-0 text-label-3" strokeWidth={1.5} />
                  <span className="text-body text-label-2">..</span>
                  <span className="sr-only">{t.parentFolder}</span>
                </button>
              </li>
            )}
            {entries.map((entry) => {
              const folder = entry.kind === "directory" || entry.kind === "symlink";
              return (
                <li key={entry.name} className="flex items-center border-b pr-2">
                  <button
                    type="button"
                    onClick={() => open(entry)}
                    className="px-safe flex min-h-14 min-w-0 flex-1 items-center gap-3 py-2 text-left transition-colors duration-micro ease-standard active:bg-fill-1"
                  >
                    {entry.kind === "directory" ? (
                      <Folder className="size-5 shrink-0 text-(--accent)" strokeWidth={1.5} />
                    ) : (
                      <FileIcon className="size-5 shrink-0 text-label-3" strokeWidth={1.5} />
                    )}
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-body text-label-1">{entry.name}</span>
                      <span className="block truncate text-callout text-label-2">
                        {[entry.kind === "directory" ? null : formatSize(entry.size), entry.modified]
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
                    </span>
                  </button>
                  <Menu
                    touch
                    label={`${t.more}: ${entry.name}`}
                    items={[
                      ...(folder
                        ? []
                        : [
                            {
                              key: "download",
                              label: t.download,
                              icon: <Download />,
                              onSelect: () => void download(entry),
                            },
                          ]),
                      { key: "rename", label: t.rename, icon: <Pencil />, onSelect: () => setSheet({ kind: "rename", entry }) },
                      "separator" as const,
                      { key: "delete", label: t.delete, icon: <Trash2 />, danger: true, onSelect: () => void remove(entry) },
                    ]}
                  />
                </li>
              );
            })}
            {state === "ready" && entries.length === 0 && (
              <li className="px-safe py-10 text-center text-callout text-label-2">{t.emptyFolder}</li>
            )}
          </ul>
        )}
      </div>

      {transfers.length > 0 && (
        <section aria-label={t.transfers} className="px-safe pb-safe material-bar shrink-0 border-t">
          <ul className="max-h-48 divide-y overflow-y-auto">
            {transfers.map((item) => (
              <TransferRow
                key={item.key}
                item={item}
                options={options}
                onStop={() => stop(item)}
                onDismiss={() => dismiss(item)}
                onHand={(choice) => void hand(item, choice)}
              />
            ))}
          </ul>
        </section>
      )}

      {sheet && (
        <MobileNameSheet
          title={sheet.kind === "mkdir" ? t.newFolder : t.mobileRenameTitle.replace("{name}", sheet.entry.name)}
          label={t.folderName}
          initial={sheet.kind === "rename" ? sheet.entry.name : ""}
          verb={sheet.kind === "mkdir" ? t.newFolder : t.rename}
          onCancel={() => setSheet(null)}
          onDone={(name) => {
            const handle = handleRef.current;
            const current = sheet;
            setSheet(null);
            if (!handle) return;
            if (current.kind === "mkdir") void run(handle.mkdir(joinPath(path, name)));
            else if (name !== current.entry.name)
              void run(handle.rename(joinPath(path, current.entry.name), joinPath(path, name)));
          }}
        />
      )}
    </div>
  );
}

function TransferRow({
  item,
  options,
  onStop,
  onDismiss,
  onHand,
}: {
  item: Transfer;
  options: ShareChoice[];
  onStop: () => void;
  onDismiss: () => void;
  onHand: (choice: ShareChoice) => void;
}) {
  const active = item.phase === "preparing" || item.phase === "running";
  const percent = item.total > 0 ? Math.min(100, Math.round((item.bytes / item.total) * 100)) : 0;
  const status =
    item.phase === "preparing"
      ? t.mobilePreparing
      : item.phase === "running"
        ? item.total > 0
          ? `${formatSize(item.bytes)} / ${formatSize(item.total)}`
          : formatSize(item.bytes)
        : item.phase === "done"
          ? t.transferDone
          : item.phase === "cancelled"
            ? t.transferCancelled
            : `${t.transferFailed}: ${item.message ?? ""}`;
  const ready = item.phase === "done" && !item.upload && item.local;

  return (
    <li className="py-1">
      <div className="flex min-h-11 items-center gap-2">
        <div className="min-w-0 flex-1">
          <p className="truncate text-callout text-label-1">{item.name}</p>
          <p
            role="status"
            className={cn("truncate text-caption", item.phase === "failed" ? "text-danger" : "text-label-2")}
          >
            {item.handed ?? status}
          </p>
        </div>
        {active ? (
          <Button variant="quiet" size="icon-lg" aria-label={`${t.cancelTransfer}: ${item.name}`} onClick={onStop}>
            <X />
          </Button>
        ) : (
          <Button variant="quiet" size="icon-lg" aria-label={`${t.close}: ${item.name}`} onClick={onDismiss}>
            <X />
          </Button>
        )}
      </div>
      {active && (
        <div
          role="progressbar"
          aria-label={item.name}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          className="mb-2 h-1 overflow-hidden rounded-pill bg-fill-2"
        >
          <div className="h-full bg-accent-fill transition-[width] duration-micro ease-standard" style={{ width: `${percent}%` }} />
        </div>
      )}
      {ready && options.length > 0 && (
        <div className="mb-2 flex gap-2">
          {options.includes("save") && (
            <Button variant="secondary" size="lg" className="flex-1" onClick={() => onHand("save")}>
              <Save />
              {t.mobileSaveDownloads}
            </Button>
          )}
          {options.includes("share") && (
            <Button variant="secondary" size="lg" className="flex-1" onClick={() => onHand("share")}>
              <Share2 />
              {t.mobileShare}
            </Button>
          )}
        </div>
      )}
    </li>
  );
}
