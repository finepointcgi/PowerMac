//! File-manager integration.
//!
//! **macOS:** two Finder Quick Actions (Services) are generated as Automator
//! `.workflow` bundles in `~/Library/Services`:
//!
//! * "PowerMac — Rename with PowerRename" runs `<exe> rename <items…>`.
//! * "PowerMac — New file here" runs `<exe> new --dir <folder>`; for a
//!   selected file it uses the file's folder.
//!
//! macOS offers Quick Actions for *selected* items: right-click a file or
//! folder (Quick Actions submenu, or Services), or use Finder ▸ Services in a
//! Finder window with something selected. There is no entry for the empty
//! background of a window; right-click the folder itself (e.g. in the path
//! bar or the parent folder) to create a file inside it. Each workflow is a
//! single "Run Shell Script" action (input as arguments), so it runs without
//! Automator being open and without extra permissions.
//!
//! **Windows:** `HKCU\Software\Classes` verbs (via `reg.exe`):
//! `*\shell\PowerMacRename` and `Directory\shell\PowerMacRename` ("Rename
//! with PowerRename"), `Directory\shell\PowerMacNew` and
//! `Directory\Background\shell\PowerMacNew` ("New file here").

use std::path::{Path, PathBuf};

/// The Quick Actions PowerMac installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAction {
    Rename,
    NewFile,
}

impl QuickAction {
    pub const ALL: [QuickAction; 2] = [QuickAction::Rename, QuickAction::NewFile];

    /// The menu title (also the bundle name).
    pub fn title(self) -> &'static str {
        match self {
            QuickAction::Rename => "PowerMac — Rename with PowerRename",
            QuickAction::NewFile => "PowerMac — New file here",
        }
    }

    pub fn bundle_name(self) -> String {
        format!("{}.workflow", self.title())
    }

    /// The shell script the action runs (`exe` is single-quoted). The app is
    /// started in the background so the Quick Action finishes right away.
    pub fn script(self, exe: &Path) -> String {
        let q = shell_quote(&exe.to_string_lossy());
        match self {
            QuickAction::Rename => format!(
                "# PowerMac: rename the selected items with PowerRename.\n\
                 [ \"$#\" -gt 0 ] || exit 0\n\
                 {q} rename \"$@\" >/dev/null 2>&1 &\n"
            ),
            QuickAction::NewFile => format!(
                "# PowerMac: create a new file from a template in the selected folder\n\
                 # (or next to the selected file).\n\
                 d=\"${{1:-$HOME/Desktop}}\"\n\
                 [ -d \"$d\" ] || d=\"$(dirname \"$d\")\"\n\
                 {q} new --dir \"$d\" >/dev/null 2>&1 &\n"
            ),
        }
    }
}

/// `'s'` with embedded single quotes escaped, for /bin/sh.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// XML-escape for plist strings.
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const PLIST_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";

/// `Contents/Info.plist` of the workflow bundle.
pub fn info_plist(action: QuickAction) -> String {
    let title = xml(action.title());
    format!(
        "{PLIST_HEAD}<dict>\n\
         \t<key>CFBundleDevelopmentRegion</key>\n\t<string>en_US</string>\n\
         \t<key>CFBundleIdentifier</key>\n\t<string>com.powermac.quickaction.{id}</string>\n\
         \t<key>CFBundleName</key>\n\t<string>{title}</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>1.0</string>\n\
         \t<key>NSServices</key>\n\t<array>\n\t\t<dict>\n\
         \t\t\t<key>NSBackgroundColorName</key>\n\t\t\t<string>background</string>\n\
         \t\t\t<key>NSIconName</key>\n\t\t\t<string>{icon}</string>\n\
         \t\t\t<key>NSMenuItem</key>\n\t\t\t<dict>\n\t\t\t\t<key>default</key>\n\t\t\t\t<string>{title}</string>\n\t\t\t</dict>\n\
         \t\t\t<key>NSMessage</key>\n\t\t\t<string>runWorkflowAsService</string>\n\
         \t\t\t<key>NSRequiredContext</key>\n\t\t\t<dict>\n\t\t\t\t<key>NSApplicationIdentifier</key>\n\t\t\t\t<string>com.apple.finder</string>\n\t\t\t</dict>\n\
         \t\t\t<key>NSSendFileTypes</key>\n\t\t\t<array>\n\t\t\t\t<string>public.item</string>\n\t\t\t</array>\n\
         \t\t</dict>\n\t</array>\n\
         </dict>\n</plist>\n",
        id = match action {
            QuickAction::Rename => "rename",
            QuickAction::NewFile => "newfile",
        },
        icon = match action {
            QuickAction::Rename => "NSActionTemplate",
            QuickAction::NewFile => "NSAddTemplate",
        },
    )
}

/// A deterministic UUID-shaped id (Automator only needs them unique within
/// the document).
fn uuid(action: QuickAction, n: u8) -> String {
    let a = match action {
        QuickAction::Rename => 0xA1,
        QuickAction::NewFile => 0xB2,
    };
    format!("6F0D{a:02X}{n:02X}-50CE-4D3A-9A1B-0000504D{a:02X}{n:02X}")
}

/// `Contents/document.wflow`: one "Run Shell Script" action (bash, input
/// as arguments) in a Finder service that receives files and folders.
pub fn document_wflow(action: QuickAction, exe: &Path) -> String {
    let script = xml(&action.script(exe));
    format!(
        "{PLIST_HEAD}<dict>\n\
\t<key>AMApplicationBuild</key>\n\t<string>523</string>\n\
\t<key>AMApplicationVersion</key>\n\t<string>2.10</string>\n\
\t<key>AMDocumentVersion</key>\n\t<string>2</string>\n\
\t<key>actions</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>action</key>\n\t\t\t<dict>\n\
\t\t\t\t<key>AMAccepts</key>\n\t\t\t\t<dict>\n\t\t\t\t\t<key>Container</key>\n\t\t\t\t\t<string>List</string>\n\t\t\t\t\t<key>Optional</key>\n\t\t\t\t\t<true/>\n\t\t\t\t\t<key>Types</key>\n\t\t\t\t\t<array>\n\t\t\t\t\t\t<string>com.apple.cocoa.string</string>\n\t\t\t\t\t</array>\n\t\t\t\t</dict>\n\
\t\t\t\t<key>AMActionVersion</key>\n\t\t\t\t<string>2.0.3</string>\n\
\t\t\t\t<key>AMApplication</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>Automator</string>\n\t\t\t\t</array>\n\
\t\t\t\t<key>AMParameterProperties</key>\n\t\t\t\t<dict>\n\t\t\t\t\t<key>COMMAND_STRING</key>\n\t\t\t\t\t<dict/>\n\t\t\t\t\t<key>CheckedForUserDefaultShell</key>\n\t\t\t\t\t<dict/>\n\t\t\t\t\t<key>inputMethod</key>\n\t\t\t\t\t<dict/>\n\t\t\t\t\t<key>shell</key>\n\t\t\t\t\t<dict/>\n\t\t\t\t\t<key>source</key>\n\t\t\t\t\t<dict/>\n\t\t\t\t</dict>\n\
\t\t\t\t<key>AMProvides</key>\n\t\t\t\t<dict>\n\t\t\t\t\t<key>Container</key>\n\t\t\t\t\t<string>List</string>\n\t\t\t\t\t<key>Types</key>\n\t\t\t\t\t<array>\n\t\t\t\t\t\t<string>com.apple.cocoa.string</string>\n\t\t\t\t\t</array>\n\t\t\t\t</dict>\n\
\t\t\t\t<key>ActionBundlePath</key>\n\t\t\t\t<string>/System/Library/Automator/Run Shell Script.action</string>\n\
\t\t\t\t<key>ActionName</key>\n\t\t\t\t<string>Run Shell Script</string>\n\
\t\t\t\t<key>ActionParameters</key>\n\t\t\t\t<dict>\n\
\t\t\t\t\t<key>COMMAND_STRING</key>\n\t\t\t\t\t<string>{script}</string>\n\
\t\t\t\t\t<key>CheckedForUserDefaultShell</key>\n\t\t\t\t\t<true/>\n\
\t\t\t\t\t<key>inputMethod</key>\n\t\t\t\t\t<integer>1</integer>\n\
\t\t\t\t\t<key>shell</key>\n\t\t\t\t\t<string>/bin/bash</string>\n\
\t\t\t\t\t<key>source</key>\n\t\t\t\t\t<string></string>\n\
\t\t\t\t</dict>\n\
\t\t\t\t<key>BundleIdentifier</key>\n\t\t\t\t<string>com.apple.RunShellScript</string>\n\
\t\t\t\t<key>CFBundleVersion</key>\n\t\t\t\t<string>2.0.3</string>\n\
\t\t\t\t<key>CanShowSelectedItemsWhenRun</key>\n\t\t\t\t<false/>\n\
\t\t\t\t<key>CanShowWhenRun</key>\n\t\t\t\t<true/>\n\
\t\t\t\t<key>Category</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>AMCategoryUtilities</string>\n\t\t\t\t</array>\n\
\t\t\t\t<key>Class Name</key>\n\t\t\t\t<string>RunShellScriptAction</string>\n\
\t\t\t\t<key>InputUUID</key>\n\t\t\t\t<string>{in_id}</string>\n\
\t\t\t\t<key>Keywords</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>Shell</string>\n\t\t\t\t\t<string>Script</string>\n\t\t\t\t\t<string>Command</string>\n\t\t\t\t\t<string>Run</string>\n\t\t\t\t\t<string>Unix</string>\n\t\t\t\t</array>\n\
\t\t\t\t<key>OutputUUID</key>\n\t\t\t\t<string>{out_id}</string>\n\
\t\t\t\t<key>UUID</key>\n\t\t\t\t<string>{act_id}</string>\n\
\t\t\t\t<key>UnlocalizedApplications</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>Automator</string>\n\t\t\t\t</array>\n\
\t\t\t\t<key>isViewVisible</key>\n\t\t\t\t<integer>1</integer>\n\
\t\t\t\t<key>location</key>\n\t\t\t\t<string>309.000000:253.000000</string>\n\
\t\t\t\t<key>nibPath</key>\n\t\t\t\t<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>\n\
\t\t\t</dict>\n\t\t\t<key>isViewVisible</key>\n\t\t\t<integer>1</integer>\n\t\t</dict>\n\t</array>\n\
\t<key>connectors</key>\n\t<dict/>\n\
\t<key>workflowMetaData</key>\n\t<dict>\n\
\t\t<key>applicationBundleID</key>\n\t\t<string>com.apple.finder</string>\n\
\t\t<key>applicationBundleIDsByPath</key>\n\t\t<dict>\n\t\t\t<key>/System/Library/CoreServices/Finder.app</key>\n\t\t\t<string>com.apple.finder</string>\n\t\t</dict>\n\
\t\t<key>applicationPath</key>\n\t\t<string>/System/Library/CoreServices/Finder.app</string>\n\
\t\t<key>applicationPaths</key>\n\t\t<array>\n\t\t\t<string>/System/Library/CoreServices/Finder.app</string>\n\t\t</array>\n\
\t\t<key>inputTypeIdentifier</key>\n\t\t<string>com.apple.Automator.fileSystemObject</string>\n\
\t\t<key>outputTypeIdentifier</key>\n\t\t<string>com.apple.Automator.nothing</string>\n\
\t\t<key>presentationMode</key>\n\t\t<integer>15</integer>\n\
\t\t<key>processesInput</key>\n\t\t<false/>\n\
\t\t<key>serviceApplicationBundleID</key>\n\t\t<string>com.apple.finder</string>\n\
\t\t<key>serviceApplicationPath</key>\n\t\t<string>/System/Library/CoreServices/Finder.app</string>\n\
\t\t<key>serviceInputTypeIdentifier</key>\n\t\t<string>com.apple.Automator.fileSystemObject</string>\n\
\t\t<key>serviceOutputTypeIdentifier</key>\n\t\t<string>com.apple.Automator.nothing</string>\n\
\t\t<key>serviceProcessesInput</key>\n\t\t<false/>\n\
\t\t<key>systemImageName</key>\n\t\t<string>{icon}</string>\n\
\t\t<key>useAutomaticInputType</key>\n\t\t<false/>\n\
\t\t<key>workflowTypeIdentifier</key>\n\t\t<string>com.apple.Automator.servicesMenu</string>\n\
\t</dict>\n\
</dict>\n</plist>\n",
        in_id = uuid(action, 1),
        out_id = uuid(action, 2),
        act_id = uuid(action, 3),
        icon = match action {
            QuickAction::Rename => "NSActionTemplate",
            QuickAction::NewFile => "NSAddTemplate",
        },
    )
}

/// `~/Library/Services` (macOS).
pub fn services_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library").join("Services"))
}

/// Write the Quick Action bundles into `dir` (replacing older versions).
/// Returns the bundle paths.
pub fn install_services_in(dir: &Path, exe: &Path) -> anyhow::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for a in QuickAction::ALL {
        let bundle = dir.join(a.bundle_name());
        let contents = bundle.join("Contents");
        if bundle.exists() {
            std::fs::remove_dir_all(&bundle)?;
        }
        std::fs::create_dir_all(&contents)?;
        std::fs::write(contents.join("Info.plist"), info_plist(a))?;
        std::fs::write(contents.join("document.wflow"), document_wflow(a, exe))?;
        out.push(bundle);
    }
    Ok(out)
}

/// Remove the Quick Action bundles from `dir` (only PowerMac's).
pub fn uninstall_services_in(dir: &Path) -> anyhow::Result<()> {
    for a in QuickAction::ALL {
        let bundle = dir.join(a.bundle_name());
        if bundle.exists() {
            std::fs::remove_dir_all(&bundle)?;
        }
    }
    Ok(())
}

/// Whether all Quick Action bundles are in `dir`.
pub fn services_installed_in(dir: &Path) -> bool {
    QuickAction::ALL.iter().all(|a| {
        dir.join(a.bundle_name())
            .join("Contents/document.wflow")
            .is_file()
    })
}

#[cfg(target_os = "macos")]
fn refresh_services() {
    // Make Finder pick up the change now rather than at next login.
    let _ = std::process::Command::new("/System/Library/CoreServices/pbs")
        .arg("-update")
        .status();
}

/// The `reg.exe` argument lists that add the Explorer verbs for `exe`.
pub fn windows_registry_commands(exe: &Path) -> Vec<Vec<String>> {
    let exe = exe.to_string_lossy().into_owned();
    let root = r"HKCU\Software\Classes";
    let mut cmds = Vec::new();
    let mut verb = |class: &str, verb: &str, title: &str, command: String| {
        let key = format!(r"{root}\{class}\shell\{verb}");
        let add = |key: &str, value: Option<&str>, data: &str| {
            let mut v = vec!["add".to_string(), key.to_string()];
            match value {
                Some(n) => v.extend(["/v".to_string(), n.to_string()]),
                None => v.push("/ve".to_string()),
            }
            v.extend([
                "/t".into(),
                "REG_SZ".into(),
                "/d".into(),
                data.to_string(),
                "/f".into(),
            ]);
            v
        };
        cmds.push(add(&key, None, title));
        cmds.push(add(&key, Some("Icon"), &exe));
        cmds.push(add(&format!(r"{key}\command"), None, &command));
    };
    let rename = format!("\"{exe}\" rename \"%1\"");
    verb(
        "*",
        "PowerMacRename",
        "Rename with PowerRename",
        rename.clone(),
    );
    verb(
        "Directory",
        "PowerMacRename",
        "Rename with PowerRename",
        rename,
    );
    verb(
        "Directory",
        "PowerMacNew",
        "New file here",
        format!("\"{exe}\" new --dir \"%1\""),
    );
    verb(
        r"Directory\Background",
        "PowerMacNew",
        "New file here",
        format!("\"{exe}\" new --dir \"%V\""),
    );
    cmds
}

/// The registry keys [`windows_registry_commands`] creates (top-level verb keys).
pub fn windows_registry_keys() -> Vec<String> {
    [
        r"*\shell\PowerMacRename",
        r"Directory\shell\PowerMacRename",
        r"Directory\shell\PowerMacNew",
        r"Directory\Background\shell\PowerMacNew",
    ]
    .iter()
    .map(|k| format!(r"HKCU\Software\Classes\{k}"))
    .collect()
}

#[cfg(windows)]
fn reg(args: &[String]) -> anyhow::Result<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let st = std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(st.success())
}

/// Install the Finder Quick Actions (macOS) / Explorer context-menu verbs
/// (Windows) that run `exe`. Returns what was written.
pub fn install_finder_integration(exe: &Path) -> anyhow::Result<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        let dir = services_dir().ok_or_else(|| anyhow::anyhow!("no home folder"))?;
        let v = install_services_in(&dir, exe)?;
        refresh_services();
        Ok(v)
    }
    #[cfg(windows)]
    {
        for args in windows_registry_commands(exe) {
            anyhow::ensure!(reg(&args)?, "reg {} failed", args.join(" "));
        }
        Ok(windows_registry_keys()
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = exe;
        anyhow::bail!("file-manager integration isn't available on this platform")
    }
}

/// Remove what [`install_finder_integration`] added.
pub fn uninstall_finder_integration() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let dir = services_dir().ok_or_else(|| anyhow::anyhow!("no home folder"))?;
        uninstall_services_in(&dir)?;
        refresh_services();
        Ok(())
    }
    #[cfg(windows)]
    {
        for k in windows_registry_keys() {
            let _ = reg(&["delete".into(), k, "/f".into()]);
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    Ok(())
}

/// Whether the integration is installed.
pub fn finder_integration_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        services_dir().is_some_and(|d| services_installed_in(&d))
    }
    #[cfg(windows)]
    {
        windows_registry_keys()
            .into_iter()
            .all(|k| reg(&["query".into(), k]).unwrap_or(false))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
        assert!(QuickAction::Rename
            .script(Path::new("/Apps/Power Mac.app/Contents/MacOS/powermac"))
            .contains("'/Apps/Power Mac.app/Contents/MacOS/powermac' rename \"$@\""));
        assert!(QuickAction::NewFile
            .script(Path::new("/x/powermac"))
            .contains("'/x/powermac' new --dir \"$d\""));
    }

    #[test]
    fn bundles_in_temp_dir() {
        let d = tempfile::tempdir().unwrap();
        let svc = d.path().join("Services");
        assert!(!services_installed_in(&svc));
        let exe = Path::new("/Applications/PowerMac & Co <beta>.app/Contents/MacOS/powermac");
        let v = install_services_in(&svc, exe).unwrap();
        assert_eq!(v.len(), 2);
        assert!(services_installed_in(&svc));
        // Re-install replaces cleanly.
        install_services_in(&svc, exe).unwrap();
        for b in &v {
            let wf = std::fs::read_to_string(b.join("Contents/document.wflow")).unwrap();
            assert!(wf.contains("PowerMac &amp; Co &lt;beta&gt;.app"));
            assert!(wf.contains("com.apple.Automator.servicesMenu"));
            let info = std::fs::read_to_string(b.join("Contents/Info.plist")).unwrap();
            assert!(info.contains("runWorkflowAsService"));
            #[cfg(target_os = "macos")]
            for f in ["Info.plist", "document.wflow"] {
                let out = std::process::Command::new("/usr/bin/plutil")
                    .arg("-lint")
                    .arg(b.join("Contents").join(f))
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{f}: {}",
                    String::from_utf8_lossy(&out.stdout)
                );
                // The command survives the plist round trip unchanged.
                if f == "document.wflow" {
                    let out = std::process::Command::new("/usr/bin/plutil")
                        .args([
                            "-extract",
                            "actions.0.action.ActionParameters.COMMAND_STRING",
                            "raw",
                            "-o",
                            "-",
                        ])
                        .arg(b.join("Contents").join(f))
                        .output()
                        .unwrap();
                    let s = String::from_utf8_lossy(&out.stdout);
                    let a = if b.to_string_lossy().contains("Rename") {
                        QuickAction::Rename
                    } else {
                        QuickAction::NewFile
                    };
                    assert_eq!(s.trim_end(), a.script(exe).trim_end());
                }
            }
        }
        // Unrelated bundles are left alone.
        std::fs::create_dir_all(svc.join("Other.workflow")).unwrap();
        uninstall_services_in(&svc).unwrap();
        assert!(!services_installed_in(&svc));
        assert!(svc.join("Other.workflow").exists());
        assert!(!v[0].exists() && !v[1].exists());
    }

    #[test]
    fn scripts_run_with_arguments() {
        // Run the generated shell scripts the way Automator does (bash, the
        // items as arguments) against a stand-in executable.
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("args.txt");
        let exe = d.path().join("fake powermac");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let wait = || {
            for _ in 0..100 {
                if let Ok(s) = std::fs::read_to_string(&log) {
                    if !s.is_empty() {
                        let _ = std::fs::remove_file(&log);
                        return s;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            panic!("script didn't run");
        };
        let file = d.path().join("a b.txt");
        std::fs::write(&file, "").unwrap();
        let run = |a: QuickAction, args: &[&Path]| {
            let st = std::process::Command::new("/bin/bash")
                .arg("-c")
                .arg(a.script(&exe))
                .arg("bash")
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        };
        run(QuickAction::Rename, &[&file, d.path()]);
        assert_eq!(
            wait(),
            format!("rename\n{}\n{}\n", file.display(), d.path().display())
        );
        run(QuickAction::NewFile, &[&file]);
        assert_eq!(wait(), format!("new\n--dir\n{}\n", d.path().display()));
        run(QuickAction::NewFile, &[d.path()]);
        assert_eq!(wait(), format!("new\n--dir\n{}\n", d.path().display()));
    }

    /// Runs the generated workflows with `/usr/bin/automator` (the real
    /// Automator runtime) from a temp folder. Slow, so opt-in:
    /// `cargo test -p file-tools automator -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn automator_runs_workflows() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("args.txt");
        let exe = d.path().join("fake powermac");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
                log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let svc = d.path().join("Services");
        let bundles = install_services_in(&svc, &exe).unwrap();
        let file = d.path().join("x y.txt");
        std::fs::write(&file, "").unwrap();
        for (b, want) in bundles.iter().zip([
            format!("rename\n{}\n", file.display()),
            format!("new\n--dir\n{}\n", d.path().display()),
        ]) {
            let out = std::process::Command::new("/usr/bin/automator")
                .arg("-i")
                .arg(&file)
                .arg(b)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let mut got = String::new();
            for _ in 0..200 {
                if let Ok(s) = std::fs::read_to_string(&log) {
                    got = s;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let _ = std::fs::remove_file(&log);
            // automator hands over /private/var/... for /var/...
            let norm = |s: &str| s.replace("/private/var/", "/var/");
            assert_eq!(norm(&got), norm(&want));
        }
    }

    #[test]
    fn registry_commands() {
        let c = windows_registry_commands(Path::new(r"C:\Program Files\PowerMac\powermac.exe"));
        assert_eq!(c.len(), 12);
        let flat: Vec<String> = c.iter().map(|v| v.join(" ")).collect();
        assert!(flat.contains(
            &r#"add HKCU\Software\Classes\*\shell\PowerMacRename\command /ve /t REG_SZ /d "C:\Program Files\PowerMac\powermac.exe" rename "%1" /f"#.to_string()
        ));
        assert!(flat.iter().any(
            |s| s.contains(r"Directory\Background\shell\PowerMacNew\command") && s.contains("%V")
        ));
        assert_eq!(windows_registry_keys().len(), 4);
    }
}
