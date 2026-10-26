# Command line

The `powermac` executable runs the app and every helper window. All subcommands accept `--config <path>` to use another config file.

| Command | What it does |
|---|---|
| `powermac` / `powermac run [--no-tray]` | Run every enabled module with the tray icon (default). |
| `powermac settings [--page <name>]` | Open the settings window, optionally on a page (`notch-widgets`, `sound`, `notch-shelf`, … — a page title in lowercase with dashes). An open window comes to the front. |
| `powermac editor [--monitor <id>]` | Open the FancyZones layout editor. |
| `powermac init [--force]` | Write a default `config.json` with a new Mouse Without Borders key and print the key. |
| `powermac gen-key` | Print a new Mouse Without Borders security key. |
| `powermac doctor [--prompt]` | Show the config location, permissions and screens; `--prompt` asks for missing permissions. |
| `powermac awake [-d] [-t <secs> \| -e <time>] [-p <pid>]` | Keep the computer awake (standalone, like PowerToys Awake's CLI). `-d` keeps the display on; `-e` takes RFC 3339, `YYYY-MM-DD HH:MM` or `HH:MM`; `-p` stays awake while that process lives. |
| `powermac clipboard` | Open the clipboard history picker. |
| `powermac color-picker [--editor]` | Pick a color from the screen, copy it in the configured format and print it; `--editor` opens the editor. |
| `powermac ocr [<file> \| --clipboard]` | Recognize text in an image file, in the clipboard image, or (neither) in a region you select on screen. |
| `powermac rename <paths…>` | PowerRename window for these items. |
| `powermac new --dir <folder>` | New+ window for this folder. |
| `powermac resize [options] <images…>` | Image Resizer; see below. |
| `powermac finder <action> …` | Finder basics; see below. |

## `powermac resize`

Without options it opens the Image Resizer window. With a size it resizes right away:

```text
--size small|medium|large|phone|<preset>|WxH|N%   (800x600, 800x0 = auto height, 50%)
--width N --height N --unit px|%|cm|in --fit fit|fill|stretch
--shrink-only --keep-orientation --replace --name "%1 (%2)"
--format keep|jpg|png|heic|webp|tiff --quality 1-100
--strip-metadata --remove-gps --keep-date --out <folder> --no-window
```

## `powermac finder`

```text
open-terminal [--app <id>] [paths…]   open the folder (of each item) in the terminal
open-editor   [--app <id>] [paths…]   open the folder in the editor
copy-path     [--style posix|tilde|url|name|quoted] [paths…]
toggle-hidden | show-hidden | hide-hidden   (restarts Finder) | hidden-status
volumes [--all]        list external volumes (--all: every mount)
eject-all [--dry-run] [--no-images] [--network]
who-uses <path>        processes using a file, folder or volume
apps                   detected terminals and editors
--front-window         without paths, use Finder's selection / front window
```

## Examples

```bash
powermac awake --display-on --time-limit 3600
powermac ocr shot.png
powermac resize --size 50% --format jpg --quality 80 ~/Pictures/trip
powermac finder copy-path --style tilde ~/Documents/report.pdf
```
