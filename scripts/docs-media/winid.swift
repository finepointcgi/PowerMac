import CoreGraphics
import Foundation
// usage: winid <pid> [title-substring] -> prints "id x y w h" for matching on-screen windows
let args = CommandLine.arguments
let pid = Int32(args[1])!
let sub = args.count > 2 ? args[2] : ""
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
for w in list {
  guard (w[kCGWindowOwnerPID as String] as? Int32) == pid else { continue }
  let name = w[kCGWindowName as String] as? String ?? ""
  if !sub.isEmpty && !name.contains(sub) { continue }
  let b = w[kCGWindowBounds as String] as! [String: CGFloat]
  if (b["Width"] ?? 0) < 200 { continue }
  print(w[kCGWindowNumber as String]!, Int(b["X"]!), Int(b["Y"]!), Int(b["Width"]!), Int(b["Height"]!), name)
}
