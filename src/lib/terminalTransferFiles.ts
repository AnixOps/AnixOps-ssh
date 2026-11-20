import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./backend";
import { t } from "./i18n";
import { isMobileShell } from "./platform";
import { TerminalTransfer, type TransferFiles } from "./terminalTransfer";
import { transferPrefs } from "./terminalTransferPrefs";


/** Local files through the app's commands (desktop only). */
export const tauriTransferFiles: TransferFiles = {
  async pickSend() {
    const picked = await openDialog({ multiple: true, directory: false, title: t.transferPickFiles });
    if (!picked) return undefined;
    return Array.isArray(picked) ? picked : [picked];
  },
  async pickSave(name) {
    return (await saveDialog({ defaultPath: name, title: t.transferSaveAs })) ?? undefined;
  },
  openRead: (path) => invoke("terminal_file_open_read", { path }),
  async read(id, len) {
    return new Uint8Array(await invoke<ArrayBuffer>("terminal_file_read", { id, len }));
  },
  openWrite: (name, path) => invoke("terminal_file_open_write", { name, path }),
  async write(id, bytes) {
    await invoke("terminal_file_write", bytes, { headers: { "x-file-id": String(id) } });
  },
  close: (id, keep) => invoke("terminal_file_close", { id, keep }),
};

/** A transfer for one terminal view; plain pass-through where there are no local files (browser preview, phones). */
export function createTerminalTransfer(): TerminalTransfer {
  return new TerminalTransfer(isTauri && !isMobileShell() ? tauriTransferFiles : null, transferPrefs);
}
