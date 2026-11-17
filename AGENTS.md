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

Nothing is compiled locally. CI (`.github/workflows/ci.yml`) runs on every
push: frontend typecheck and build, Playwright screenshots in light and dark,
then `cargo fmt`, `clippy -D warnings`, `cargo test` and `tauri build` on
Windows, macOS and Linux.

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
- Shortcuts live once in `src/lib/shortcutTable.ts` and `src/lib/shortcuts.ts`;
  the list (Ctrl/⌘+/), tooltips and docs read it. Plain Ctrl keys belong to
  the shell inside terminals; never use Ctrl+Alt.
- All user-facing strings live in `src/lib/i18n.ts` (Chinese and English). The
  product is "AnixOps SSH" everywhere the user sees a name.
- `npm run dev` without Tauri uses `src/lib/preview.ts`; keep it in step with
  the backend types so screenshots stay meaningful.
