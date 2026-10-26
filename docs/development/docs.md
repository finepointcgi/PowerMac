# Writing documentation

This site is built with [MkDocs](https://www.mkdocs.org/) and the Material theme. Sources are in `docs/`, the navigation in `mkdocs.yml`.

## Preview locally

```bash
python3 -m venv .venv
.venv/bin/pip install -r requirements-docs.txt
.venv/bin/mkdocs serve
```

Then open <http://127.0.0.1:8000>. `mkdocs build --strict` fails on broken links, as CI does.

## Publishing

| Host | How |
|---|---|
| GitLab Pages | `.gitlab-ci.yml` job `pages` builds the site into `public/` on the default branch. |
| GitHub Pages | `.github/workflows/docs.yml` builds and deploys on pushes to `main`/`master`. In the repository settings set **Pages → Source** to **GitHub Actions** once. |

Merge requests / pull requests build the site with `--strict` without publishing it.

## Screenshots and GIFs

Images live in `docs/assets/screenshots/` (PNG) and `docs/assets/gifs/`. Both are made by scripts in `scripts/docs-media/`, so they can be regenerated whenever the UI changes. The scripts need a release build, the Swift compiler (Xcode Command Line Tools) and `ffmpeg`, and the terminal needs **Screen Recording** permission (plus **Accessibility** for GIFs).

```bash
cargo build --release -p powermac
./scripts/docs-media/capture.sh                 # every settings page + the layout editor
./scripts/docs-media/capture.sh sound awake     # just these pages
./scripts/docs-media/record.sh settings-tour general scripts/docs-media/scenes/settings-tour.txt
```

- **Throwaway config.** Both scripts open the windows on a demo config created with `powermac init` in a temporary folder, so your own settings, keys and history never show up. When PowerMac is running, its `*-status.json` files (device lists and running state, no settings) are copied in, so pages show live devices instead of "PowerMac isn't running". Set `STATUS_FROM=` to skip that.
- **Screenshots** capture just the window (`screencapture -l`), scaled to 1400 px wide and reduced to a 256-color palette, which keeps each one around 100 KB.
- **GIFs** record the window while `mouse.swift` moves the real pointer, clicks, scrolls and types from a scene file in `scripts/docs-media/scenes/`. Coordinates are in points, relative to the window's top-left corner. Don't touch the mouse while a recording runs. `record.sh <name> region:X,Y,W,H <scene>` records part of the screen instead, for the notch and other overlays of the running app; use a clean desktop for those.

| Scene | GIF | Used on |
|---|---|---|
| `settings-tour.txt` | `settings-tour.gif` | Home, Install, README |
| `fancyzones-editor.txt` | `fancyzones-editor.gif` | Home, FancyZones |
| `notch-widgets.txt` | `notch-widgets.gif` | Notch widgets |
| `fancyzones-drag.txt` | `fancyzones-drag.gif` | Home, FancyZones, README |
| `window-switcher.txt` | `window-switcher.gif` | Home, Window Switcher, README |
| `notch.txt` | `notch.gif` | Home, Notch Shelf, README |

The last three record the running app on the whole screen (`region:0,0,1512,982`) or around the notch (`region:206,0,1100,300`, on a 1512-point-wide screen), so they show whatever is on the desktop. Before recording them:

- Hide every app except a few demo windows (a PowerMac Settings window on the demo config, TextEdit with a sample note, Calculator), and hide the desktop icons (`defaults write com.apple.finder CreateDesktop -bool false; killall Finder`; undo with `defaults delete com.apple.finder CreateDesktop; killall Finder`).
- The Window Switcher lists hidden and minimized windows and windows on other desktops by title. Set `showHidden`, `showMinimized` and `showOtherSpaces` to `false` in `config.json` while recording, and put them back afterwards.
- `fancyzones-drag.txt` drags windows by fixed title-bar positions and drops them into the zones of a three-column layout; adjust the coordinates to where your windows and zones are.
- Pause music you don't want in the Now Playing widget.
- Check every frame before committing, for example with `ffmpeg -i x.gif -vf "select='not(mod(n\,12))',scale=500:-1,tile=4x3" -frames:v 1 sheet.png`.

Put an image on a page as a figure, so it gets the shared style and a caption:

```markdown
<figure markdown="span">
  ![The Sound settings page](../assets/screenshots/settings-sound.png){ loading=lazy }
  <figcaption>The Sound settings page.</figcaption>
</figure>
```

## Conventions

- One page per module under `docs/features/`, with: what it does, how to use it, a settings table (JSON key, default, meaning), how it works, limitations.
- Config keys are written exactly as in JSON (camelCase).
- When a module's defaults or shortcuts change, update its page and the table in `docs/features/index.md` in the same change.
- When a settings page changes visibly, re-run `capture.sh` for that page (and the scene of any GIF that shows it).
