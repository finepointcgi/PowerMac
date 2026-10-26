import CoreGraphics
import Foundation
// Scripted pointer for recording docs GIFs. Reads commands from stdin, one per line:
//   move X Y [ms]      glide to X,Y (global points) over ms (default 400)
//   click              left click where the pointer is
//   scroll N           scroll N lines, one line per event (positive = down
//                      with natural scrolling on)
//   type TEXT          type TEXT
//   key CODE           press a virtual key code
//   wait MS
let src = CGEventSource(stateID: .hidSystemState)
var pos = CGEvent(source: nil)!.location
func post(_ e: CGEvent?) { e?.post(tap: .cghidEventTap) }
func sleepMs(_ ms: Double) { usleep(useconds_t(ms * 1000)) }
while let line = readLine() {
  let p = line.split(separator: " ", maxSplits: 1).map(String.init)
  guard let cmd = p.first else { continue }
  let rest = p.count > 1 ? p[1] : ""
  let n = rest.split(separator: " ").compactMap { Double($0) }
  switch cmd {
  case "move":
    let to = CGPoint(x: n[0], y: n[1]); let ms = n.count > 2 ? n[2] : 400
    let steps = max(1, Int(ms / 10)); let from = pos
    for i in 1...steps {
      var t = Double(i) / Double(steps); t = t < 0.5 ? 2 * t * t : 1 - pow(-2 * t + 2, 2) / 2
      pos = CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t)
      post(CGEvent(mouseEventSource: src, mouseType: .mouseMoved, mouseCursorPosition: pos, mouseButton: .left))
      sleepMs(10)
    }
  case "click":
    post(CGEvent(mouseEventSource: src, mouseType: .leftMouseDown, mouseCursorPosition: pos, mouseButton: .left))
    sleepMs(70)
    post(CGEvent(mouseEventSource: src, mouseType: .leftMouseUp, mouseCursorPosition: pos, mouseButton: .left))
  case "scroll":
    for _ in 0..<Int(abs(n[0])) {
      post(CGEvent(scrollWheelEvent2Source: src, units: .line, wheelCount: 1, wheel1: n[0] < 0 ? -1 : 1, wheel2: 0, wheel3: 0))
      sleepMs(60)
    }
  case "type":
    for ch in rest.utf16 {
      var c = ch
      for down in [true, false] {
        let e = CGEvent(keyboardEventSource: src, virtualKey: 0, keyDown: down)
        e?.keyboardSetUnicodeString(stringLength: 1, unicodeString: &c)
        post(e)
      }
      sleepMs(90)
    }
  case "key":
    for down in [true, false] { post(CGEvent(keyboardEventSource: src, virtualKey: CGKeyCode(n[0]), keyDown: down)) }
  case "wait": sleepMs(n[0])
  default: break
  }
}
