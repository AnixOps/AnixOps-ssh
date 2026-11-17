# AnixOps Design 1.2.0 audit

The audit covers the whole desktop UI of AnixOps SSH against every guideline in
AnixOps Design 1.2.0 (commit 5ce77c2, vendored in `src/design/`). Locations in
the findings are on `main` at bc43fda (the "before" state, screenshots in
`docs/design-audit/before/`). The fixes are on this branch; the result is in
`docs/design-audit/after/`. All screenshots are 1200 x 760, device scale 1, in
light and dark, generated with `npm run screenshots` and preview data.

## Result in numbers

| Guideline | Found | Fixed | Left |
|---|---|---|---|
| Colour (literal colours outside `src/design`) | 32 hex literals in `src/lib/terminalTheme.ts:9-47` | 32 | 0, `npm run lint:colors` gates it |
| Brand (assets, fonts, name) | old mark, favicon, Tauri icons, Inter, README heading without banner | all | 0 |
| actions.md (button levels, S3 rows, confirmations) | 51 `ghost`/`outline`/`destructive` variants, 6 inline "click again" confirm swaps, rows with 3+ icon actions | all | 0 |
| motion.md | spring `slide` and `reflow` transitions, 8 `layout`/`layoutId` uses, page-in animation, no reduced-motion block | all | 0 |
| keyboard.md | no shortcut list (K9), Ctrl+Shift+Alt+D (K22), no Esc/Enter contract on menus, focus not returned | all | 0 |
| density.md | 43 `size-3/3.5/5/6` icons, 9 hit targets under 32 px, arbitrary `z-40` | all | see exceptions |
| Typography | 2 raw weights, `text-xs`-style classes, Inter instead of `--font-app` | all | `AiMarkdown.tsx:36,75` keep semibold for Markdown emphasis |
| Voice and tone, naming | "EasySSH" in UI strings (window title and About were already AnixOps SSH), mixed button verbs, error strings without a next step | all zh and en strings reviewed | 0 |
| Accessibility | menus and dialogs without roles, icon buttons without labels, focus ring not global | all | 0 |

## Screen by screen

Each row lists the violations found in the before state and what changed.
Before and after files are `docs/design-audit/{before,after}/<name>-{light,dark}.png`.

| Screen | Before | After | Findings (before location) |
|---|---|---|---|
| Host list | `home` | `home` | Hover row showed star, edit, delete (S3, `HostList.tsx`); delete had no confirmation; custom spring on the selection pill; `ghost` buttons. Now one star plus a "More" menu (edit, duplicate, delete with confirm), F2/Delete/Shift+F10 on the row. |
| Tabs and terminal | `terminal`, `split`, `exited`, `find`, `local` | same | Tab close and toolbar icons at 24 px; `layoutId` tab pill; terminal palette hard-coded; Ctrl+Shift+Alt+D to split. Now tokens, 32 px targets, Ctrl+Shift+O. |
| SFTP pane | `files` | `files` | Row actions (download, rename, delete) always three icons (S3); delete without confirm; `layout` on rows. Now a "More" menu and confirm. |
| Transfers | `files` | `files` | Cancel and clear as `ghost`; progress rows animated with layout. |
| Port forwards, tunnel editor | `forwards`, `tunnel-editor` | same | Row with start, edit, delete icons; delete without confirm. |
| Snippets | `snippets` | `snippets` | Same row pattern; `layout` animation. |
| Command palette | `palette` | `palette` | Caption-size subtitles; spring on selection; no combobox roles. Now `combobox`/`listbox`, Esc and arrows, token sizes. The input keeps its outline (see exceptions). |
| Settings (general, vault, sync, privacy, advanced) | `settings`, `sync-setup` | same | Inline "click again to confirm" for sign out, forget all, pull (BackupSync.tsx:25,166; SyncSettings.tsx:37,161; VaultSettings.tsx:31,154); ad hoc rows. Now `SettingsGroup`/`SettingsRow`, `useConfirm()` with typed confirmation for forget-all (T2). |
| Host editor | `host-editor`, `new-host` | same | Delete confirmation inline (HostEditor.tsx:61,427); labels without `Field`; Mod+S missing. Now `Field`, `Notice`, Mod+S via `Overlay`, delete via confirm. |
| Import dialog | `import` | `import` | Custom alerts and checkboxes; mixed button levels. Now `Notice`, `Badge`, one primary. |
| Known hosts | settings | settings | Remove icon deleted immediately; now confirm explaining the key will be asked again. |
| Team vaults | settings (Team nav) | same | Inline member-removal confirm (TeamVaults.tsx:179,230); now `useConfirm()` naming the member and stating that keys rotate (PR #42). Revocation dates and new error strings (`certificate_expired`, `stale_key_generation`) use `Notice`. |
| AI panel | `terminal` | same | Raw buttons; spring; fixed-px text. Now shared button and `Notice`. |
| Askpass, host-key, vault-unlock dialogs | `askpass` | `askpass` | Cancel was not the default focus; primary mixed with `ghost`. Now Overlay focus trap, Cancel first, one primary. |
| First-run card and empty states | `first-run`, `no-ssh` | same | `z-40` literal, fade/slide of 400 ms, buttons at three levels. Now token z-index, one primary, a shortcut list for discovery. |
| Error states | `no-ssh`, `exited` | same | Red text without icon or next step. Now `Notice` with icon and an action sentence. |

Serial ports and Windows Hello / Touch ID unlock (PR #41) were not on `main` when
this audit was written. They land through the shared components, and the
audit is updated when that PR is merged.

## Button mapping (actions.md, "short mapping document")

| Level | Component | Use |
|---|---|---|
| E1 Primary | `Button variant="primary"` | One per view: Save, Connect, Add |
| E2 Secondary | `Button variant="secondary"` | Other labelled actions |
| E3 Quiet | `Button variant="quiet"` (`size="icon"` / `"icon-sm"` for icon-only) | Toolbars, row icons, Cancel |
| E4 Destructive | `Button variant="danger"` outside dialogs (outline red); `variant="confirm"` (filled red) only inside `useConfirm()` | Delete, remove, sign out, forget |

Rows (S3) show at most one labelled and one icon action; the rest live in
`Menu` ("More"). Every destructive action calls `useConfirm()`: title names the
object, body says what happens, Cancel is focused (T1), and irreversible
account-level actions require typing a word (T2).

## Exceptions kept on purpose

- Host list rows are two lines (name and target) at compact density; the
  guideline allows this for lists with a secondary identifier.
- The settings column is 288 px wide in the side panel, narrower than the
  recommended 320, to leave room for the terminal; text wraps, no clipping.
- The host editor is still a dialog, not a side sheet. Moving it is a layout
  change beyond this lane and is proposed for the roadmap.
- The tab bar mixes tabs and pane toolbar; the toolbar is icon-only with tooltips.
- The command palette input has no visible border; focus is shown by the
  palette frame.
- `AiMarkdown` uses semibold for emphasis inside rendered Markdown.

## Process

- `npm test` runs `lint:colors` first, so CI fails on any literal colour.
- Shortcuts are defined once (`src/lib/shortcutTable.ts`), read by the list
  (Ctrl/⌘ + /), the tooltips and the docs.
