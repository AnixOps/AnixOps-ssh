import type { UpdateStatus } from "./update";

/** The update row in the browser preview, in step with `UpdateStatus` in
 *  src-tauri/src/update.rs. `?update=found` shows a newer release, `?update=off`
 *  a build without a verification key. */
const scene = new URLSearchParams(window.location.search).get("update");

let updates: UpdateStatus = {
  available: scene !== "off",
  autoCheck: true,
  current: "preview",
  lastChecked: scene === "off" ? null : 1_800_000_000,
  update:
    scene === "found"
      ? {
          version: "2.2.0",
          build: 2_002_000,
          url: "https://github.com/AnixOps/EasySSH/releases/tag/v2.2.0",
          notes: "内置 SSH 引擎修复了跳板机重连；新增更新提示。",
          publishedAt: "2027-01-15T08:00:00Z",
        }
      : null,
};

export function previewUpdateStatus(): UpdateStatus {
  return updates;
}

export function previewUpdateSetAuto(on: boolean): UpdateStatus {
  updates = { ...updates, autoCheck: on };
  return updates;
}

export function previewUpdateCheck(): UpdateStatus {
  updates = { ...updates, lastChecked: Math.floor(Date.now() / 1000) };
  return updates;
}
