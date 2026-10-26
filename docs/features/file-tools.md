# File Tools

PowerRename, New+ and Image Resizer ported from PowerToys, plus "Finder is missing basics" helpers, all reachable from Finder's right-click menu.

Crate: `crates/file-tools` · Settings page: **File Tools** · Config: `file-tools.json`

<figure markdown="span">
  ![The File Tools settings page](../assets/screenshots/settings-file-tools.png){ loading=lazy }
  <figcaption>The File Tools settings page.</figcaption>
</figure>

## Finder Quick Actions

Install them from **Settings → File Tools** or the tray. They appear when you right-click selected items (Quick Actions submenu, or Services):

| Quick Action | Runs |
|---|---|
| PowerMac — Rename with PowerRename | `powermac rename <items…>` |
| PowerMac — New file here | `powermac new --dir <folder>` (a selected file uses its folder) |
| PowerMac — Open in Terminal | `powermac finder open-terminal <items…>` |
| PowerMac — Open in Editor | `powermac finder open-editor <items…>` |
| PowerMac — Copy Path | `powermac finder copy-path <items…>` |
| PowerMac — Resize Images (images only) | `powermac resize <items…>` |

They are Automator workflows in `~/Library/Services`, each a single "Run Shell Script" action, so they need no extra permission. macOS offers Quick Actions only for selected items, not for the empty background of a window; right-click the folder itself (e.g. in the path bar) to create a file inside it.

On Windows the same verbs are added under `HKCU\Software\Classes` (Explorer context menu).

## PowerRename

Bulk rename with search and replace, regular expressions, case transforms and a live preview of every change before it is applied. Options and recent search/replace terms can be remembered (`rename.rememberOptions`, `searchMru`, `replaceMru`), and the list can show only the items that will change.

## New+

Create files and folders from your templates. Put templates in `~/Library/Application Support/PowerMac/Templates/` (or `newPlus.templatesDir`).

- Leading ordering digits ("01. Notes.txt") can be hidden in the picker and the new name.
- Variables in names and text contents are expanded (`$YYYY`, `$PARENT_FOLDER_NAME`, `%ENV%`, …) when `replaceVariables` is on.
- The new item can be revealed and/or opened afterwards.

## Image Resizer

Presets (Small 854×480, Medium 1366×768, Large 1920×1080, Phone 320×568, custom), fit modes (fill, fit, stretch), units (px, %, cm, in), shrink only, ignore orientation, replace originals or `%1 (%2)`-style names, output format, JPEG quality and metadata handling (strip, remove GPS, keep date). Size math and naming follow PowerToys' `ResizeOperation.cs`. ImageIO/CoreGraphics on macOS, WIC on Windows. See [`powermac resize`](../getting-started/cli.md#powermac-resize).

## Finder basics

| Action | Notes |
|---|---|
| Open in Terminal / Editor | `finder.terminalApp`, `finder.editorApp` (bundle id or app path; empty = default Terminal / first installed of VS Code, Cursor, Zed, Sublime Text, …) |
| Copy path | `finder.pathStyle`: posix, tilde, url, name or quoted |
| Show / hide hidden files | restarts Finder |
| Eject all | external volumes; optionally disk images and network shares |
| Who uses this | processes holding a file, folder or volume open |

Acting on the front Finder window without a selection (`useFrontFinderWindow`) asks Finder through AppleScript and needs the Automation permission.
