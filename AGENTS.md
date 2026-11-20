# Working on AnixOps SSH

## Ground rules

- The owner decides product direction and architecture; agents write the
  code. Architecture changes go into README/ROADMAP first and wait for
  approval.
- One feature per branch and pull request. Merge only with green CI.
- No unattended "retry until it passes" automation. After two failed CI
  attempts on the same problem, stop and report the evidence.
- Never connect to real hosts or read the user's SSH configuration in tests.

## Building

Nothing is compiled locally. CI (`.github/workflows/ci.yml`) runs once per
pull request push (as `pull_request`) and on main after each merge: frontend
typecheck and build, Playwright screenshots in light and dark plus the
layout checks at the smallest window in Chinese and English
([docs/layout-checks.md](docs/layout-checks.md)), then
`cargo fmt`, `clippy -D warnings` and `cargo test` on Windows, macOS and Linux.

CI also builds the mobile prototype (`docs/mobile-prototype.md`): the
"Android (debug APK)" job builds an APK, and the macOS Rust job builds the iOS
app library without signing (one macOS runner per run).

`scripts/ci-changes.sh` decides the rest. `tauri build` (installers) runs on
main, on manual runs (Actions → CI → Run workflow on the branch), and on pull
requests that change the version or packaging files. End-to-end and Android
are skipped when a pull request only touches frontend or docs files. The
required Rust checks always run.

"Terminal end-to-end (Linux)" runs for every change but docs: the real
frontend in Chromium (Windows key habits), a real OpenSSH server, and the real
session daemon in between (`scripts/terminal-e2e.mjs`,
`crates/easyssh-e2e/examples/web_bridge.rs`). Each check is something a person
does every day in Windows Terminal, iTerm2 or Termius; the list and the
findings are in `docs/terminal-parity.md`. A terminal change that breaks one
fails CI; add a check when you add a terminal behaviour. Run it locally with
the e2e containers up: `cargo build -p easyssh-e2e --example web_bridge`,
`npm run build && npm run preview`, then
`node scripts/terminal-e2e.mjs http://localhost:4173 out` (`ONLY=name,…`).

Results (logs, screenshots, lockfiles) are force-pushed to the branch
`ci-results/<branch>`; read them with
`git fetch origin ci-results/<branch>` and `git show FETCH_HEAD:<path>`.
When rustfmt fails, `logs-Linux/logs/fmt.patch` holds the fix; apply it with
`git show FETCH_HEAD:logs-Linux/logs/fmt.patch | git apply`.

## Branches and releases

- Work on a feature branch, open a PR with `gh pr create`, squash-merge once
  CI is green, and delete the branch (`gh pr merge --squash --delete-branch`).
  Deleting a branch also deletes its `ci-results/<branch>` (cleanup.yml).
- Releases follow [RELEASING.md](RELEASING.md): version bump pull request,
  smoke test on a real machine, tag, and release.yml does the rest.
- `main` is branch-protected: the CI checks must pass before merging.

## UI rules

The UI follows AnixOps Design 1.2.0 (`src/design/`, vendored, never hand-edit;
upgrade per its `platforms.md` and record the version in `src/design/README.md`).
The audit and the button-level mapping are in `docs/design-audit.md`.

- Use only design tokens and the Tailwind utilities generated from them. No
  literal colours outside `src/design`; `npm run lint:colors` (run by
  `npm test`) fails on any. The terminal palette is read from tokens in
  `src/lib/terminalTheme.ts`.
- One button family (`ui/button.tsx`): primary, secondary, quiet, danger, and
  the filled-red `confirm` that only appears inside a confirmation dialog.
  Rows show at most one labelled and one icon action; the rest go in `ui/menu.tsx`.
  Every destructive action goes through `useConfirm()` naming the object, with
  Cancel focused.
- Type, spacing, radius, shadow and z-index come from tokens; no arbitrary
  pixel values for them. Icons are Lucide at stroke 1.5; 20 px in navigation,
  16 px in buttons. Icon-only buttons need an `aria-label` and a tooltip.
- Motion uses `src/lib/motion.ts` (120–280 ms, opacity and transform, no
  springs) and respects `prefers-reduced-motion`.
- The window never shows the web view's own menu or browser keys
  (`src/lib/webviewChrome.ts`). A right-click gets the app's menu: `ContextMenu`,
  or `Menu contextRow` inside a `[data-context-row]` row; text fields and
  selected text get `ui/editMenu.tsx` without extra work.
- Shortcuts live once in `src/lib/shortcutTable.ts` and `src/lib/shortcuts.ts`;
  the list (Ctrl/⌘+/), tooltips and docs read it. Plain Ctrl keys belong to
  the shell inside terminals; never use Ctrl+Alt. The only plain-Ctrl
  exceptions are the ones every terminal shares: Ctrl+Tab / Ctrl+Shift+Tab
  (tabs), Ctrl+= / Ctrl+- / Ctrl+0 and Ctrl+wheel (font zoom), and, behind a
  setting that is on by default on Windows only, Windows Terminal's Ctrl+C
  (with a selection) and Ctrl+V.
- All user-facing strings live in `src/lib/i18n.ts` (Chinese and English). The
  product is "AnixOps SSH" everywhere the user sees a name.
- The touch UI for phones and tablets is `src/components/mobile/` (shown when
  `isMobileShell()` says so, or with `?mobile=1` in a browser). It reuses the
  same components and tokens; touch targets are at least 44 px and the
  screenshot script fails on any smaller one.
- Layouts hold at the smallest window (860 × 520) and in English: no text
  squeezed to a character per line, overlapping or cut off without an
  ellipsis. `scripts/layout-audit.mjs` fails CI on any; new screens add a
  scene to `scripts/screenshots.mjs`. The rules are in
  `docs/layout-checks.md`.
- `npm run dev` without Tauri uses `src/lib/preview.ts`; keep it in step with
  the backend types so screenshots stay meaningful.
