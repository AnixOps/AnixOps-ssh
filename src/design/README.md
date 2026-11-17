# AnixOps Design (vendored)

Copied from [AnixOps/AnixOps-design](https://github.com/AnixOps/AnixOps-design)
version 1.2.0 (commit 5ce77c2). Do not edit these files: change `tokens.json`
upstream, regenerate, and copy again.

| File | Upstream path |
|---|---|
| `tokens.css` | `tokens/css/tokens.css` |
| `theme.css` | `tokens/tailwind/theme.css` |
| `fonts/` | `fonts/inter/` |
| `../assets/mark*.svg` | `brand/assets/` |
| `../../public/favicon.svg` | `brand/assets/favicon.svg` |
| `../../src-tauri/icons/` | `brand/assets/tauri/` |
| `../../src-tauri/icons/android/`, `ios/` | `brand/assets/app-icon/` (1024 source, adaptive foreground, background and monochrome), resized with `tauri icon` and a manifest that names the four files |

Not vendored from `brand/assets/`: the README banner in `docs/brand/`, made with
`node scripts/build-assets.mjs --product SSH` from the same version. AnixOps
SSH is an installed client, so it uses `--font-app` and `--font-app-mono`
(guidelines/typography.md, "Client applications") and does not load Inter; the
`fonts/` copy is kept so the vendored set matches upstream.
