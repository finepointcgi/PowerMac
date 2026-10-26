# Color Picker

Pick a color anywhere on screen and copy it in the format you need. A port of PowerToys Color Picker.

Crate: `crates/color-picker` · Settings page: **Color Picker** · Config: `colorPicker`

## Using it

- **Ctrl+Option+C** (Win+Shift+C on Windows), tray → Pick color, or `powermac color-picker`.
- On macOS this uses the system magnifier loupe (`NSColorSampler`), so no Screen Recording permission is needed.
- The color is copied in `copiedFormat` (HEX by default) and added to the history.
- The **editor** (`powermac color-picker --editor`) shows the history, a large swatch, lighter and darker variations, hue/saturation/brightness adjustment, and every visible format with a copy button.

## Settings

| Key | Meaning |
|---|---|
| `hotkey` | pick shortcut |
| `activation` | pick then open the editor (PowerToys default), pick and copy only, or open the editor directly |
| `copiedFormat` | format copied after a pick (HEX, RGB, HSL, HSV, CMYK, …) |
| `visibleFormats` | formats shown in the editor, in order |
| `customFormats` | your own format strings |
| `showColorName` | show a name such as "Light blue" |
| `historyLimit` | colors kept in `color-picker-history.json` |

Formats follow PowerToys' `ColorFormatHelper` exactly, so custom format strings carry over.
