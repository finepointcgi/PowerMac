# Text Extractor

Drag over any part of the screen and the text in it is copied. A port of PowerToys Text Extractor (PowerOCR).

Crate: `crates/text-extractor` · Settings page: **Text Extractor** · Config: `textExtractor`

## Using it

- **Ctrl+Option+T** (Win+Shift+T on Windows), or tray → Extract text.
- Drag a rectangle; Esc or a right-click cancels. A small toast confirms ("Copied 42 words").
- From the command line: `powermac ocr image.png`, `powermac ocr --clipboard`, or `powermac ocr` to select on screen.

## Settings

| Key | Meaning |
|---|---|
| `hotkey` | capture shortcut |
| `languages` | BCP-47 tags in order of preference (`["en-US", "de-DE"]`); empty = automatic |
| `singleLine` | join all lines into one |
| `showToast` | show the confirmation |
| `languageCorrection` | let the recognizer fix words (macOS) |

## How it works

- **macOS:** a dimmed overlay per display; capture with ScreenCaptureKit (macOS 14+), falling back to `CGWindowListCreateImage`; OCR with Vision (`VNRecognizeTextRequest`, accurate level). Capturing needs **Screen Recording**; files and clipboard images don't.
- **Windows:** the screen is frozen into a dimmed overlay, the selection is cut from it and read with `Windows.Media.Ocr`.
