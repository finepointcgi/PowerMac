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
3. With **Find other computers automatically** on (the default), PowerMac finds the PC itself (see below). Otherwise put both machines in the matrix in left-to-right order, and if the PC's name doesn't resolve add a peer address such as `192.168.1.20`.
4. Allow TCP **15101** (and 15100) through the firewall on both machines.
5. Move the pointer past the screen edge to switch machines.

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

## How it works

- `protocol`: the `DATA` package layout, package types, magic/checksum stamping.
- `crypto`: key normalization, 24-bit magic hash, **PBKDF2 + AES-256-CBC** stream, byte-compatible with the C# code.
- `transport`, `clipboard` (deflate + UTF-16 + chunking), `matrix`, `discovery`, `engine` (sockets, handshake, switching, forwarding, injection).
- `pm-input` captures with a `CGEventTap` and injects with `CGEventPost` on macOS (low-level hooks and `SendInput` on Windows). Events are expressed in Windows semantics (VK codes, `WM_*` messages) because that is what the wire carries.
- `crates/mwb/tests/interop.rs` builds a .NET harness from PowerToys' own encryption and packet code and checks test vectors and live sessions in both directions.

## Limitations

- No file transfer, drag-and-drop or clipboard images (clipboard text works).
- About 1 in 512 keys trips a C# quirk that makes Windows MWB reject every packet. PowerMac warns about it; generate a new key if you see the warning.
