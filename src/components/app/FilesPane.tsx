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
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
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
import { t } from "@/lib/i18n";
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
  const [selected, setSelected] = useState<string | null>(null);
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
        setEntries(sortEntries(found.entries));
        entriesRef.current = found.entries;
        setCwd(found.dir);
        setPathInput(found.dir);
        cwdRef.current = found.dir;
        setSelected(found.select);
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

  const transfer = useCallback(
    async (upload: boolean, remote: string, local: string | undefined, total: number | null) => {
      const targetDir = cwdRef.current;
      let id = "";
      // Events can arrive before the start call returns; hold them until the item exists.
      let ready = false;
      const early: TransferEvent[] = [];
      const onEvent = (event: TransferEvent) => (ready ? apply(event) : early.push(event));
      const apply = (event: TransferEvent) => {
        if (event.kind === "progress") updateTransfer(id, { bytes: event.bytes });
        else if (event.kind === "done") {
          updateTransfer(id, { state: "done", bytes: total ?? 0 });
          if (upload && handle && cwdRef.current === targetDir) void navigate(handle, targetDir);
        } else if (event.kind === "failed") updateTransfer(id, { state: "failed", message: event.message });
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
        setActionError(String(reason));
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
    if (!value || value.includes("/")) return;
    if (editing.kind === "mkdir") void run(() => handle.mkdir(joinPath(cwd, value)));
    else if (value !== editing.from) void run(() => handle.rename(joinPath(cwd, editing.from), joinPath(cwd, value)));
    setEditing(null);
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
          <Notice className="min-w-0 flex-1 break-all">{actionError}</Notice>
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
        aria-label={cwd}
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
              onChange={(value) => setEditing({ ...editing, value })}
              onSubmit={submitEdit}
              onCancel={() => setEditing(null)}
            />
          ) : (
            <EntryRow
              key={entry.name}
              entry={entry}
              selected={selected === entry.name}
              onSelect={() => setSelected(entry.name)}
              onOpen={() => open(entry)}
              onDownload={() => void transfer(false, joinPath(cwd, entry.name), undefined, entry.kind === "file" ? entry.size : null)}
              onRename={() => setEditing({ kind: "rename", from: entry.name, value: entry.name })}
              onDelete={async () => {
                const directory = entry.kind === "directory";
                const title = t.fileDeleteTitle.replace("{name}", entry.name);
                const body = directory ? t.folderDeleteBody : t.fileDeleteBody;
                if (await confirm({ title, body, verb: t.delete }))
                  void run(() => handle.remove(joinPath(cwd, entry.name), directory));
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
  onSelect,
  onOpen,
  onDownload,
  onRename,
  onDelete,
}: {
  entry: RemoteFile;
  selected: boolean;
  onSelect: () => void;
  onOpen: () => void;
  onDownload: () => void;
  onRename: () => void;
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
        if (event.key === "Enter") onOpen();
        else if (event.key === "F2") onRename();
        else if (event.key === "Delete") onDelete();
        else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          const next = event.currentTarget[event.key === "ArrowDown" ? "nextElementSibling" : "previousElementSibling"];
          if (next instanceof HTMLElement && next.getAttribute("role") === "option") next.focus();
        } else return;
        event.preventDefault();
      }}
      className={cn(
        "group relative flex h-7 items-center gap-2 rounded-xs px-2 text-callout transition-colors duration-micro",
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

function TransferStatus({ item }: { item: TransferItem }) {
  const text = "shrink-0 text-callout";
  if (item.state === "running") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-label-2")}>
        <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
        {item.bytes > 0 ? formatSize(item.bytes) : ""}
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
