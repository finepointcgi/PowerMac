# Mouse Without Borders

Share one keyboard and mouse, plus clipboard text, across up to four machines. It is **wire-compatible with Windows PowerToys Mouse Without Borders**, so a Mac running PowerMac can join a matrix of PCs running PowerToys.

Crate: `crates/mwb` (protocol + engine), `crates/pm-input` (capture/injection) · Settings page: **Mouse Without Borders** · Config: `mouseWithoutBorders`

<figure markdown="span">
  ![The Mouse Without Borders settings page](../assets/screenshots/settings-mouse-without-borders.png){ loading=lazy }
  <figcaption>The Mouse Without Borders settings page.</figcaption>
</figure>

## Pairing with Windows PowerToys

1. In PowerToys → Mouse Without Borders on the PC, copy its security key and machine name. You can also go the other way: tray → **Copy security key** in PowerMac, and paste it into PowerToys.
2. In **Settings → Mouse Without Borders**, enter the key.
3. With **Find other computers automatically** on (the default), PowerMac finds the PC itself (see below). This needs the PC's TCP 15101 to be reachable through its firewall.
4. Or connect from the PC: in PowerToys use **Connect** with the key and the Mac's **name** (for example `MacBook-Pro`), not its IP address. PowerMac answers Windows' LLMNR name queries, so the name resolves without setup. If it doesn't, add `MACBOOK-PRO 10.0.0.112` (name, space, address) under PowerToys → Advanced → **IP address mapping**. One direction is enough, so the PC's firewall can stay closed.
5. Allow TCP **15101** through the firewall on the machine that accepts the connection. Quit VPNs such as NordVPN on the Mac, or allow LAN traffic in them.
6. Move the pointer past the screen edge to switch machines.

If it doesn't pair, run `powermac mwb doctor`. [Pairing with a Windows PC](mouse-without-borders-pairing.md) explains how PowerToys resolves and accepts machines, with source references, and why typing an IP address into **Connect** does nothing.

**Swap Cmd/Ctrl** (on by default on macOS) makes Cmd+C on the Mac arrive as Ctrl+C on Windows, and the other way round.

## Automatic discovery

PowerToys has no discovery of its own, but accepts any machine that completes the key handshake and then adds it to its layout. So PowerMac probes the MWB port on the hosts of this Mac's private subnets (RFC 1918 and link-local only; at most 1024 hosts per network, the /24 around the Mac on larger ones):

- every 60 s while nothing is connected, every 5 min once something is (`discoveryIntervalSecs`, `discoveryIntervalConnectedSecs`);
- right away with **Search now** in Settings or tray → **Find computers now**.

A machine that completes the handshake is connected, placed to the right of this Mac in the layout and its address saved. A machine with a different key is left alone for 30 minutes.

## Settings

| Key | Default | Meaning |
|---|---|---|
| `key` | generated | shared security key (≥ 16 chars, whitespace stripped) |
| `machineName` | host name | name on the wire (≤ 32 ASCII chars) |
| `matrix` | [this machine] | up to 4 names in layout order |
| `oneRow` | true | one row, or a 2×2 grid |
| `wrapAround` | false | wrap at the matrix ends |
| `easyMouse` | true | switch at screen edges |
| `hideCursorWhenAway`, `moveMouseRelatively` | true, false | as in PowerToys |
| `port` | 15100 | base TCP port (messages use port + 1) |
| `peers` | {} | name → `host`, `ip` or `host:baseport` |
| `shareClipboard` | true | exchange clipboard text |
| `swapCmdCtrl` | true on macOS | swap Cmd and Ctrl on forwarded keys |
| `autoDiscover` | true | LAN discovery |
| `answer_name_queries` | true | answer LLMNR name queries (UDP 5355) for `machineName`, so Windows finds the Mac by name |
| `llmnr_port` | 5355 | LLMNR port (change only for tests) |

## How it works

- `protocol`: the `DATA` package layout, package types, magic/checksum stamping.
- `crypto`: key normalization, 24-bit magic hash, **PBKDF2 + AES-256-CBC** stream, byte-compatible with the C# code.
- `transport`, `clipboard` (deflate + UTF-16 + chunking), `matrix`, `discovery`, `engine` (sockets, handshake, switching, forwarding, injection).
- `pm-input` captures with a `CGEventTap` and injects with `CGEventPost` on macOS (low-level hooks and `SendInput` on Windows). Events are expressed in Windows semantics (VK codes, `WM_*` messages) because that is what the wire carries.
- `crates/mwb/tests/interop.rs` builds a .NET harness from PowerToys' own encryption and packet code and checks test vectors and live sessions in both directions.

## Limitations

- No file transfer, drag-and-drop or clipboard images (clipboard text works).
- About 1 in 512 keys trips a C# quirk that makes Windows MWB reject every packet. PowerMac warns about it; generate a new key if you see the warning.
