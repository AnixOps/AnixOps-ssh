# Layout checks

Every screenshot scene in CI (`npm run screenshots`, see
`scripts/screenshots.mjs`) also runs `scripts/layout-audit.mjs`. A scene that
breaks one of the rules below fails the "Frontend and screenshots" job, with a
line naming the scene, the window size and the text involved, e.g.

    settings/min: squeezed to 36 lines of 31 px: "“用编辑器打开”使用的命令…" in 文件

## What runs

| Pass | Window | Language | Theme |
|---|---|---|---|
| `light`, `dark` | 1200 × 760 (default window) | Chinese | light, dark |
| `min` | 860 × 520 (`minWidth`/`minHeight` in `src-tauri/tauri.conf.json`) | Chinese | light |
| `min-en` | 860 × 520 | English (`?lang=en`) | dark |
| mobile scenes | phones (360 and 390 wide), a landscape phone, a tablet | English | light, dark |

Each check scrolls every scroll area through its content, so rows below the
fold are checked too, and only reports what it finds twice 600 ms apart (no
failures from a frame caught mid-animation). The English pass finds buttons by
looking up the Chinese name's key in `src/lib/i18n.ts`.

## The rules

1. **Squeezed text**: no text wraps to three or more lines that are each
   narrower than about two characters (the label column of a row squeezed by
   its control).
2. **Truncated to nothing**: no ellipsis-truncated label narrower than about
   two characters that actually overflows.
3. **Text over text**: no two pieces of text paint over each other, unless
   one is in a layer meant to cover the other (a dialog, menu, tooltip,
   toast, sticky header or footer).
4. **Cut off without an ellipsis**: no text clipped sideways by an
   `overflow: hidden` box unless it shows an ellipsis (`truncate`) or the box
   scrolls.
5. **Sideways scroll**: the window never scrolls horizontally.

Content that is meant to be clipped or stacked (a terminal, a chart) can opt
out with `data-layout-ignore` on its container; terminals (`.xterm`), SVG,
canvas and `aria-hidden` content are skipped already.

## Layout rules that keep the checks green

- A row with a label and a control (`SettingsRow`, the exited-session card)
  gives the words a flex basis (`grow basis-40`) and lets the control wrap
  under them, never `flex-1` with `basis: 0`, which lets the control squeeze
  the words to one character per line. Text fields in a settings row are
  `stacked`: label above, field at full width.
- Fixed widths (`w-56`) are for things in a known-width column only; anything
  that can sit in a narrow column gets `max-w-full`.
- The terminal keeps at least 440 px (`src/lib/workspaceFit.ts`): in a narrow
  window the monitor folds to its rail (expanding it floats it over the
  terminal) and the files pane floats over the terminal instead of taking
  width. Nothing is remembered; a wider window restores the user's layout.
- The tab bar's labelled buttons (broadcast, monitor, files) drop their words
  below a 42rem bar (`@container`), keeping icon, tooltip and accessible name.
- A new screen or dialog adds a scene to `scripts/screenshots.mjs`, so it is
  checked at the smallest window and in English from its first pull request.

## Running locally

    npm run build && npm run preview &
    npm run screenshots -- http://localhost:4173 screenshots

`ONLY=settings,files` limits it to scenes, `PASSES=min,min-en` to passes,
`DSF=1` makes it faster. Chinese text needs a CJK font (`fonts-noto-cjk` on
Linux) or the measurements are wrong.
