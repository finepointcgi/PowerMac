# Command line

The `powermac` executable runs the app and every helper window. All subcommands accept `--config <path>` to use another config file.

| Command | What it does |
|---|---|
| `powermac` / `powermac run [--no-tray]` | Run every enabled module with the tray icon (default). |
| `powermac --version` | Print the version (the workspace version in `Cargo.toml`). |
| `powermac settings [--page <name>]` | Open the settings window, optionally on a page (`notch-widgets`, `sound`, `notch-shelf`, … — a page title in lowercase with dashes; `welcome` opens the setup, `permissions` its Permissions step). An open window comes to the front. |
| `powermac workspace list` | List the saved [workspaces](../features/workspaces.md) (id, name, windows, last launch). |
| `powermac workspace capture [<name>]` | Save the windows on screen as a new workspace ("Workspace N" without a name). |
| `powermac workspace launch <name\|id>` | Restore a workspace; prints each app's result (exit code 1 when one failed). |
| `powermac workspace edit` | Open the Workspaces editor. |
| `powermac net list [--all] [--json]` | Every app's current TCP/UDP connections ([Network Monitor](../features/network-monitor.md)); `--all` adds listening sockets. Read-only; use `sudo` on macOS to include other users' and system processes. |
| `powermac net watch [-i <secs>] [--all] [--json]` | Print connections as they open, close and change state. |
| `powermac net audit [--blocked] [--json]` | What the firewall rules would do to each current connection (nothing is blocked). |
| `powermac net rules list\|add\|remove\|enable\|disable\|default\|import\|check` | Edit `net-rules.json`: e.g. `add deny --app /Applications/Safari.app`, `add allow --app Safari --remote '*.apple.com' --port 443`, `import group.lsrules` (Little Snitch), `check <app> <host-or-ip[:port]>`. |
| `powermac permissions [--json]` | Each privacy permission, its state and the enabled features that use it; `--json` prints the live state for scripts (the settings window runs this to bypass macOS' per-process caching). Never prompts. |
| `powermac update check [--pre]` | Ask GitHub Releases for a newer version (one request; nothing is downloaded); see [Updates](updates.md). |
| `powermac mwb doctor` | Everything that decides whether a Windows PC can pair with this Mac, plus the PowerShell to run on the PC ([Pairing with Windows](../features/mouse-without-borders-pairing.md)). |
| `powermac editor [--monitor <id>]` | Open the FancyZones layout editor. |
| `powermac switcher-windows [--app]` | List the windows the [Window Switcher](../features/window-switcher.md) would show right now, in its order; `--app` lists only the front app's. Needs Accessibility. |
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
