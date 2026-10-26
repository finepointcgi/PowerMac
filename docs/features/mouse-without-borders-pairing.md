# Pairing with a Windows PC

This page explains how PowerToys Mouse Without Borders (MWB) finds and accepts other machines, why typing the Mac's IP address into PowerToys' **Connect** does nothing, and the steps that work with PowerMac. Run `powermac mwb doctor` on the Mac first. It checks everything below and prints the PowerShell commands to run on the PC, filled in with your Mac's name and address.

## Short version

On the PC, in **PowerToys Settings → Mouse Without Borders**:

1. Click **Connect**. Enter the security key and the Mac's **name** as PowerMac shows it (Settings → Mouse Without Borders, or `powermac mwb doctor`), for example `MacBook-Pro`. Don't enter the IP address. Upper or lower case doesn't matter.
2. PowerMac answers Windows' LLMNR name queries for that name, so Windows can find the Mac by name without any setup. If the name still doesn't resolve on the PC (`Resolve-DnsName MACBOOK-PRO -LlmnrOnly` finds nothing, for example because a policy turned LLMNR off), open **Advanced → IP address mapping** and add one line with the name, a **space**, and the Mac's address:

    ```
    MACBOOK-PRO 10.0.0.112
    ```

    The separator is a space, not a colon. Then click **Connect** again.
3. The PC's firewall can stay closed. One TCP connection from the PC to the Mac's port 15101 carries everything: input both ways, clipboard text and the layout. If you also open TCP 15100–15101 on the PC, PowerMac connects back and keeps the second socket (the usual MWB pair). It also finds the PC by itself when **Find other computers automatically** is on.
4. If the Mac runs a VPN such as NordVPN, quit it or allow LAN traffic in it. With the VPN running, the Mac's replies leave through the tunnel and the PC's connection never completes.

## How PowerToys finds a peer

All citations are to `src/modules/MouseWithoutBorders/App` in PowerToys (`Class/…`, `Core/…`) and to `src/settings-ui` (`Settings.UI…`).

### The Connect button

* `MouseWithoutBordersViewModel.SubmitConnectionRequestAsync` (`Settings.UI/ViewModels/MouseWithoutBordersViewModel.cs:621`) trims the name and the key (`:628`) and calls `ConnectToMachine(pcName, securityKey)` over IPC (`:652`).
* `SettingsSyncHelper.ConnectToMachine` (`Class/Program.cs:347`) sets the key. It then **replaces the machine matrix** with `{ pcName.Trim().ToUpper(CurrentCulture), Common.MachineName, "", "" }` (`:358`), initializes the machine pool from it, reopens the sockets (`Common.ReopenSockets(true)`) and sends the matrix. Nothing else happens with the name. There is no discovery and no retry loop. The only later reconnects are triggered by events (a peer connecting, a received matrix, a send error, and one helper-timer tick at startup, `Form/frmScreen.cs:530–536`).

### From a matrix name to a TCP connection

`SocketStuff.UpdateTCPClients` (`Class/SocketStuff.cs:797`) calls `StartNewTcpClient(name)` for every matrix entry that isn't this machine and has no connected client socket. `StartNewTcpClient` (`:884`) works like this:

1. **IP address mapping** (`Setting.Values.Name2IP`, `:904–944`). The text is split into lines on `"\r\n"` (`GetName2IpMappingLines`, `:1260`; the settings UI normalizes line endings to `\r\n`, view model `:1165`). Each line is split on **spaces** (`BlankSeparator`, `:1258`). A line counts when its first word equals the matrix name (`OrdinalIgnoreCase`) and its second word parses as an IP address. Every such address gets a connection thread right away, before any DNS lookup. The UI's placeholder text is `Example: MyLaptop 192.168.0.24` (`Settings.UI/Strings/en-us/Resources.resw:465`). There is no `NAME:IP,...` syntax.
2. **Name resolution** with `Dns.GetHostEntry(machineName)` (`:949`). A `SocketException` only marks the status socket as timed out (`:951–960`).
3. Valid addresses are the IPv4 and IPv6 entries of the result. **ReverseLookup** (`ValidateRemoteMachineIP`, default `false`, `Settings.UI.Library/MouseWithoutBordersProperties.cs:189`) would also require each address's reverse name to start with the machine name (`:990–1030`).
4. **CheckForSameSubNet** (`:1064`) does nothing unless `SameSubnetOnly` is on (default `false`, `MouseWithoutBordersProperties.cs:190`). When on, it only compares the first two octets.
5. With no address, the log says "Cannot resolve IPv4 Addresses of machine" and nothing else happens (`:1048`). A tooltip appears only when no mapping was used (`:1050`).
6. `StartNewTcpClientThread` connects to `ip : TcpPort + 1` (`:1168`). That is the PC's **own** port setting plus one, so both machines must use the same port (15100 by default, which makes the message port 15101).

### Why "10.0.0.112" never produced a connection

When the matrix entry is an IP-address string, `Dns.GetHostEntry` doesn't return that address. .NET first does a **reverse lookup** (`getnameinfo` with `NI_NAMEREQD`), then a forward lookup of the name it found (`System.Net.NameResolution`, `Dns.GetHostEntryOrAddressesCore(IPAddress)`). The interop harness shows the reverse lookup: `dns-probe 127.0.0.1` returns `HostName = "localhost"`. On a home network, the router's DNS usually has no PTR record for the Mac, so the call throws "No such host is known". `host` is then `null`, there are no valid addresses, and **no TCP connection is attempted**. That matches the field log: nothing reached 15100 or 15101, although `Test-NetConnection 10.0.0.112 -Port 15101` worked.

The address isn't the right thing to type anyway. Suppose the PC did reach the Mac through a mapping such as `10.0.0.112 10.0.0.112`. In the handshake, the Mac says its name is `MacBook-Pro`. PowerToys renames the socket to that claimed name (`:1421`), finds no `MacBook-Pro` in its pool (`:1458`), and treats the Mac as a new machine on its first heartbeat (`Core/Receiver.cs:210–222`). `MachineStuff.AddToMachinePool` (`Core/MachineStuff.cs:712`) then learns the name and rebuilds the matrix from the pool (`:770`). It also calls `Common.ReopenSockets(true)` (`:771`), which closes every socket, and redials by the name `MacBook-Pro`. So the name has to resolve in the end. The interop test `connect_with_ip_and_mapping_for_the_ip_redials_by_name` reproduces this sequence.

### What the name has to match

* The pool and matrix compare names with `OrdinalIgnoreCase` (`Class/MachinePool.cs:309`, `Core/MachineStuff.cs:1100`). The upper-casing in `ConnectToMachine` therefore doesn't matter: `MACBOOK-PRO` matches the Mac's `MacBook-Pro`. Only the socket rename at `:1421` compares with `Ordinal`, and it only renames.
* Broadcasts (heartbeats, layout, clipboard) go only to sockets whose name is **in the matrix** (`Core/Common.cs:926`). This is another reason the matrix entry must be the Mac's name.
* PowerToys truncates its own name to 32 characters (`Common.GetMachineName`, `Core/Common.cs:1383`). It applies no 15-character NetBIOS limit. PowerMac uses the host name without `.local`, at most 32 characters (`Config::effective_machine_name`).
* Typing `MacBook-Pro.local` (Bonjour) gets the PC connected but leads to the same new-machine, reopen and redial-by-short-name sequence as an IP address.

### Accepting a machine that connects in

Any machine that completes the key handshake on the listener is trusted. Its heartbeat adds it to the pool and the matrix (`AddToMachinePool`, "automatically active bidirectional control", `Core/MachineStuff.cs:712–777`). This is why PowerMac's discovery works whenever the PC's port 15101 is reachable from the Mac. It wasn't reachable in the field, because the Windows firewall dropped the Mac's connections.

### One socket or two?

MWB normally ends up with two sockets per peer, one in each direction. Sending doesn't depend on direction: `Common.SkSend` writes to any connected socket whose machine id matches, and for broadcasts to any whose name is in the matrix (`Core/Common.cs:926`). So the PC→Mac socket alone is enough. PowerMac also sends on whichever trusted socket exists. When the PC firewall drops PowerMac's connection back, PowerMac retries only once a minute while the PC's socket is up.

## What PowerMac does

* **LLMNR responder** (`crates/mwb/src/llmnr.rs`, RFC 4795). It listens on UDP 5355, which needs no root, on 224.0.0.252 and ff02::1:3 on every interface, and answers unicast queries too. It answers `A` queries for the MWB name (case-insensitive, with or without `.local`) with the Mac's IPv4 address on the interface the query came in on. `AAAA` queries get an empty answer, because the listener is IPv4-only. `PTR` queries for the Mac's own addresses get the name. Config: `answer_name_queries` (alias `answerNameQueries`, default on) and `llmnr_port` (default 5355). It is part of the engine and stops with it. It isn't used on Windows, where the OS answers for its own computer name. NetBIOS name service (UDP 137) would need root and isn't provided. On Windows, `getaddrinfo` asks DNS for a single-label name first and then LLMNR, so the Mac's answer comes after the DNS lookup fails.
* **Connecting back.** When a PC connects in, PowerMac remembers its source address (`ip : port+1`, the same port rule as PowerToys) and uses it to dial back even if the PC's name doesn't resolve on the Mac. PowerMac also puts the PC to the right of the Mac in its layout, even with discovery off (like `AddToMachinePool`). If the dial-back fails, the single socket is used.
* **Diagnostics.** The engine keeps the last 16 incoming connections ("connected", "trusted as 'PC'", or why the connection ended) and the LLMNR counters. The app writes them to `mwb-diagnostics.json` next to `config.json`, and `powermac mwb doctor` prints them. A `Test-NetConnection` from the PC shows up there as `ended before the handshake: failed to fill whole buffer`, because it connects and hangs up.

## `powermac mwb doctor`

The doctor prints:

* the Mac's MWB name (and what to type), Bonjour name, IPv4 addresses and the one Windows should use;
* whether TCP 15101 is listening and which process listens;
* key validity and the bit-31 magic-number quirk;
* whether the LLMNR responder answers, by querying it at 127.0.0.1 and at the Mac's own LAN address;
* the default route and routes to known peers, flagging VPN tunnels (`utun…`);
* the macOS firewall state for the listening app;
* the peers and incoming connections the running app saw;
* the PowerShell for the PC: `Test-NetConnection`, `Resolve-DnsName -LlmnrOnly`, `[System.Net.Dns]::GetHostEntry` (the exact call PowerToys makes), the LLMNR policy and network profile checks, an optional inbound firewall rule, and the IP-mapping line.

## Tests

* `crates/mwb/src/llmnr.rs`: packet parsing and drop rules, name matching, answers, response bytes, per-interface address choice, and a live query to a responder bound to loopback. `live_query_on_own_lan_address` is `#[ignore]` because the macOS firewall may prompt for the unsigned test binary.
* `crates/mwb/tests/pairing.rs` drives `interop/PowerToysFlow.cs`, a port of PowerToys' Connect path: `ConnectToMachine` → matrix/pool → `UpdateTCPClients` → `StartNewTcpClient` (IP mapping, `Dns.GetHostEntry` semantics against an in-memory table) → `MainTCPRoutine` → `AddToMachinePool` / `SkSend`. It is driven like the Connect button. The tests check:
    * Connect with an IP address: no TCP connection, as in the field.
    * Connect with the name plus a mapping, with a firewalled PC: pairing over one socket, input, clipboard and layout both ways.
    * Connect with the IP plus a mapping for the IP: the PC reopens its sockets and redials by name.
    * A listening PC: both sockets.

    The harness can't use LLMNR, because .NET on macOS resolves through `getaddrinfo`/mDNSResponder. So these tests cover the name matching, the handshake and the matrix logic through the mapping path.
