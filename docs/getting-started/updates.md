# Updates

PowerMac checks [GitHub Releases](https://github.com/finepointcgi/PowerMac/releases) for a newer version. It never downloads or installs anything by itself: it tells you, shows the release notes and gives you a **Download** button that opens the disk image (or the release page) in your browser.

## When it checks

- Once a day while PowerMac runs (`updates.check`, on by default; the first check runs half a minute after startup). The time of the last check is `updates.lastCheck` in `config.json`.
- When you click **Check now** in **Settings → General → Updates**.
- `powermac update check [--pre]` from the command line.

Each check is one unauthenticated request to `https://api.github.com/repos/finepointcgi/PowerMac/releases/latest` (with **Include beta versions**, `…/releases?per_page=15`), made with the system `curl`. Versions are compared as [SemVer](https://semver.org): `1.2.0` is newer than `1.2.0-beta.3`, which is newer than `1.1.9`.

| GitHub answers | PowerMac shows |
|---|---|
| a newer release | **Update available: vX.Y.Z** in the menu bar menu (it opens Settings → General) and a card with the notes and **Download** |
| the same or an older version | "You're up to date" |
| 404 (no releases published yet) | "No releases have been published yet" |
| 403 / 429 rate limit (60 requests an hour per address) | "GitHub's rate limit was reached; try again in N min" |
| no network, DNS failure, timeout | "can't reach GitHub"; the daily check tries again an hour later |

The result is kept in `update-status.json` next to `config.json`, so the tray and the settings window agree.

## Installing an update

1. Click **Download** (it opens `PowerMac-X.Y.Z-macos-<arch>.dmg`).
2. Open the disk image and drag PowerMac onto **Applications**, replacing the old one. Your settings in `~/Library/Application Support/PowerMac/` stay.
3. Quit PowerMac from its menu and open it again.

If the new build is signed differently from the old one, macOS may list PowerMac twice in **Privacy & Security**; the setup's [permission rows](permissions.md#outdated-entries) explain how to clean that up.

## Turning it off

**Settings → General → Updates → Check for updates automatically**, the last step of the setup, or `"updates": {"check": false}` in `config.json`.
