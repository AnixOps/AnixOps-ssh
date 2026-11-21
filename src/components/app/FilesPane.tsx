import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronUp,
  CircleAlert,
  Copy,
  Download,
  Eye,
  EyeOff,
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
  SquarePen,
  SquareTerminal,
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
  copyText,
  editOpen,
  editPrepare,
  editStop,
  editWatch,
  enableShellIntegration,
  isTauri,
  loadHosts,
  openFiles,
  pickUploads,
  revealLocal,
  startTransfer,
  transferDiscard,
  transferPartial,
  transferVerify,
  type FilesHandle,
  type RemoteFile,
  type TransferEvent,
  type TransferRequest,
  type VerifyResult,
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
import { canResume, canVerify, resumeNoteText, verifyView } from "@/lib/transferResume";
import { failureMessage, formatDuration, nextRate, secondsLeft, type Rate } from "@/lib/transferProgress";
import { baseName, formatSize, locate, localName, parentPath, resolvePath, uploadConflicts } from "@/lib/remotePath";
import { cn } from "@/lib/utils";
import {
  BINARY_CONTENT,
  checkEditable,
  editConflict,
  editKey,
  FOLLOW_DEBOUNCE_MS,
  isBinaryName,
  isEditableName,
  stampOf,
  type EditingFile,
  type RemoteStamp,
} from "@/lib/remoteEdit";
import { onSettings, settings, updateSettings } from "@/lib/settings";
import {
  isHidden,
  nextSort,
  rememberShowHidden,
  rememberSort,
  savedShowHidden,
  savedSort,
  sortEntries,
  type FileSort,
  type SortKey,
} from "@/lib/fileList";
import { followTarget, onTerminalCwd, terminalCwd } from "@/lib/terminalCwd";

interface TransferItem {
  id: string;
  name: string;
  upload: boolean;
  local: string;
  /** The remote file or folder, for failure messages. */
  remote: string;
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  bytes: number;
  total: number | null;
  rate?: Rate;
  message?: string;
  /** Set when the transfer belongs to a file opened in an editor: the download that opens it or the upload of a save. */
  edit?: { key: string; step: "open" | "save" };
  /** What was asked, to continue it: the remote directory (upload) or path (download). */
  request: { upload: boolean; remote: string; local?: string };
  /** The built-in engine keeps partial files, so a failed try can be continued and checked. */
  resumable?: boolean;
  /** How this try went on from an earlier one. */
  note?: { note: "resumed" | "source_changed" | "part_missing"; resumedFrom?: number | null };
  verify?: VerifyResult;
  verifying?: boolean;
  /** The check could not run (as opposed to saying "unavailable"). */
  verifyError?: string;
}

type Editing = { kind: "mkdir"; value: string } | { kind: "rename"; from: string; value: string };

const joinPath = (dir: string, name: string) => (dir.endsWith("/") ? dir + name : `${dir}/${name}`);


export function FilesPane({
  hostId,
  visible,
  overlay = false,
  active,
  onClose,
}: {
  hostId: string;
  visible: boolean;
  /** Float over the terminal instead of taking width (workspaceFit.ts). */
  overlay?: boolean;
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
  const cwdRef = useRef("");
  /** Files opened in an editor, and what stops listening for their saves. */
  const [edits, setEdits] = useState<EditingFile[]>([]);
  const editsRef = useRef<EditingFile[]>([]);
  editsRef.current = edits;
  const stopListening = useRef(new Map<string, () => void>());
  /** Saves that came in while the previous one was still uploading. */
  const savedAgain = useRef(new Set<string>());
  const [follow, setFollow] = useState(settings().followTerminalFolder);
  const [sort, setSort] = useState<FileSort>(savedSort);
  const [showHidden, setShowHidden] = useState(savedShowHidden);
  const [terminal, setTerminal] = useState(() => terminalCwd(hostId));
  /** The listing of `cwdRef`, for checking what an upload would replace. */
  const entriesRef = useRef<RemoteFile[]>([]);

  const navigate = useCallback(
    async (files: FilesHandle, path: string, typed = false, notFolder?: () => void) => {
      setLoading(true);
      setActionError(null);
      try {
        // A typed location may name a file: show its folder with it selected.
        const found = typed ? await locate(files.list, path) : { dir: path, entries: await files.list(path), select: null };
        setEntries(found.entries);
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

  /** Set below, once the edit helpers exist; `transfer` is older than they are. */
  const editEnded = useRef<(edit: { key: string; step: "open" | "save" }, result: "done" | "failed") => void>(() => {});

  const transfer = useCallback(
    async (
      upload: boolean,
      remote: string,
      local: string | undefined,
      total: number | null,
      edit?: { key: string; step: "open" | "save" },
      again?: { replace: string },
    ) => {
      const targetDir = cwdRef.current;
      let request: TransferRequest = { hostId, upload, remote, local, verify: settings().filesVerifyTransfers };
      if (again) {
        request = { ...request, resume: true };
      } else if (!edit) {
        // An earlier try may have left partial files: offer to continue it.
        const partial = await transferPartial(hostId, upload, remote, local).catch(() => null);
        if (partial?.found) {
          const name = localName(upload ? (local ?? remote) : remote);
          const done = formatSize(partial.bytes ?? 0);
          const all = formatSize(partial.size ?? 0);
          if (
            await confirm({
              title: t.resumeFoundTitle.replace("{name}", name),
              body: t.resumeFoundBody.replace("{done}", done).replace("{total}", all),
              verb: t.resumeVerb,
              destructive: false,
            })
          ) {
            request = { ...request, resume: true, local: upload ? local : (partial.localPath ?? undefined) };
          } else if (
            await confirm({
              title: t.restartTitle.replace("{name}", name),
              body: t.restartBody,
              verb: t.restartVerb,
            })
          ) {
            await transferDiscard(hostId, upload, remote, local).catch(() => {});
          } else {
            return;
          }
        }
      }
      const asked = { upload, remote, local: request.local };
      let id = "";
      // Events can arrive before the start call returns; hold them until the item exists.
      let ready = false;
      const early: TransferEvent[] = [];
      const onEvent = (event: TransferEvent) => (ready ? apply(event) : early.push(event));
      const apply = (event: TransferEvent) => {
        if (event.kind === "started") updateTransfer(id, { state: "running" });
        else if (event.kind === "note") {
          updateTransfer(id, { note: { note: event.note, resumedFrom: event.resumedFrom } });
        } else if (event.kind === "verify") {
          const { kind: _kind, ...result } = event;
          updateTransfer(id, { verify: result });
        } else if (event.kind === "progress") {
          // The backend sizes the work once it runs, so `total` can arrive after the row exists.
          setTransfers((all) =>
            all.map((item) =>
              item.id === id
                ? {
                    ...item,
                    state: "running",
                    bytes: event.bytes,
                    total: event.total ?? item.total,
                    rate: nextRate(item.rate, event.bytes, Date.now()),
                  }
                : item,
            ),
          );
        } else if (event.kind === "done") {
          setTransfers((all) =>
            all.map((item) => (item.id === id ? { ...item, state: "done", bytes: item.total ?? item.bytes } : item)),
          );
          if (upload && handle && cwdRef.current === targetDir) void navigate(handle, targetDir);
          if (edit) editEnded.current(edit, "done");
        } else if (event.kind === "failed") {
          setTransfers((all) =>
            all.map((item) => (item.id === id ? { ...item, state: "failed", message: failureMessage(item.remote, event.message) } : item)),
          );
          if (edit) editEnded.current(edit, "failed");
        } else {
          updateTransfer(id, { state: "cancelled" });
          if (edit) editEnded.current(edit, "failed");
        }
      };
      try {
        const started = await startTransfer(request, onEvent);
        id = started.id;
        setTransfers((current) => [
          {
            id,
            name: localName(upload ? started.local : started.remote),
            upload,
            local: started.local,
            remote: started.remote,
            state: started.queued ? "queued" : "running",
            bytes: 0,
            total,
            edit,
            request: asked,
            resumable: started.resumable === true,
          },
          ...current.filter((item) => item.id !== again?.replace),
        ]);
        ready = true;
        early.forEach(apply);
      } catch (reason) {
        setActionError(String(reason));
        if (edit) editEnded.current(edit, "failed");
      }
    },
    [confirm, handle, hostId, navigate, updateTransfer],
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

  const patchEdit = useCallback((key: string, patch: Partial<EditingFile>) => {
    setEdits((current) => current.map((file) => (file.key === key ? { ...file, ...patch } : file)));
  }, []);

  /** The remote file as it is now, for comparing with what was downloaded. */
  const remoteStamp = async (files: FilesHandle, dir: string, name: string): Promise<RemoteStamp | null> => {
    const found = (await files.list(dir)).find((entry) => entry.name === name && entry.kind !== "directory");
    return found ? stampOf(found) : null;
  };

  /** Stops watching a file, deletes its local copy and drops it from the list. */
  const stopEdit = useCallback((file: EditingFile) => {
    stopListening.current.get(file.key)?.();
    stopListening.current.delete(file.key);
    savedAgain.current.delete(file.key);
    void editStop(file.key, file.local).catch(console.error);
    setEdits((current) => current.filter((candidate) => candidate.key !== file.key));
  }, []);

  /** Uploads the saved copy, after asking when the remote file changed meanwhile. */
  const saveEdit = async (key: string) => {
    const file = editsRef.current.find((candidate) => candidate.key === key);
    if (!file || !handle) return;
    if (file.busy) {
      savedAgain.current.add(key);
      return;
    }
    patchEdit(key, { busy: "upload" });
    try {
      const conflict = editConflict(file.base, await remoteStamp(handle, file.dir, file.name));
      if (conflict === "changed" || conflict === "gone") {
        const gone = conflict === "gone";
        const go = await confirm({
          title: (gone ? t.editGoneTitle : t.editConflictTitle).replace("{name}", file.name),
          body: gone ? t.editGoneBody : t.editConflictBody,
          verb: gone ? t.editRecreate : t.editOverwrite,
          destructive: !gone,
        });
        if (!go) {
          patchEdit(key, { busy: null });
          return;
        }
      }
      transfer(true, file.dir, file.local, null, { key, step: "save" });
    } catch (reason) {
      patchEdit(key, { busy: null });
      setActionError(`${file.remote}: ${String(reason)}`);
    }
  };
  const saveRef = useRef(saveEdit);
  saveRef.current = saveEdit;

  editEnded.current = (edit, result) => {
    const file = editsRef.current.find((candidate) => candidate.key === edit.key);
    if (!file) return;
    if (edit.step === "open") {
      if (result === "failed") return stopEdit(file);
      patchEdit(edit.key, { busy: null });
      void (async () => {
        try {
          stopListening.current.set(
            edit.key,
            await editWatch(edit.key, file.local, () => void saveRef.current(edit.key)),
          );
          await editOpen(file.local);
        } catch (reason) {
          stopEdit(file);
          // A binary would hang the editor: it is downloaded instead.
          if (String(reason).includes(BINARY_CONTENT)) {
            setActionError(t.editBinary.replace("{name}", file.name));
            transfer(false, file.remote, undefined, file.base.size);
            return;
          }
          setActionError(`${t.editFailed.replace("{name}", file.name)}${String(reason)}`);
        }
      })();
      return;
    }
    // A save: what the remote file looks like now is the new base for the next conflict check.
    void (async () => {
      let base = file.base;
      if (result === "done" && handle) base = (await remoteStamp(handle, file.dir, file.name).catch(() => null)) ?? base;
      patchEdit(edit.key, { busy: null, base });
      if (savedAgain.current.delete(edit.key)) void saveRef.current(edit.key);
    })();
  };

  /** Downloads the file to a copy of its own, opens it in the editor and uploads it again on every save. */
  const openInEditor = async (entry: RemoteFile) => {
    if (!handle) return;
    const check = checkEditable(entry);
    if (check === "not-file") return setActionError(t.editNotFile);
    if (check === "too-large") return setActionError(t.editTooLarge.replace("{name}", entry.name));
    if (check === "binary") {
      setActionError(t.editBinary.replace("{name}", entry.name));
      return void transfer(false, joinPath(cwd, entry.name), undefined, entry.size);
    }
    if (!isEditableName(entry.name)) {
      return setActionError(`${t.editFailed.replace("{name}", entry.name)}${t.renameInvalid}`);
    }
    const remote = joinPath(cwd, entry.name);
    const key = editKey(hostId, remote);
    const existing = editsRef.current.find((file) => file.key === key);
    if (existing) {
      if (!existing.busy) void editOpen(existing.local).catch((reason) => setActionError(String(reason)));
      return;
    }
    try {
      const local = await editPrepare(hostId, remote);
      setEdits((current) => [
        ...current,
        { key, remote, dir: cwd, name: entry.name, local, base: stampOf(entry), busy: "download" },
      ]);
      transfer(false, remote, local, entry.size, { key, step: "open" });
    } catch (reason) {
      setActionError(`${t.editFailed.replace("{name}", entry.name)}${String(reason)}`);
    }
  };

  // The pane going away ends every edit: no watcher or copy outlives it.
  useEffect(
    () => () => {
      for (const file of editsRef.current) {
        stopListening.current.get(file.key)?.();
        void editStop(file.key, file.local).catch(console.error);
      }
      stopListening.current.clear();
    },
    [],
  );

  useEffect(() => onSettings((next) => setFollow(next.followTerminalFolder)), []);
  useEffect(() => onTerminalCwd(hostId, setTerminal), [hostId]);
  useEffect(() => setTerminal(terminalCwd(hostId)), [hostId]);

  // Follows the terminal's folder once it has stayed put for a moment. The
  // pane's own folder is read when the timer fires, so browsing elsewhere is
  // only undone by the terminal's next move.
  useEffect(() => {
    if (!handle) return;
    if (!followTarget({ enabled: follow, state: terminal, shown: "" })) return;
    const timer = setTimeout(() => {
      const target = followTarget({ enabled: follow, state: terminal, shown: cwdRef.current });
      if (target) void navigate(handle, target, false, () => {});
    }, FOLLOW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [follow, terminal, handle, navigate]);

  const open = (entry: RemoteFile) => {
    if (!handle) return;
    const path = joinPath(cwd, entry.name);
    if (entry.kind === "directory") void navigate(handle, path);
    else if (entry.kind === "symlink") void navigate(handle, path, false, () => void transfer(false, path, undefined, null));
    // A file opens in the editor (saves go back), as in FinalShell and
    // WinSCP, unless Settings say download; a binary (by name here, by its
    // content once downloaded) or one too large to edit downloads.
    else if (settings().filesDoubleClick === "edit" && checkEditable(entry) === "ok" && isEditableName(entry.name)) void openInEditor(entry);
    else void transfer(false, path, undefined, entry.size);
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

  const selectedNames = new Set(selection.names);
  // A hidden entry still shows while it is selected (a typed path to it).
  const shown = sortEntries(entries, sort).filter((entry) => showHidden || !isHidden(entry) || selectedNames.has(entry.name));
  const order = shown.map((entry) => entry.name);

  const chooseSort = (key: SortKey) => {
    const next = nextSort(sort, key);
    setSort(next);
    rememberSort(next);
  };

  const toggleHidden = () => {
    const next = !showHidden;
    setShowHidden(next);
    rememberShowHidden(next);
    if (!next) setSelection((current) => (current.names.some((name) => name.startsWith(".")) ? emptySelection : current));
  };

  /** Without the integration, the follow button offers to turn it on for this host. */
  const offerIntegration = async () => {
    try {
      const { hosts } = await loadHosts();
      const host = hosts.find((candidate) => candidate.id === hostId);
      if (host && host.source !== "saved") return setActionError(t.followTerminalConfigHost);
      const name = host?.name ?? hostId;
      const ok = await confirm({
        title: t.followTerminalEnableTitle.replace("{host}", name),
        body: t.followTerminalEnableBody,
        verb: t.followTerminalEnable,
        destructive: false,
      });
      if (!ok) return;
      if (!(await enableShellIntegration(hostId))) return setActionError(t.followTerminalConfigHost);
      await updateSettings({ followTerminalFolder: true });
      setActionError(t.followTerminalEnabled);
    } catch (reason) {
      setActionError(String(reason));
    }
  };

  const downloadEntries = (names: string[]) => {
    for (const name of names) {
      const entry = entries.find((candidate) => candidate.name === name);
      if (entry) void transfer(false, joinPath(cwd, name), undefined, entry.kind === "file" ? entry.size : null);
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
      <PaneFrame paneRef={paneRef} visible={visible} overlay={overlay} onClose={onClose}>
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
      <PaneFrame paneRef={paneRef} visible={visible} overlay={overlay} onClose={onClose}>
        <div className="flex flex-1 items-center justify-center gap-2 text-callout text-label-2">
          <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
          {t.filesConnecting}
        </div>
      </PaneFrame>
    );
  }

  const finished = transfers.some((item) => !isActive(item));

  return (
    <PaneFrame
      paneRef={paneRef}
      overlay={overlay}
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
          <IconButton label={t.showHiddenFiles} pressed={showHidden} onClick={toggleHidden}>
            {showHidden ? <Eye /> : <EyeOff />}
          </IconButton>
        </>
      }
    >
      <div className="flex items-center gap-1 px-3 pt-2 pb-2">
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
          className="h-8 min-w-0 flex-1 font-mono text-callout"
        />
        <Tooltip label={terminal.integration ? t.followTerminalOn : t.followTerminalNeedsIntegration}>
          <span className="inline-flex">
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label={t.followTerminal}
              aria-pressed={follow && terminal.integration}
              className={cn(follow && terminal.integration && "bg-accent-soft text-(--accent)", !terminal.integration && "text-label-3")}
              onClick={() =>
                terminal.integration
                  ? void updateSettings({ followTerminalFolder: !follow }).catch((reason) => setActionError(String(reason)))
                  : void offerIntegration()
              }
            >
              <SquareTerminal />
            </Button>
          </span>
        </Tooltip>
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

      <div className="flex h-6 shrink-0 items-center gap-2 px-4 text-caption text-label-3" role="toolbar" aria-label={t.sortBy}>
        <SortButton label={t.sortByName} column="name" sort={sort} onSort={chooseSort} className="min-w-0 flex-1 justify-start" />
        <SortButton label={t.sortBySize} column="size" sort={sort} onSort={chooseSort} className="w-16 justify-end" />
        <SortButton label={t.sortByModified} column="modified" sort={sort} onSort={chooseSort} className="w-24 justify-end" />
        <span className="w-14 shrink-0" />
      </div>

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
          // Backspace and Alt+Up (⌘↑ on a Mac) go to the parent folder, as in Explorer, Finder and WinSCP.
          else if (
            cwd !== "/" &&
            ((event.key === "Backspace" && !event.ctrlKey && !event.metaKey && !event.altKey) ||
              (event.key === "ArrowUp" && (isMac ? event.metaKey : event.altKey)))
          ) {
            event.preventDefault();
            const from = baseName(cwd);
            void navigate(handle, parentPath(cwd)).then(() => setReveal(from));
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
        {shown.length === 0 && !loading && !editing && (
          <p className="pt-10 text-center text-callout text-label-2">{t.emptyFolder}</p>
        )}
        {shown.map((entry) =>
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
              onCopyPath={() => void copyText(joinPath(cwd, entry.name)).catch((reason) => setActionError(String(reason)))}
              onEdit={entry.kind === "directory" || isBinaryName(entry.name) ? undefined : () => void openInEditor(entry)}
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

      {edits.length > 0 && (
        <section className="shrink-0 border-t px-2 pt-2 pb-2" aria-label={t.editingFiles}>
          <h3 className="px-2 pb-1 text-callout text-label-2">{t.editingFiles}</h3>
          <ul className="space-y-0.5">
            {edits.map((file) => (
              <li key={file.key} className="flex items-center gap-2 rounded-xs px-2 py-0.5 text-callout">
                <SquarePen className="size-4 shrink-0 text-(--accent)" strokeWidth={1.5} />
                <span className="min-w-0 flex-1 truncate" title={file.remote}>
                  {file.name}
                </span>
                <span className="shrink-0 text-label-3">
                  {file.busy === "download" ? t.editBusyDownload : file.busy === "upload" ? t.editBusyUpload : t.editWatching}
                </span>
                <IconButton label={t.stopEditing} onClick={() => stopEdit(file)}>
                  <X />
                </IconButton>
              </li>
            ))}
          </ul>
        </section>
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
                onClick={() => setTransfers((current) => current.filter(isActive))}
              >
                {t.clearFinished}
              </Button>
            )}
          </div>
          <ul className="space-y-1">
            <AnimatePresence initial={false}>
            {transfers.map((item) => (
              <TransferRow
                key={item.id}
                item={item}
                onCancel={() => {
                  void cancelTransfer(item.id);
                  updateTransfer(item.id, { state: "cancelled" });
                }}
                onResume={() =>
                  void transfer(
                    item.request.upload,
                    item.request.remote,
                    // A download goes back to the file the earlier try chose.
                    item.request.upload ? item.request.local : item.local,
                    item.total,
                    undefined,
                    { replace: item.id },
                  )
                }
                onDiscard={() => {
                  void transferDiscard(hostId, item.request.upload, item.request.remote, item.request.local).catch(
                    (reason) => setActionError(String(reason)),
                  );
                  setTransfers((current) => current.filter((other) => other.id !== item.id));
                }}
                onVerify={() => {
                  updateTransfer(item.id, { verifying: true, verifyError: undefined });
                  transferVerify(hostId, item.local, item.remote)
                    .then((verify) => updateTransfer(item.id, { verify, verifying: false }))
                    .catch((reason) => updateTransfer(item.id, { verifying: false, verifyError: String(reason) }));
                }}
              />
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
  overlay,
  onClose,
  actions,
  children,
}: {
  paneRef: RefObject<HTMLElement | null>;
  visible: boolean;
  overlay: boolean;
  onClose: () => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <motion.aside
      ref={paneRef}
      aria-label={t.files}
      aria-hidden={!visible}
      className={cn(
        "material-sidebar flex w-100 max-w-full shrink-0 flex-col border-l",
        overlay && "absolute inset-y-0 right-0 z-sticky shadow-3",
      )}
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
  pressed,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  /** A toggle: its state is announced and drawn. */
  pressed?: boolean;
  children: ReactNode;
}) {
  return (
    <Tooltip label={label}>
      <Button
        variant="quiet"
        size="icon-sm"
        aria-label={label}
        aria-pressed={pressed}
        onClick={onClick}
        disabled={disabled}
        className={cn(pressed && "text-(--accent)")}
      >
        {children}
      </Button>
    </Tooltip>
  );
}

/** A column heading that sorts by it; a second click reverses the order. */
function SortButton({
  label,
  column,
  sort,
  onSort,
  className,
}: {
  label: string;
  column: SortKey;
  sort: FileSort;
  onSort: (column: SortKey) => void;
  className?: string;
}) {
  const active = sort.key === column;
  const Arrow = sort.descending ? ChevronDown : ChevronUp;
  return (
    <button
      type="button"
      onClick={() => onSort(column)}
      aria-pressed={active}
      className={cn(
        "flex h-6 shrink-0 items-center gap-0.5 truncate rounded-xs px-0.5 transition-colors duration-micro hover:text-label-1 focus-visible:outline-2 focus-visible:outline-(--accent)",
        active && "font-medium text-label-2",
        className,
      )}
    >
      <span className="truncate">{label}</span>
      {active && <Arrow className="size-3 shrink-0" strokeWidth={2} aria-hidden />}
    </button>
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
  onEdit,
  onCopyPath,
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
  /** Absent for a folder. */
  onEdit?: () => void;
  onCopyPath: () => void;
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
      data-context-row
      // A right-click acts on the row under the pointer, as in a file manager.
      onContextMenu={(event) => {
        if (!selected) onSelect(event);
      }}
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
          contextRow
          items={[
            { key: "download", label: t.download, icon: <Download />, onSelect: onDownload },
            ...(onEdit ? [{ key: "edit", label: t.openInEditor, icon: <SquarePen />, onSelect: onEdit }] : []),
            { key: "copy-path", label: t.copyPath, icon: <Copy />, onSelect: onCopyPath },
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

/** Queued and running transfers can be cancelled and are kept in the list. */
const isActive = (item: TransferItem) => item.state === "running" || item.state === "queued";

function TransferRow({
  item,
  onCancel,
  onResume,
  onDiscard,
  onVerify,
}: {
  item: TransferItem;
  onCancel: () => void;
  onResume: () => void;
  onDiscard: () => void;
  onVerify: () => void;
}) {
  const verdict = item.verify ? verifyView(item.verify) : null;
  const progress = item.total ? Math.min(1, item.bytes / item.total) : null;
  const Direction = item.upload ? ArrowUp : ArrowDown;
  return (
    <motion.li {...appear} className="rounded-xs px-2 py-1.5">
      <div className="flex items-center gap-2 text-callout">
        <Direction className="size-4 shrink-0 text-label-2" strokeWidth={1.5} aria-label={item.upload ? t.upload : t.download} />
        <span className="min-w-0 flex-1 truncate">{item.name}</span>
        <TransferStatus item={item} />
        {isActive(item) && (
          <IconButton label={t.cancelTransfer} onClick={onCancel}>
            <X />
          </IconButton>
        )}
        {canResume(item) && (
          <>
            <Button variant="quiet" size="sm" onClick={onResume}>
              {t.transferResume}
            </Button>
            <IconButton label={t.transferDiscard} onClick={onDiscard}>
              <Trash2 />
            </IconButton>
          </>
        )}
        {canVerify(item) && !verdict && (
          <Button variant="quiet" size="sm" disabled={item.verifying} onClick={onVerify}>
            {item.verifying ? t.transferVerifying : t.transferVerify}
          </Button>
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
      {item.note && (
        <p className="mt-1 text-callout text-label-3">{resumeNoteText(item.note.note, item.note.resumedFrom)}</p>
      )}
      {item.state === "failed" && item.message && (
        <p className="selectable mt-1 text-callout break-all text-danger">{item.message}</p>
      )}
      {(item.state === "failed" || item.state === "cancelled") && !item.resumable && (
        <p className="mt-1 text-callout text-label-3">{t.transferNoResume}</p>
      )}
      {verdict && (
        <div className="mt-1 text-callout">
          <p className={verdict.tone === "success" ? "text-success" : verdict.tone === "danger" ? "text-danger" : "text-label-3"}>
            {verdict.text}
          </p>
          {verdict.note && <p className="text-label-3">{verdict.note}</p>}
          {verdict.hashes && (
            <dl className="selectable mt-0.5 grid grid-cols-[auto_1fr] gap-x-2 font-mono text-label-2 break-all">
              <dt>{t.verifyLocal}</dt>
              <dd>{verdict.hashes.local}</dd>
              <dt>{t.verifyRemote}</dt>
              <dd>{verdict.hashes.remote}</dd>
            </dl>
          )}
        </div>
      )}
      {item.verifyError && <p className="selectable mt-1 text-callout break-all text-danger">{item.verifyError}</p>}
    </motion.li>
  );
}

function TransferStatus({ item }: { item: TransferItem }) {
  const text = "shrink-0 text-callout";
  if (item.state === "queued") return <span className={cn(text, "text-label-3")}>{t.transferQueued}</span>;
  if (item.state === "running") {
    return (
      <span className={cn(text, "flex items-center gap-1 text-label-2")}>
        <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
        {progressPercent(item)}
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
