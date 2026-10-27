# Network Monitor and firewall rules

See which apps are talking to the network right now, and write per-app firewall rules for them, after Little Snitch's Network Monitor and Radio Silence's app list.

Crate: `crates/net-monitor` · Command line: `powermac net …` · Rules: `net-rules.json` in the data folder

!!! warning "The rules aren't enforced yet"
    The monitor is read-only and the rules are a **model**: PowerMac stores them, checks them against live traffic (`powermac net audit`) and tells you what they *would* do, but it blocks nothing. Blocking needs a Network Extension content filter on macOS, which requires an entitlement only Apple's paid developer program can grant, or a WFP callout driver on Windows.

## Monitor

```bash
powermac net list            # apps with connections, grouped by app
powermac net list --all      # plus listening and unconnected UDP sockets
powermac net list --json     # the same, as JSON
powermac net watch           # print connections as they open (+), close (-) and change state (~)
powermac net watch -i 0.25 --json
```

```text
Google Chrome  (pid 812, 840; 3 connections, 1 listening)
    TCP 192.168.1.5:50001 → 142.250.1.1:443 ESTABLISHED  [internet]
    UDP 192.168.1.5:61022 → 142.250.1.1:443  [internet]
    …
22:14:10 + curl[52980] TCP 192.168.1.5:60131 → 93.184.215.14:80 SYN_SENT
22:14:11 - curl[52980] TCP 192.168.1.5:60131 → 93.184.215.14:80 ESTABLISHED after 1.2 s
```

- **Per app.** Sockets are grouped by `.app` bundle, so helper processes (`Google Chrome Helper`, WebKit's networking process inside its app) count as their app. Tools outside a bundle are grouped by executable path.
- **Direction.** A TCP connection on a port the same process listens on was accepted (incoming); everything else is outgoing. Listeners and UDP sockets without a peer are shown with `--all`.
- **Scope.** Each remote address is labelled loopback, local network (private, link-local, CGNAT, unique-local, multicast) or internet. IPv4-mapped IPv6 addresses are shown as IPv4.
- **Polling.** `watch` compares snapshots of the socket table (every second by default), so a connection shorter than one interval, such as a quick DNS query, can go unseen. Nothing is captured: no packets or contents are read, and no traffic counters are kept.

### Who can be seen

- **macOS:** libproc (`proc_pidinfo` / `proc_pidfdinfo`, the source of `lsof -i`). A normal user sees only their own processes' sockets; the number of other processes is printed (`342 processes of other users or the system weren't readable`). Run `sudo powermac net list` to include system daemons. No privacy permission is involved.
- **Windows:** the IP Helper owner-pid tables (`GetExtendedTcpTable` / `GetExtendedUdpTable`, the source of `netstat -ano`), which every user can read; the image paths of a few system processes need an elevated prompt. The UDP tables have no remote address, so UDP sockets show as bound only.

## Rules

A rule says what to do with an app's traffic to a remote end:

| Field | Values |
|---|---|
| action | `allow`, `deny`, `ask` (prompt, once something enforces rules) |
| app | any app, an `.app` **bundle** (with its helpers), an exact **executable path**, or an executable / app **name** (`curl`, `Google Chrome`) |
| remote | any, `local-net`, an address (`1.2.3.4`), a network (`10.0.0.0/8`), a host (`api.example.com`) or a domain with its subdomains (`*.example.com`) |
| ports | any, `443`, `80,443`, `8000-8100` (remote port) |
| protocol | TCP, UDP or both |
| direction | outgoing (default), incoming, both |
| high priority | wins over every regular rule |

**Which rule wins.** Of the enabled rules that match, the most specific one decides, as in Little Snitch: high priority first, then a path over a bundle over a name over any app, then an address or host over a network or domain over the local network over any server, then a port list over any port, a protocol over both, one direction over both. Between equally specific rules **deny** beats **ask** beats **allow**. When no rule matches, the default action applies: **allow**, so a Radio Silence-style list of apps to block works as is. `powermac net rules default deny` turns it into an allow-list.

Host and domain rules need the remote's host name, which the socket table doesn't have (it lists addresses), so in `audit` they only decide connections checked with a host (`rules check`); address, network and local-network rules work on live traffic.

```bash
powermac net rules add deny --app /Applications/Safari.app                    # block all of Safari's traffic
powermac net rules add allow --app Safari --remote '*.apple.com' --port 443    # …except Apple's servers on 443
powermac net rules add allow --remote local-net --direction both               # any app may use the LAN
powermac net rules list
powermac net rules check /Applications/Safari.app/Contents/MacOS/Safari www.apple.com:443
#   allow (rule r2: allow Safari → *.apple.com, port 443)
powermac net rules disable r1    # also enable, remove
powermac net audit --blocked     # what the rules would deny or ask about right now
```

### Importing Little Snitch rules

`powermac net rules import group.lsrules` reads a Little Snitch rule group (the JSON `.lsrules` format used for rule group subscriptions). Every entry of `remote-hosts`, `remote-domains` and `remote-addresses` becomes one rule, and `denied-remote-hosts` / `-domains` / `-addresses` become deny rules for any app. `process`, `action`, `direction`, `ports`, `protocol`, `priority`, `disabled` and `notes` are kept, and the group's name is recorded as each rule's source. What the model can't express is listed as skipped instead of being widened: `via`, address ranges (`a.b.c.d-e.f.g.h`; use a network), and remotes such as `bonjour`, `dns-servers`, `multicast` or `broadcast`.

### `net-rules.json`

```json
{
  "version": 1,
  "defaultAction": "allow",
  "rules": [
    { "id": "r1", "enabled": true, "action": "deny",
      "app": { "type": "bundle", "value": "/Applications/Safari.app" },
      "direction": "outgoing", "remote": { "type": "any" }, "created": 1793050000 },
    { "id": "r2", "enabled": true, "action": "allow",
      "app": { "type": "bundle", "value": "/Applications/Safari.app" },
      "direction": "outgoing", "protocol": "tcp",
      "remote": { "type": "domain", "value": "apple.com" },
      "ports": [{ "start": 443, "end": 443 }], "created": 1793050000 }
  ]
}
```

`app.type` is `any`, `bundle`, `path` or `name`; `remote.type` is `any`, `localNetwork`, `address`, `network` (`"value": {"addr": "10.0.0.0", "prefix": 8}`), `host` or `domain`. Missing fields take their defaults (enabled, deny, outgoing, any). The file is written atomically, and `net rules …` commands rewrite it.

## How it works

- `net_monitor::snapshot()` returns every process's TCP/UDP sockets; `Snapshot::by_app()` groups them; `Monitor` diffs successive snapshots into `Opened` / `Closed` / `StateChanged` events and `watch()` polls.
- On macOS the `struct socket_fdinfo` that `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)` fills in isn't in the `libc` crate, so its fields are read at their offsets from `sys/proc_info.h` (checked against the SDK; the struct is 792 bytes). A test opens a socket in the test process and finds it in a live snapshot.
- `rules::RuleSet::evaluate` answers a `Query` (process, direction, protocol, remote address / host / port); `audit` runs it over a snapshot.
