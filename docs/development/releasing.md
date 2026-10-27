# CI and releases

## Continuous integration

`.github/workflows/ci.yml` runs on every push to `master`/`main` and every pull request (documentation-only changes are skipped):

| Job | Runner | What it does |
|---|---|---|
| macOS | `macos-14` | `cargo clippy --workspace --all-targets` (errors fail, warnings don't), `cargo test --workspace`, builds `PowerMac.app` and verifies its signature and `--version` |
| Windows | `windows-latest` | `cargo build --workspace --all-targets` (tests aren't run: the Windows backends haven't been run on Windows yet) |

The documentation site has its own workflow (`docs.yml`, see [Writing documentation](docs.md)).

## Cutting a release

1. Set the version in the root `Cargo.toml` (`[workspace.package] version`), and run `cargo check` so `Cargo.lock` follows.
2. Add a `## [X.Y.Z] - YYYY-MM-DD` section to `CHANGELOG.md`. It becomes the release notes.
3. Commit, push and wait for CI to pass.
4. Tag and push:

    ```bash
    git tag -a vX.Y.Z -m "PowerMac X.Y.Z"
    git push origin vX.Y.Z
    ```

`.github/workflows/release.yml` then:

1. checks that the tag matches the workspace version;
2. runs the tests in release mode;
3. builds a **universal** (Apple Silicon + Intel) `PowerMac.app` with `UNIVERSAL=1 scripts/bundle-macos.sh`;
4. packages `PowerMac-X.Y.Z-macos-universal.dmg` (`scripts/package-dmg.sh` with `UNIVERSAL=1 SKIP_BUILD=1`) and `PowerMac-X.Y.Z-macos-universal.zip`, plus `SHA256SUMS.txt` (the app's update check looks for `-macos-<arch>` / `-universal` DMGs in the release assets);
5. signs and notarizes them when the Apple secrets are set;
6. publishes a GitHub Release with the changelog section as its notes.

The workflow ad-hoc signs (CI doesn't have the local "PowerMac Local Signing" identity) unless the Developer ID secrets below are set. Don't publish anything by hand: the app's [update check](../getting-started/updates.md) offers every non-draft release to everyone running PowerMac.

## A DMG on your Mac

```bash
CARGO_TARGET_DIR=target scripts/package-dmg.sh
```

builds the bundle, makes `target/PowerMac-X.Y.Z-macos-arm64.dmg` (the architecture of this Mac; `UNIVERSAL=1` for both), signs it with the local identity when it exists, runs `hdiutil verify` and prints the size and SHA-256 (also written to `….dmg.sha256`). `FANCY=1` adds a background picture with an arrow to Applications (drawn by `scripts/dmg-background.py`; laying out the window asks Finder through AppleScript, so macOS asks once for Automation). The image isn't notarized; on other Macs the first launch needs **Open Anyway** in Privacy & Security (see [Install](../getting-started/install.md#opening-a-build-that-isnt-notarized)).

To rebuild an existing tag, run the workflow by hand (**Actions → release → Run workflow**) with the tag name; it replaces the assets.

## Signing and notarization

Without Apple credentials the app is ad-hoc signed. It runs, but Gatekeeper blocks the first launch until the user right-clicks → **Open**; the release notes then say so. For signed, notarized builds add these repository secrets (**Settings → Secrets and variables → Actions**):

| Secret | Value |
|---|---|
| `MACOS_CERT_P12_BASE64` | Developer ID Application certificate exported as `.p12`, then `base64 -i cert.p12` |
| `MACOS_CERT_PASSWORD` | the `.p12` password |
| `MACOS_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | the Apple ID used for notarization |
| `APPLE_TEAM_ID` | the 10-character team id |
| `APPLE_APP_PASSWORD` | an app-specific password for that Apple ID |

With them set, `bundle-macos.sh` signs with the hardened runtime and `scripts/PowerMac.entitlements` (Apple Events, camera, audio input), and the workflow notarizes and staples the DMG and the app.
