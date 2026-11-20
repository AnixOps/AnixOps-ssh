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
  Pencil,
  RotateCw,
  ShieldCheck,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type DragEvent, type MouseEvent, type ReactNode, type RefObject } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
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
import { PermissionsDialog } from "@/components/app/PermissionsDialog";
import { actionTargets, clickSelect, emptySelection, inOrder, prune, selectAll, type Selection } from "@/lib/fileSelection";
import { describeError, isMove, renameTarget } from "@/lib/fileOps";
import { t } from "@/lib/i18n";
import { formatOctal, modeFromListing } from "@/lib/permissions";
import { shortcutHint } from "@/lib/shortcutTable";
import { appear, duration, ease } from "@/lib/motion";
import { locate, localName, parentPath, resolvePath, uploadConflicts } from "@/lib/remotePath";
import { cn } from "@/lib/utils";

interface TransferItem {
  id: string;
  name: string;
  upload: boolean;
  local: string;
  state: "running" | "done" | "failed" | "cancelled";
  bytes: number;
  total: number | null;
  filesDone?: number;
  filesTotal?: number;
  message?: string;
}

type Editing = { kind: "mkdir"; value: string } | { kind: "rename"; from: string; value: string };

const joinPath = (dir: string, name: string) => (dir.endsWith("/") ? dir + name : `${dir}/${name}`);

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

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
  const [permissionsFor, setPermissionsFor] = useState<RemoteFile[] | null>(null);
  /** Names being dragged inside the pane, and the folder row they are over. */
  const dragRef = useRef<string[] | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  /** An entry to scroll to and focus once the listing renders. */
  const [reveal, setReveal] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const confirm = useConfirm();
  const [transfers, setTransfers] = useState<TransferItem[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const paneRef = useRef<HTMLElement>(null);
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
        const sorted = sortEntries(found.entries);
        const sameFolder = found.dir === cwdRef.current;
        setEntries(sorted);
        entriesRef.current = found.entries;
        setCwd(found.dir);
        setPathInput(found.dir);
        cwdRef.current = found.dir;
        // A refresh of the same folder keeps what is selected and still listed.
        setSelection((current) =>
          found.select
            ? clickSelect(emptySelection, [], found.select)
            : sameFolder
              ? prune(current, sorted.map((entry) => entry.name))
              : emptySelection,
        );
        setReveal(found.select);
        setEditing(null);
      } catch (reason) {
        setPathInput(cwdRef.current);
        // A symbolic link that cannot be listed most likely points at a file.
        if (notFolder) notFolder();
        else setActionError(describeError(t.opListing, path, reason));
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

  /** Runs `action` for each item, then refreshes. Every failure is shown
   *  with its operation and path; the rest still run, as file managers do. */
  const run = async <T,>(items: T[], operation: string, pathOf: (item: T) => string, action: (item: T) => Promise<void>) => {
    if (!handle) return;
    const failures: string[] = [];
    for (const item of items) {
      try {
        await action(item);
      } catch (reason) {
        failures.push(describeError(operation, pathOf(item), reason));
      }
    }
    await navigate(handle, cwd);
    if (failures.length > 0) {
      const more = failures.length > 3 ? t.moreErrors.replace("{n}", String(failures.length - 3)) : "";
      setActionError(failures.slice(0, 3).join("\n") + more);
    }
  };

  const updateTransfer = useCallback((id: string, patch: Partial<TransferItem>) => {
    setTransfers((current) => current.map((item) => (item.id === id ? { ...item, ...patch } : item)));
  }, []);

  const transfer = useCallback(
    async (upload: boolean, remote: string, local: string | undefined, total: number | null) => {
      const targetDir = cwdRef.current;
      let id = "";
      // Events can arrive before the start call returns; hold them until the item exists.
      let ready = false;
      const early: TransferEvent[] = [];
      const onEvent = (event: TransferEvent) => (ready ? apply(event) : early.push(event));
      const apply = (event: TransferEvent) => {
        if (event.kind === "progress")
          updateTransfer(id, {
            bytes: event.bytes,
            ...(event.total !== undefined && { total: event.total }),
            ...(event.filesDone !== undefined && { filesDone: event.filesDone }),
            ...(event.filesTotal !== undefined && { filesTotal: event.filesTotal }),
          });
        else if (event.kind === "done") {
          setTransfers((current) =>
            current.map((item) => (item.id === id ? { ...item, state: "done", bytes: item.total ?? total ?? item.bytes } : item)),
          );
          if (upload && handle && cwdRef.current === targetDir) void navigate(handle, targetDir);
        } else if (event.kind === "failed")
          updateTransfer(id, { state: "failed", message: describeError(upload ? t.upload : t.download, upload ? (local ?? remote) : remote, event.message) });
        else updateTransfer(id, { state: "cancelled" });
      };
      try {
        const started = await startTransfer({ hostId, upload, remote, local }, onEvent);
        id = started.id;
        setTransfers((current) => [
          {
            id,
            name: localName(upload ? started.local : started.remote),
            upload,
            local: started.local,
            state: "running",
            bytes: 0,
            total,
          },
          ...current,
        ]);
        ready = true;
        early.forEach(apply);
      } catch (reason) {
        setActionError(describeError(upload ? t.upload : t.download, upload ? (local ?? remote) : remote, reason));
      }
    },
    [handle, hostId, navigate, updateTransfer],
  );

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
      for (const path of paths) void transfer(true, dir, path, null);
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
    else if (entry.kind === "symlink") void navigate(handle, path, false, () => void transfer(false, path, undefined, null));
    else void transfer(false, path, undefined, entry.size);
  };

  const submitEdit = () => {
    if (!editing || !handle) return;
    const value = editing.value.trim();
    if (!value) return;
    if (editing.kind === "mkdir") {
      if (value.includes("/")) return;
      const path = joinPath(cwd, value);
      void run([path], t.opCreatingFolder, (item) => item, (item) => handle.mkdir(item));
    } else {
      // A name renames in place; a path (relative or absolute) moves, as `sftp rename` does.
      const from = joinPath(cwd, editing.from);
      const target = renameTarget(value, from, cwd, handle.home);
      if (target) {
        void run([target], isMove(from, target) ? t.opMoving : t.opRenaming, () => from, (to) => handle.rename(from, to));
      }
    }
    setEditing(null);
  };

  const names = entries.map((entry) => entry.name);
  const byName = (list: string[]) => list.map((name) => entries.find((entry) => entry.name === name)).filter((entry): entry is RemoteFile => !!entry);

  const download = (targets: RemoteFile[]) => {
    for (const entry of targets)
      void transfer(false, joinPath(cwd, entry.name), undefined, entry.kind === "file" ? entry.size : null);
  };

  const remove = async (targets: RemoteFile[]) => {
    if (!handle || targets.length === 0) return;
    const single = targets[0];
    const title =
      targets.length === 1 ? t.fileDeleteTitle.replace("{name}", single.name) : t.deleteManyTitle.replace("{n}", String(targets.length));
    const hasFolder = targets.some((entry) => entry.kind === "directory");
    const body = targets.length > 1 ? t.deleteManyBody : hasFolder ? t.folderDeleteBody : t.fileDeleteBody;
    const items = targets.length > 1 ? targets.map((entry) => entry.name) : undefined;
    if (!(await confirm({ title, body, verb: t.delete, items }))) return;
    void run(targets, t.opDeleting, (entry) => joinPath(cwd, entry.name), (entry) =>
      handle.remove(joinPath(cwd, entry.name), entry.kind === "directory"),
    );
  };

  const applyPermissions = (targets: RemoteFile[], mode: number, recursive: boolean) => {
    if (!handle) return;
    setPermissionsFor(null);
    void run(targets, t.opPermissions, (entry) => joinPath(cwd, entry.name), (entry) =>
      handle.chmod(joinPath(cwd, entry.name), mode, recursive && entry.kind === "directory"),
    );
  };

  const moveInto = (folder: string, moved: string[]) => {
    if (!handle) return;
    const items = moved.filter((name) => name !== folder);
    void run(items, t.opMoving, (name) => joinPath(cwd, name), (name) =>
      handle.rename(joinPath(cwd, name), joinPath(joinPath(cwd, folder), name)),
    );
  };

  /** What a row's action applies to: the selection if the row is in it, else the row. */
  const targetsOf = (entry: RemoteFile) => byName(actionTargets(selection, names, entry.name));

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

  const finished = transfers.some((item) => item.state !== "running");

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
          <Notice className="min-w-0 flex-1 break-all whitespace-pre-line">{actionError}</Notice>
          <IconButton label={t.close} onClick={() => setActionError(null)}>
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
          // Ctrl/Cmd+A selects every row; in the rename box it keeps selecting text.
          if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a" && !(event.target instanceof HTMLInputElement)) {
            event.preventDefault();
            setSelection(selectAll(names));
          }
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
              placeholder={t.renameOrMovePlaceholder}
              onChange={(value) => setEditing({ ...editing, value })}
              onSubmit={submitEdit}
              onCancel={() => setEditing(null)}
            />
          ) : (
            <EntryRow
              key={entry.name}
              entry={entry}
              selected={selection.names.has(entry.name)}
              dropTarget={dropTarget === entry.name}
              onSelect={(event) =>
                setSelection((current) =>
                  clickSelect(current, names, entry.name, { shift: event.shiftKey, toggle: event.ctrlKey || event.metaKey }),
                )
              }
              onExtend={(name) => setSelection((current) => clickSelect(current, names, name, { shift: true }))}
              onOpen={() => open(entry)}
              onDownload={() => download(targetsOf(entry))}
              onRename={() => setEditing({ kind: "rename", from: entry.name, value: entry.name })}
              onPermissions={() => setPermissionsFor(targetsOf(entry))}
              onDelete={() => void remove(targetsOf(entry))}
              onDragStart={() => {
                // Dragging a row outside the selection drags just that row.
                if (!selection.names.has(entry.name)) setSelection(clickSelect(emptySelection, names, entry.name));
                dragRef.current = actionTargets(selection, names, entry.name);
              }}
              onDragEnd={() => {
                dragRef.current = null;
                setDropTarget(null);
              }}
              onDragOver={(event) => {
                if (entry.kind !== "directory" || !dragRef.current || dragRef.current.includes(entry.name)) return;
                event.preventDefault();
                setDropTarget(entry.name);
              }}
              onDrop={() => {
                const moved = dragRef.current;
                dragRef.current = null;
                setDropTarget(null);
                if (moved && entry.kind === "directory") moveInto(entry.name, moved);
              }}
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

      <SelectionStatus entries={byName(inOrder(selection, names))} />

      {permissionsFor && (
        <AnimatePresence>
          <PermissionsDialog
            title={
              permissionsFor.length === 1
                ? t.permissionsTitle.replace("{name}", permissionsFor[0].name)
                : t.permissionsTitleMany.replace("{n}", String(permissionsFor.length))
            }
            initial={modeFromListing(permissionsFor[0].permissions) ?? 0o644}
            folders={permissionsFor.some((entry) => entry.kind === "directory")}
            onApply={(mode, recursive) => applyPermissions(permissionsFor, mode, recursive)}
            onClose={() => setPermissionsFor(null)}
          />
        </AnimatePresence>
      )}

      {transfers.length > 0 && (
        <section className="max-h-56 shrink-0 overflow-y-auto border-t px-2 pt-2 pb-3">
          <div className="flex items-center px-2 pb-1">
            <h3 className="text-callout text-label-2">{t.transfers}</h3>
            {finished && (
              <Button
                variant="quiet"
                size="sm"
                className="ml-auto"
                onClick={() => setTransfers((current) => current.filter((item) => item.state === "running"))}
              >
                {t.clearFinished}
              </Button>
            )}
          </div>
          <ul className="space-y-1">
            <AnimatePresence initial={false}>
            {transfers.map((item) => (
              <TransferRow key={item.id} item={item} onCancel={() => {
                void cancelTransfer(item.id);
                updateTransfer(item.id, { state: "cancelled" });
              }} />
            ))}
            </AnimatePresence>
          </ul>
        </section>
      )}
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
  dropTarget,
  onSelect,
  onExtend,
  onOpen,
  onDownload,
  onRename,
  onPermissions,
  onDelete,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDrop,
}: {
  entry: RemoteFile;
  selected: boolean;
  dropTarget: boolean;
  onSelect: (event: MouseEvent<HTMLElement>) => void;
  onExtend: (name: string) => void;
  onOpen: () => void;
  onDownload: () => void;
  onRename: () => void;
  onPermissions: () => void;
  onDelete: () => void;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDragOver: (event: DragEvent<HTMLElement>) => void;
  onDrop: () => void;
}) {
  return (
    <div
      role="option"
      data-name={entry.name}
      aria-selected={selected}
      tabIndex={0}
      title={`${entry.permissions} ${entry.name}`}
      draggable
      onDragStart={(event) => {
        event.dataTransfer.effectAllowed = "move";
        event.dataTransfer.setData("text/plain", entry.name);
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onDragOver={onDragOver}
      onDrop={(event) => {
        event.preventDefault();
        onDrop();
      }}
      onClick={onSelect}
      onDoubleClick={onOpen}
      onKeyDown={(event) => {
        // Keyboard.md, lists: Enter opens, F2 renames, Delete removes after a confirmation.
        if (event.key === "Enter" && event.altKey) onPermissions();
        else if (event.key === "Enter") onOpen();
        else if (event.key === "F2") onRename();
        else if (event.key === "Delete") onDelete();
        else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          const next = event.currentTarget[event.key === "ArrowDown" ? "nextElementSibling" : "previousElementSibling"];
          if (next instanceof HTMLElement && next.getAttribute("role") === "option") {
            next.focus();
            // Shift+arrow grows the selection like a click with Shift.
            if (event.shiftKey && next.dataset.name) onExtend(next.dataset.name);
          }
        } else return;
        event.preventDefault();
      }}
      className={cn(
        "group relative flex h-7 items-center gap-2 rounded-xs px-2 text-callout transition-colors duration-micro",
        selected ? "bg-accent-soft" : "hover:bg-fill-1",
        dropTarget && "ring-2 ring-(--accent)",
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
            { key: "permissions", label: t.permissions, icon: <ShieldCheck />, shortcut: shortcutHint("permissions"), onSelect: onPermissions },
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

function TransferRow({ item, onCancel }: { item: TransferItem; onCancel: () => void }) {
  const progress = item.total ? Math.min(1, item.bytes / item.total) : null;
  const Direction = item.upload ? ArrowUp : ArrowDown;
  return (
    <motion.li {...appear} className="rounded-xs px-2 py-1.5">
      <div className="flex items-center gap-2 text-callout">
        <Direction className="size-4 shrink-0 text-label-2" strokeWidth={1.5} aria-label={item.upload ? t.upload : t.download} />
        <span className="min-w-0 flex-1 truncate">{item.name}</span>
        <TransferStatus item={item} />
        {item.state === "running" && (
          <IconButton label={t.cancelTransfer} onClick={onCancel}>
            <X />
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
      {item.state === "failed" && item.message && (
        <p className="selectable mt-1 text-callout break-all text-danger">{item.message}</p>
      )}
    </motion.li>
  );
}

/** `12 MB / 80 MB`, and `3/10 files` for a folder. */
function progressText(item: TransferItem): string {
  const bytes = item.total ? `${formatSize(item.bytes)} / ${formatSize(item.total)}` : formatSize(item.bytes);
  return item.filesTotal && item.filesTotal > 1
    ? `${t.transferFiles.replace("{done}", String(item.filesDone ?? 0)).replace("{total}", String(item.filesTotal))} · ${bytes}`
    : bytes;
}

/** What the selected rows are: their count and size, or the mode of the one row. */
function SelectionStatus({ entries }: { entries: RemoteFile[] }) {
  if (entries.length === 0) return null;
  const mode = entries.length === 1 ? modeFromListing(entries[0].permissions) : null;
  const size = entries.reduce((sum, entry) => sum + (entry.kind === "directory" ? 0 : entry.size), 0);
  return (
    <div className="flex h-7 shrink-0 items-center gap-3 border-t px-4 text-callout text-label-2" aria-live="polite">
      <span className="truncate">{entries.length === 1 ? entries[0].name : t.selectedCount.replace("{n}", String(entries.length))}</span>
      {mode !== null && (
        <span className="shrink-0 font-mono text-label-3">
          {entries[0].permissions} {formatOctal(mode)}
        </span>
      )}
      {entries.length > 1 && size > 0 && <span className="shrink-0 tabular-nums text-label-3">{formatSize(size)}</span>}
    </div>
  );
}

function TransferStatus({ item }: { item: TransferItem }) {
  const text = "shrink-0 text-callout";
  if (item.state === "running") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-label-2")}>
        <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
        {item.bytes > 0 || item.total ? progressText(item) : ""}
      </span>
    );
  }
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
