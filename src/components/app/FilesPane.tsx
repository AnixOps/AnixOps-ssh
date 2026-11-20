import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  ArrowDown,
  ArrowUp,
  Check,
  CircleAlert,
  Download,
  File,
  Folder,
  FolderPlus,
  FolderSearch,
  FolderUp,
  Link2,
  LoaderCircle,
  Info,
  Pencil,
  RotateCw,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type MouseEvent, type ReactNode, type RefObject } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { FilePropertiesDialog } from "@/components/app/FilePropertiesDialog";
import { Menu } from "@/components/ui/menu";
import { Tooltip } from "@/components/ui/tooltip";
import {
  cancelTransfer,
  isTauri,
  openFiles,
  pickUploads,
  revealLocal,
  startTransfer,
  type FilesHandle,
  type RemoteFile,
  type TransferEvent,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { shortcutHint } from "@/lib/shortcutTable";
import { isMac } from "@/lib/shortcuts";
import {
  clickSelect,
  emptySelection,
  neighbour,
  selectAll,
  selectOnly,
  targetsOf,
  type ClickModifiers,
  type Selection,
} from "@/lib/fileSelection";
import { resolveRenameTarget } from "@/lib/renameTarget";
import { appear, duration, ease } from "@/lib/motion";
import {
  DEFAULT_TRANSFER_LIMIT,
  failureMessage,
  formatDuration,
  nextRate,
  pickToStart,
  secondsLeft,
  type QueueState,
  type Rate,
} from "@/lib/transferQueue";
import { baseName, formatSize, locate, localName, parentPath, resolvePath, uploadConflicts } from "@/lib/remotePath";
import { cn } from "@/lib/utils";

interface TransferItem {
  /** The pane's own id; `backendId` is the one the backend gave it once started. */
  id: string;
  backendId?: string;
  name: string;
  upload: boolean;
  local: string;
  /** The remote file or folder, for the messages. */
  remote: string;
  /** What `startTransfer` is asked for: the remote folder (upload) or path (download). */
  target: string;
  /** The folder listed when an upload was queued, refreshed when it ends. */
  dir: string;
  state: QueueState;
  bytes: number;
  total: number | null;
  rate?: Rate;
  message?: string;
}

type Editing = { kind: "mkdir"; value: string } | { kind: "rename"; from: string; value: string };

const joinPath = (dir: string, name: string) => (dir.endsWith("/") ? dir + name : `${dir}/${name}`);

function sortEntries(entries: RemoteFile[]): RemoteFile[] {
  return [...entries].sort((a, b) => {
    const aDir = a.kind === "directory" ? 0 : 1;
    const bDir = b.kind === "directory" ? 0 : 1;
    return aDir - bDir || a.name.localeCompare(b.name);
  });
}

export function FilesPane({
  hostId,
  visible,
  active,
  onClose,
}: {
  hostId: string;
  visible: boolean;
  active: boolean;
  onClose: () => void;
}) {
  const [handle, setHandle] = useState<FilesHandle | null>(null);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [cwd, setCwd] = useState("");
  const [pathInput, setPathInput] = useState("");
  const [entries, setEntries] = useState<RemoteFile[]>([]);
  const [loading, setLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Selection>(emptySelection);
  /** The entry whose Properties dialog is open. */
  const [properties, setProperties] = useState<RemoteFile | null>(null);
  /** An entry to scroll to and focus once the listing renders. */
  const [reveal, setReveal] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const confirm = useConfirm();
  const [transfers, setTransfers] = useState<TransferItem[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const paneRef = useRef<HTMLElement>(null);
  /** Counts the items queued, for their ids. */
  const queued = useRef(0);
  /** Items already handed to `begin`, so a re-render does not start one twice. */
  const started = useRef(new Set<string>());
  /** Items cancelled while their start call was still on its way. */
  const cancelAsked = useRef(new Set<string>());
  const cwdRef = useRef("");
  /** The listing of `cwdRef`, for checking what an upload would replace. */
  const entriesRef = useRef<RemoteFile[]>([]);

  const navigate = useCallback(
    async (files: FilesHandle, path: string, typed = false, notFolder?: () => void) => {
      setLoading(true);
      setActionError(null);
      try {
        // A typed location may name a file: show its folder with it selected.
        const found = typed ? await locate(files.list, path) : { dir: path, entries: await files.list(path), select: null };
        setEntries(sortEntries(found.entries));
        entriesRef.current = found.entries;
        setCwd(found.dir);
        setPathInput(found.dir);
        cwdRef.current = found.dir;
        setSelection(found.select ? selectOnly(found.select) : emptySelection);
        setReveal(found.select);
        setEditing(null);
      } catch (reason) {
        setPathInput(cwdRef.current);
        // A symbolic link that cannot be listed most likely points at a file.
        if (notFolder) notFolder();
        else setActionError(String(reason));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (!reveal) return;
    const row = paneRef.current?.querySelector<HTMLElement>(`[role="option"][data-name="${CSS.escape(reveal)}"]`);
    row?.scrollIntoView({ block: "nearest" });
    row?.focus({ preventScroll: true });
    setReveal(null);
  }, [reveal, entries]);

  useEffect(() => {
    let disposed = false;
    let opened: FilesHandle | null = null;
    setConnectError(null);
    setHandle(null);
    openFiles(hostId)
      .then((files) => {
        if (disposed) return files.close();
        opened = files;
        setHandle(files);
        void navigate(files, files.home);
      })
      .catch((reason) => !disposed && setConnectError(String(reason)));
    return () => {
      disposed = true;
      opened?.close();
    };
  }, [hostId, attempt, navigate]);

  const run = async (action: () => Promise<void>) => {
    if (!handle) return;
    try {
      await action();
      await navigate(handle, cwd);
    } catch (reason) {
      setActionError(String(reason));
    }
  };

  const updateTransfer = useCallback((id: string, patch: Partial<TransferItem>) => {
    setTransfers((current) => current.map((item) => (item.id === id ? { ...item, ...patch } : item)));
  }, []);

  /** Starts a queued item: it counts against the host's limit from here. */
  const begin = useCallback(
    async (item: TransferItem) => {
      updateTransfer(item.id, { state: "running", bytes: 0, rate: undefined, message: undefined, backendId: undefined });
      let ready = false;
      const early: TransferEvent[] = [];
      const onEvent = (event: TransferEvent) => (ready ? apply(event) : early.push(event));
      const apply = (event: TransferEvent) => {
        // Whatever arrives after a cancel is ignored.
        const settle = (change: (current: TransferItem) => Partial<TransferItem>) =>
          setTransfers((all) =>
            all.map((current) => (current.id === item.id && current.state === "running" ? { ...current, ...change(current) } : current)),
          );
        if (event.kind === "progress") {
          settle((current) => ({
            bytes: event.bytes,
            total: event.total ?? current.total,
            rate: nextRate(current.rate, event.bytes, Date.now()),
          }));
        } else if (event.kind === "done") {
          settle((current) => ({ state: "done", bytes: current.total ?? current.bytes }));
          if (item.upload && handle && cwdRef.current === item.dir) void navigate(handle, item.dir);
        } else if (event.kind === "failed") {
          settle(() => ({ state: "failed", message: failureMessage(item.remote, event.message) }));
        } else settle(() => ({ state: "cancelled" }));
      };
      try {
        const started = await startTransfer(
          { hostId, upload: item.upload, remote: item.target, local: item.upload ? item.local : undefined },
          onEvent,
        );
        updateTransfer(item.id, { backendId: started.id });
        if (cancelAsked.current.delete(item.id)) void cancelTransfer(started.id);
        ready = true;
        early.forEach(apply);
      } catch (reason) {
        updateTransfer(item.id, { state: "failed", message: failureMessage(item.remote, reason) });
      }
    },
    [handle, hostId, navigate, updateTransfer],
  );

  // Starts queued transfers while the host has room, oldest first.
  useEffect(() => {
    for (const id of pickToStart(transfers, DEFAULT_TRANSFER_LIMIT)) {
      const item = transfers.find((candidate) => candidate.id === id);
      if (!item || started.current.has(id)) continue;
      started.current.add(id);
      void begin(item);
    }
  }, [transfers, begin]);

  const transfer = useCallback(
    (upload: boolean, remote: string, local: string | undefined, total: number | null) => {
      const source = upload ? (local ?? "") : remote;
      const item: TransferItem = {
        id: `transfer-${++queued.current}`,
        name: localName(source),
        upload,
        local: local ?? "",
        remote: upload ? joinPath(remote, localName(source)) : remote,
        target: remote,
        dir: cwdRef.current,
        state: "queued",
        bytes: 0,
        total,
      };
      setTransfers((current) => [...current, item]);
    },
    [],
  );

  const cancelItem = (item: TransferItem) => {
    if (item.state === "running") {
      if (item.backendId) void cancelTransfer(item.backendId);
      else cancelAsked.current.add(item.id);
    }
    updateTransfer(item.id, { state: "cancelled" });
  };

  const retryItem = (item: TransferItem) => {
    started.current.delete(item.id);
    updateTransfer(item.id, { state: "queued", bytes: 0, rate: undefined, message: undefined, backendId: undefined });
  };

  const uploadPaths = useCallback(
    async (paths: string[]) => {
      const dir = cwdRef.current;
      // Ask once before anything in the folder is overwritten.
      const conflicts = uploadConflicts(paths, entriesRef.current);
      if (conflicts.length > 0) {
        const title =
          conflicts.length === 1
            ? t.uploadReplaceTitle.replace("{name}", conflicts[0])
            : t.uploadReplaceManyTitle.replace("{n}", String(conflicts.length)).replace("{dir}", dir);
        const items = conflicts.length > 1 ? conflicts : undefined;
        if (!(await confirm({ title, body: t.uploadReplaceBody, verb: t.replace, items }))) return;
      }
      for (const path of paths) transfer(true, dir, path, null);
    },
    [confirm, transfer],
  );

  // Files dropped from the system onto the pane are uploaded to the current folder.
  useEffect(() => {
    if (!isTauri || !active || !handle) return;
    const inside = (x: number, y: number) => {
      const rect = paneRef.current?.getBoundingClientRect();
      if (!rect) return false;
      const scale = window.devicePixelRatio;
      return x >= rect.left * scale && x <= rect.right * scale && y >= rect.top * scale && y <= rect.bottom * scale;
    };
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "over") setDragOver(inside(payload.position.x, payload.position.y));
      else if (payload.type === "drop") {
        setDragOver(false);
        if (inside(payload.position.x, payload.position.y)) void uploadPaths(payload.paths);
      } else setDragOver(false);
    });
    return () => void unlisten.then((stop) => stop());
  }, [active, handle, uploadPaths]);

  const open = (entry: RemoteFile) => {
    if (!handle) return;
    const path = joinPath(cwd, entry.name);
    if (entry.kind === "directory") void navigate(handle, path);
    else if (entry.kind === "symlink") void navigate(handle, path, false, () => transfer(false, path, undefined, null));
    else transfer(false, path, undefined, entry.size);
  };

  /** Renames `name` in the shown folder, or moves it when `input` has a `/`. */
  const renameEntry = async (name: string, input: string) => {
    if (!handle) return;
    const from = joinPath(cwd, name);
    const target = resolveRenameTarget(input, from, cwd, handle.home);
    if (!target.ok) {
      if (target.reason === "inside") setActionError(t.renameInside.replace("{path}", from));
      else if (target.reason === "invalid") setActionError(t.renameInvalid);
      return;
    }
    const toDir = parentPath(target.to);
    const toName = baseName(target.to);
    let siblings = entriesRef.current;
    if (toDir !== cwd) {
      try {
        siblings = await handle.list(toDir);
      } catch (reason) {
        setActionError(`${t.renameMissingFolder.replace("{path}", toDir)} ${String(reason)}`);
        return;
      }
    }
    const moved = entriesRef.current.find((entry) => entry.name === name);
    const existing = siblings.find((entry) => entry.name === toName);
    if (existing) {
      // Only a file may replace a file; a folder is never replaced.
      if (existing.kind === "directory" || moved?.kind === "directory") {
        setActionError(t.renameExists.replace("{path}", target.to));
        return;
      }
      const title = t.renameReplaceTitle.replace("{path}", target.to);
      if (!(await confirm({ title, body: t.renameReplaceBody, verb: t.replace }))) return;
      await run(async () => {
        await handle.remove(target.to, false);
        await handle.rename(from, target.to);
      });
      return;
    }
    await run(() => handle.rename(from, target.to));
  };

  const submitEdit = () => {
    if (!editing || !handle) return;
    const value = editing.value.trim();
    if (!value) return;
    if (editing.kind === "mkdir") {
      if (value.includes("/")) return;
      void run(() => handle.mkdir(joinPath(cwd, value)));
    } else if (value !== editing.from) void renameEntry(editing.from, value);
    setEditing(null);
  };

  const order = entries.map((entry) => entry.name);
  const selectedNames = new Set(selection.names);

  const downloadEntries = (names: string[]) => {
    for (const name of names) {
      const entry = entries.find((candidate) => candidate.name === name);
      if (entry) transfer(false, joinPath(cwd, name), undefined, entry.kind === "file" ? entry.size : null);
    }
  };

  /** Deletes after one confirmation naming the count and the first names. */
  const removeEntries = async (names: string[]) => {
    if (!handle) return;
    const picked = names
      .map((name) => entries.find((entry) => entry.name === name))
      .filter((entry): entry is RemoteFile => entry !== undefined);
    if (picked.length === 0) return;
    const folders = picked.some((entry) => entry.kind === "directory");
    const single = picked.length === 1 ? picked[0] : null;
    const confirmed = await confirm(
      single
        ? {
            title: t.fileDeleteTitle.replace("{name}", single.name),
            body: single.kind === "directory" ? t.folderDeleteBody : t.fileDeleteBody,
            verb: t.delete,
          }
        : {
            title: t.filesDeleteManyTitle.replace("{n}", String(picked.length)),
            body: folders ? t.filesDeleteManyFoldersBody : t.filesDeleteManyBody,
            verb: t.delete,
            items: picked.map((entry) => entry.name),
          },
    );
    if (!confirmed) return;
    // Every entry is tried; the ones that fail are named with the reason.
    const failures: string[] = [];
    for (const entry of picked) {
      try {
        await handle.remove(joinPath(cwd, entry.name), entry.kind === "directory");
      } catch (reason) {
        failures.push(`${joinPath(cwd, entry.name)}: ${String(reason)}`);
      }
    }
    await navigate(handle, cwd);
    if (failures.length > 0) {
      setActionError(
        picked.length === 1 ? failures[0] : `${t.filesDeleteSomeFailed.replace("{n}", String(failures.length))}\n${failures.join("\n")}`,
      );
    }
  };

  const click = (name: string, event: { ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) => {
    const modifiers: ClickModifiers = { toggle: isMac ? event.metaKey : event.ctrlKey, shift: event.shiftKey };
    setSelection((current) => clickSelect(current, order, name, modifiers));
  };

  const move = (name: string, step: 1 | -1, extend: boolean) => {
    const next = neighbour(order, name, step);
    if (!next) return;
    setSelection((current) =>
      extend ? clickSelect(current.anchor ? current : selectOnly(name), order, next, { toggle: false, shift: true }) : selectOnly(next),
    );
    setReveal(next);
  };

  const applyMode = async (entry: RemoteFile, mode: number, recursive: boolean) => {
    if (!handle) return;
    await handle.chmod(joinPath(cwd, entry.name), mode, recursive);
    await navigate(handle, cwd);
  };

  if (connectError) {
    return (
      <PaneFrame paneRef={paneRef} visible={visible} onClose={onClose}>
        <div className="flex flex-1 flex-col items-center px-6 pt-16 text-center">
          <CircleAlert className="mb-3 size-12 text-label-3" strokeWidth={1.5} />
          <p className="text-callout font-medium text-label-1">{t.filesFailed}</p>
          <p className="selectable mt-2 text-callout break-all text-label-2">{connectError}</p>
          <p className="mt-3 text-callout text-label-2">{t.filesAuthHint}</p>
          <Button variant="secondary" size="sm" className="mt-5" onClick={() => setAttempt((n) => n + 1)}>
            <RotateCw />
            {t.retry}
          </Button>
        </div>
      </PaneFrame>
    );
  }

  if (!handle) {
    return (
      <PaneFrame paneRef={paneRef} visible={visible} onClose={onClose}>
        <div className="flex flex-1 items-center justify-center gap-2 text-callout text-label-2">
          <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
          {t.filesConnecting}
        </div>
      </PaneFrame>
    );
  }

  const finished = transfers.some((item) => item.state !== "running" && item.state !== "queued");

  return (
    <PaneFrame
      paneRef={paneRef}
      visible={visible}
      onClose={onClose}
      actions={
        <>
          <IconButton label={t.parentFolder} onClick={() => void navigate(handle, parentPath(cwd))} disabled={cwd === "/"}>
            <FolderUp />
          </IconButton>
          <IconButton label={t.refresh} onClick={() => void navigate(handle, cwd)}>
            <RotateCw />
          </IconButton>
          <IconButton label={t.newFolder} onClick={() => setEditing({ kind: "mkdir", value: "" })}>
            <FolderPlus />
          </IconButton>
          <IconButton label={t.upload} onClick={() => void pickUploads().then(uploadPaths)}>
            <Upload />
          </IconButton>
        </>
      }
    >
      <div className="px-3 pt-2 pb-2">
        <Input
          value={pathInput}
          onChange={(event) => setPathInput(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && pathInput.trim())
              void navigate(handle, resolvePath(pathInput, cwdRef.current, handle.home), true);
            if (event.key === "Escape") setPathInput(cwd);
          }}
          aria-label={t.path}
          spellCheck={false}
          className="h-8 font-mono text-callout"
        />
      </div>

      {actionError && (
        <div className="mx-3 mb-2 flex items-start gap-1">
          <Notice className="min-w-0 flex-1 break-all">{actionError}</Notice>
          <IconButton label={t.close} onClick={() => setActionError(null)}>
            <X />
          </IconButton>
        </div>
      )}

      {selection.names.length > 1 && (
        <div role="status" className="mx-3 mb-2 flex items-center gap-1 rounded-sm bg-accent-soft py-1 pr-1 pl-3">
          <span className="mr-auto text-callout font-medium text-label-1">
            {t.selectedCount.replace("{n}", String(selection.names.length))}
          </span>
          <Button variant="quiet" size="sm" onClick={() => downloadEntries(selection.names)}>
            <Download />
            {t.download}
          </Button>
          <Button variant="danger" size="sm" onClick={() => void removeEntries(selection.names)}>
            <Trash2 />
            {t.delete}
          </Button>
          <IconButton label={t.clearSelection} onClick={() => setSelection(emptySelection)}>
            <X />
          </IconButton>
        </div>
      )}

      <div
        className={cn(
          "relative min-h-0 flex-1 overflow-y-auto px-2 pb-2 transition-opacity duration-micro",
          loading && "opacity-60",
        )}
        role="listbox"
        aria-multiselectable
        aria-label={cwd}
        onKeyDown={(event) => {
          // Ctrl/⌘+A selects every entry, only while an entry has the focus.
          if (!(event.target instanceof HTMLElement) || event.target.getAttribute("role") !== "option") return;
          if ((isMac ? event.metaKey : event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "a") {
            event.preventDefault();
            setSelection(selectAll(order));
          } else if (event.key === "Escape" && selection.names.length > 1) setSelection(emptySelection);
        }}
      >
        {editing?.kind === "mkdir" && (
          <EditRow
            icon={<Folder />}
            value={editing.value}
            placeholder={t.folderName}
            onChange={(value) => setEditing({ ...editing, value })}
            onSubmit={submitEdit}
            onCancel={() => setEditing(null)}
          />
        )}
        {entries.length === 0 && !loading && !editing && (
          <p className="pt-10 text-center text-callout text-label-2">{t.emptyFolder}</p>
        )}
        {entries.map((entry) =>
          editing?.kind === "rename" && editing.from === entry.name ? (
            <EditRow
              key={entry.name}
              icon={<EntryIcon entry={entry} />}
              value={editing.value}
              placeholder={t.renameHint}
              onChange={(value) => setEditing({ ...editing, value })}
              onSubmit={submitEdit}
              onCancel={() => setEditing(null)}
            />
          ) : (
            <EntryRow
              key={entry.name}
              entry={entry}
              selected={selectedNames.has(entry.name)}
              onSelect={(event) => click(entry.name, event)}
              onOpen={() => open(entry)}
              onMove={(step, extend) => move(entry.name, step, extend)}
              onDownload={() => downloadEntries(targetsOf(selection, entry.name))}
              onRename={() => setEditing({ kind: "rename", from: entry.name, value: entry.name })}
              onProperties={() => setProperties(entry)}
              onDelete={() => void removeEntries(targetsOf(selection, entry.name))}
            />
          ),
        )}

        {dragOver && (
          <div className="pointer-events-none absolute inset-2 flex items-center justify-center rounded-md bg-accent-soft ring-2 ring-(--accent)">
            <p className="flex items-center gap-2 text-callout font-medium text-(--accent)">
              <Upload className="size-4" strokeWidth={1.5} />
              {t.dropToUpload} {cwd}
            </p>
          </div>
        )}
      </div>

      {transfers.length > 0 && (
        <section className="max-h-56 shrink-0 overflow-y-auto border-t px-2 pt-2 pb-3">
          <div className="flex items-center px-2 pb-1">
            <h3 className="text-callout text-label-2">{t.transfers}</h3>
            {finished && (
              <Button
                variant="quiet"
                size="sm"
                className="ml-auto"
                onClick={() => setTransfers((current) => current.filter((item) => item.state === "running" || item.state === "queued"))}
              >
                {t.clearFinished}
              </Button>
            )}
          </div>
          <ul className="space-y-1">
            <AnimatePresence initial={false}>
            {transfers.map((item) => (
              <TransferRow key={item.id} item={item} onCancel={() => cancelItem(item)} onRetry={() => retryItem(item)} />
            ))}
            </AnimatePresence>
          </ul>
        </section>
      )}

      <AnimatePresence>
        {properties && (
          <FilePropertiesDialog
            entry={properties}
            dir={cwd}
            onApply={(mode, recursive) => applyMode(properties, mode, recursive)}
            onClose={() => setProperties(null)}
          />
        )}
      </AnimatePresence>
    </PaneFrame>
  );
}

function PaneFrame({
  paneRef,
  visible,
  onClose,
  actions,
  children,
}: {
  paneRef: RefObject<HTMLElement | null>;
  visible: boolean;
  onClose: () => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <motion.aside
      ref={paneRef}
      aria-label={t.files}
      aria-hidden={!visible}
      className="material-sidebar flex w-100 shrink-0 flex-col border-l"
      initial={{ opacity: 0 }}
      animate={
        visible
          ? { opacity: 1, display: "flex", transition: { duration: duration.toggle, ease: ease.standard } }
          : { opacity: 0, transitionEnd: { display: "none" }, transition: { duration: duration.micro, ease: ease.exit } }
      }
    >
      <div className="flex h-11 shrink-0 items-center gap-0.5 border-b pr-2 pl-4">
        <h2 className="mr-auto text-title-3">{t.files}</h2>
        {actions}
        <IconButton label={t.closeFiles} onClick={onClose}>
          <X />
        </IconButton>
      </div>
      {children}
    </motion.aside>
  );
}

function IconButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label}>
      <Button variant="quiet" size="icon-sm" aria-label={label} onClick={onClick} disabled={disabled}>
        {children}
      </Button>
    </Tooltip>
  );
}

function EntryIcon({ entry }: { entry: RemoteFile }) {
  const Icon = entry.kind === "directory" ? Folder : entry.kind === "symlink" ? Link2 : File;
  return <Icon className={cn("size-4 shrink-0", entry.kind === "directory" ? "text-(--accent)" : "text-label-2")} strokeWidth={1.5} />;
}

function EntryRow({
  entry,
  selected,
  onSelect,
  onOpen,
  onMove,
  onDownload,
  onRename,
  onProperties,
  onDelete,
}: {
  entry: RemoteFile;
  selected: boolean;
  onSelect: (event: MouseEvent<HTMLDivElement>) => void;
  onOpen: () => void;
  onMove: (step: 1 | -1, extend: boolean) => void;
  onDownload: () => void;
  onRename: () => void;
  onProperties: () => void;
  onDelete: () => void;
}) {
  return (
    <div
      role="option"
      data-name={entry.name}
      aria-selected={selected}
      tabIndex={0}
      onClick={onSelect}
      onDoubleClick={onOpen}
      onKeyDown={(event) => {
        // Keyboard.md, lists: Enter opens, F2 renames, Delete removes after a confirmation.
        if (event.key === "Enter" && event.altKey) onProperties();
        else if (event.key === "Enter") onOpen();
        else if (event.key === "F2") onRename();
        else if (event.key === "Delete") onDelete();
        else if (event.key === "ArrowDown" || event.key === "ArrowUp") onMove(event.key === "ArrowDown" ? 1 : -1, event.shiftKey);
        else return;
        event.preventDefault();
      }}
      className={cn(
        "group relative flex h-7 items-center gap-2 rounded-xs px-2 text-callout transition-colors duration-micro select-none",
        selected ? "bg-accent-soft" : "hover:bg-fill-1",
      )}
    >
      <EntryIcon entry={entry} />
      <span className="min-w-0 flex-1 truncate" title={entry.name}>
        {entry.name}
      </span>
      <span className="w-16 shrink-0 text-right text-callout text-label-3 tabular-nums">
        {entry.kind === "directory" ? "—" : formatSize(entry.size)}
      </span>
      <span className="w-24 shrink-0 truncate text-right text-callout text-label-3 tabular-nums">{entry.modified}</span>
      {/* S3: one icon action and the "more" menu, in reserved space. */}
      <span className="flex w-14 shrink-0 items-center justify-end opacity-0 transition-opacity duration-micro group-focus-within:opacity-100 group-hover:opacity-100">
        <IconButton label={t.download} onClick={onDownload}>
          <Download />
        </IconButton>
        <Menu
          label={t.more}
          items={[
            { key: "rename", label: t.rename, icon: <Pencil />, shortcut: shortcutHint("rename"), onSelect: onRename },
            { key: "properties", label: t.properties, icon: <Info />, shortcut: shortcutHint("properties"), onSelect: onProperties },
            "separator",
            { key: "delete", label: t.delete, icon: <Trash2 />, danger: true, shortcut: shortcutHint("remove"), onSelect: onDelete },
          ]}
        />
      </span>
    </div>
  );
}

function EditRow({
  icon,
  value,
  placeholder,
  onChange,
  onSubmit,
  onCancel,
}: {
  icon: ReactNode;
  value: string;
  placeholder?: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="flex h-8 items-center gap-2 px-2 [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-label-2">
      {icon}
      <Input
        autoFocus
        value={value}
        placeholder={placeholder}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") onSubmit();
          if (event.key === "Escape") onCancel();
        }}
        onBlur={onCancel}
        className="h-7 text-callout"
      />
    </div>
  );
}

function TransferRow({ item, onCancel, onRetry }: { item: TransferItem; onCancel: () => void; onRetry: () => void }) {
  const progress = item.total ? Math.min(1, item.bytes / item.total) : null;
  const Direction = item.upload ? ArrowUp : ArrowDown;
  return (
    <motion.li {...appear} className="rounded-xs px-2 py-1.5">
      <div className="flex items-center gap-2 text-callout">
        <Direction className="size-4 shrink-0 text-label-2" strokeWidth={1.5} aria-label={item.upload ? t.upload : t.download} />
        <span className="min-w-0 flex-1 truncate">{item.name}</span>
        <TransferStatus item={item} />
        {(item.state === "running" || item.state === "queued") && (
          <IconButton label={t.cancelTransfer} onClick={onCancel}>
            <X />
          </IconButton>
        )}
        {item.state === "failed" && (
          <IconButton label={t.retryTransfer} onClick={onRetry}>
            <RotateCw />
          </IconButton>
        )}
        {item.state === "done" && !item.upload && isTauri && (
          <IconButton label={t.showInFolder} onClick={() => void revealLocal(item.local)}>
            <FolderSearch />
          </IconButton>
        )}
      </div>
      {item.state === "running" && progress !== null && (
        <div className="mt-1.5 h-1 overflow-hidden rounded-pill bg-fill-2">
          <div
            className="h-full origin-left rounded-pill bg-accent-fill"
            style={{ transform: `scaleX(${progress})` }}
          />
        </div>
      )}
      {item.state === "running" && <TransferDetail item={item} />}
      {item.state === "failed" && item.message && (
        <p className="selectable mt-1 text-callout break-all text-danger">{item.message}</p>
      )}
    </motion.li>
  );
}

function TransferStatus({ item }: { item: TransferItem }) {
  const text = "shrink-0 text-callout";
  if (item.state === "running") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-label-2")}>
        <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
        {progressPercent(item)}
      </span>
    );
  }
  if (item.state === "queued") return <span className={cn(text, "text-label-3")}>{t.transferQueued}</span>;
  if (item.state === "done") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-success")}>
        <Check className="size-4" strokeWidth={1.5} />
        {t.transferDone}
      </span>
    );
  }
  if (item.state === "failed") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-danger")}>
        <CircleAlert className="size-4" strokeWidth={1.5} />
        {t.transferFailed}
      </span>
    );
  }
  return <span className={cn(text, "text-label-3")}>{t.transferCancelled}</span>;
}

function progressPercent(item: TransferItem): string {
  return item.total ? `${Math.min(100, Math.floor((item.bytes / item.total) * 100))}%` : item.bytes > 0 ? formatSize(item.bytes) : "";
}

/** Bytes moved of the total, the speed and the time left. */
function TransferDetail({ item }: { item: TransferItem }) {
  const perSecond = item.rate?.perSecond ?? 0;
  const left = secondsLeft(item.bytes, item.total, perSecond);
  const parts = [item.total ? `${formatSize(item.bytes)} / ${formatSize(item.total)}` : formatSize(item.bytes)];
  if (perSecond > 0) parts.push(`${formatSize(perSecond)}/s`);
  if (left !== null) parts.push(t.transferTimeLeft.replace("{time}", formatDuration(left)));
  return <p className="mt-1 text-callout text-label-3 tabular-nums">{parts.join(" · ")}</p>;
}
