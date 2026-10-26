use std::path::{Path, PathBuf};

use super::apps::{self, open_command_for, AppKind};
use super::paths::{format_path_with, percent_encode_path, CopyStyle};
use super::volumes::{self, *};
use super::*;

#[test]
fn copy_path_styles() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let file = home.join("Docs/Trip notes (final) é.txt");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "").unwrap();
    let h = Some(home.as_path());
    let f = |s| format_path_with(&file, s, h, false);
    assert_eq!(f(CopyStyle::Posix), file.display().to_string());
    assert_eq!(f(CopyStyle::Tilde), "~/Docs/Trip notes (final) é.txt");
    assert_eq!(f(CopyStyle::Name), "Trip notes (final) é.txt");
    assert_eq!(
        f(CopyStyle::FileUrl),
        format!(
            "file://{}/Docs/Trip%20notes%20(final)%20%C3%A9.txt",
            percent_encode_path(&home.to_string_lossy())
        )
    );
    assert_eq!(f(CopyStyle::Quoted), format!("'{}'", file.display()));
    // Folders: URL with a trailing slash; home itself is "~".
    let dir = home.join("Docs");
    assert!(format_path_with(&dir, CopyStyle::FileUrl, h, true).ends_with("/Docs/"));
    assert_eq!(format_path_with(&home, CopyStyle::Tilde, h, true), "~");
    // Outside home: the full path.
    assert_eq!(
        format_path_with(Path::new("/etc/hosts"), CopyStyle::Tilde, h, false),
        "/etc/hosts"
    );
    // Plain paths aren't quoted; quotes inside are escaped.
    assert_eq!(
        format_path_with(Path::new("/usr/bin"), CopyStyle::Quoted, h, true),
        "/usr/bin"
    );
    assert_eq!(
        format_path_with(Path::new("/tmp/it's"), CopyStyle::Quoted, h, false),
        r"'/tmp/it'\''s'"
    );
    assert_eq!(percent_encode_path("/a b/#?%"), "/a%20b/%23%3F%25");
    // Several items: one per line, folder URL detection from the disk.
    let s = format_paths(&[file.clone(), dir.clone()], CopyStyle::FileUrl);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].ends_with("/Docs/"));
    assert_eq!(
        format_paths(&[file.clone(), dir], CopyStyle::Name),
        "Trip notes (final) é.txt\nDocs"
    );
    for s in CopyStyle::ALL {
        assert_eq!(CopyStyle::parse(s.cli_name()), Some(s));
    }
    assert_eq!(CopyStyle::parse("~"), Some(CopyStyle::Tilde));
    assert_eq!(CopyStyle::parse("nope"), None);
}

#[test]
fn terminal_and_editor_commands() {
    let dir = Path::new("/Users/me/My Project");
    let mac = |app: &str| open_command_for(app, dir, false).join(" | ");
    assert_eq!(
        mac("com.apple.Terminal"),
        "/usr/bin/open | -b | com.apple.Terminal | /Users/me/My Project"
    );
    assert_eq!(
        mac("com.googlecode.iterm2"),
        "/usr/bin/open | -b | com.googlecode.iterm2 | /Users/me/My Project"
    );
    assert_eq!(
        mac("dev.warp.Warp-Stable"),
        "/usr/bin/open | -b | dev.warp.Warp-Stable | /Users/me/My Project"
    );
    assert_eq!(
        mac("com.mitchellh.ghostty"),
        "/usr/bin/open | -b | com.mitchellh.ghostty | /Users/me/My Project"
    );
    assert_eq!(
        mac("org.alacritty"),
        "/usr/bin/open | -n | -b | org.alacritty | --args | --working-directory | /Users/me/My Project"
    );
    assert_eq!(
        mac("com.github.wez.wezterm"),
        "/usr/bin/open | -n | -b | com.github.wez.wezterm | --args | start | --cwd | /Users/me/My Project"
    );
    assert_eq!(
        mac("com.microsoft.VSCode"),
        "/usr/bin/open | -b | com.microsoft.VSCode | /Users/me/My Project"
    );
    assert_eq!(
        mac("dev.zed.Zed"),
        "/usr/bin/open | -b | dev.zed.Zed | /Users/me/My Project"
    );
    // Unknown bundle ids and app paths.
    assert_eq!(
        mac("com.example.Thing"),
        "/usr/bin/open | -b | com.example.Thing | /Users/me/My Project"
    );
    assert_eq!(
        mac("/Applications/Foo Bar.app"),
        "/usr/bin/open | -a | /Applications/Foo Bar.app | /Users/me/My Project"
    );
    // Windows.
    let wdir = Path::new(r"C:\Users\me\My Project");
    let win = |app: &str| open_command_for(app, wdir, true);
    assert_eq!(win("wt"), vec!["wt.exe", "-d", r"C:\Users\me\My Project"]);
    assert_eq!(
        win("code"),
        vec!["cmd.exe", "/C", "code", r"C:\Users\me\My Project"]
    );
    assert_eq!(win("cmd")[5], r"C:\Users\me\My Project");
    assert_eq!(
        win(r"C:\Tools\ed.exe"),
        vec![r"C:\Tools\ed.exe", r"C:\Users\me\My Project"]
    );
    // Names.
    assert_eq!(app_name("com.todesktop.230313mt0aa7e5"), "Cursor");
    assert_eq!(app_name("/Applications/Nova.app"), "Nova");
    assert_eq!(app_name("org.unknown"), "org.unknown");
    // Configured wins; otherwise something sensible.
    assert_eq!(
        resolve_app(AppKind::Terminal, " dev.warp.Warp-Stable "),
        "dev.warp.Warp-Stable"
    );
    assert!(!resolve_app(AppKind::Terminal, "").is_empty());
    assert!(!resolve_app(AppKind::Editor, "").is_empty());
    // Every macOS terminal / editor resolves to some command.
    for a in apps::MAC_APPS {
        let c = open_command_for(a.id, dir, false);
        assert_eq!(c[0], "/usr/bin/open");
        assert_eq!(c.last().unwrap(), "/Users/me/My Project");
    }
}

#[test]
fn folders_for_selection() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    std::fs::write(&f, "").unwrap();
    let sub = d.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(apps::folder_of(&f), d.path());
    assert_eq!(apps::folder_of(&sub), sub);
    assert_eq!(
        apps::folders_of(&[f.clone(), sub.clone(), d.path().join("b"), f], 5),
        vec![d.path().to_path_buf(), sub.clone()]
    );
    assert_eq!(
        apps::folders_of(&[sub.clone(), d.path().to_path_buf()], 1),
        vec![sub]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn installed_apps_are_found() {
    // Terminal ships with macOS.
    let t = installed_apps(AppKind::Terminal);
    assert!(t.iter().any(|a| a.id == "com.apple.Terminal"), "{t:?}");
    assert!(t[0]
        .path
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .ends_with("Terminal.app"));
}

#[test]
fn hidden_files_commands_only() {
    // Never run these in tests: they restart Finder.
    let c = hidden_files_commands(true);
    if cfg!(target_os = "macos") {
        assert_eq!(
            c[0].join(" "),
            "/usr/bin/defaults write com.apple.finder AppleShowAllFiles -bool YES"
        );
        assert_eq!(c[1].join(" "), "/usr/bin/killall Finder");
        assert!(hidden_files_commands(false)[0]
            .join(" ")
            .ends_with("-bool NO"));
    }
    for (s, v) in [
        ("1\n", true),
        ("YES", true),
        ("true", true),
        ("0", false),
        ("NO\n", false),
        ("", false),
    ] {
        assert_eq!(parse_defaults_bool(s), v, "{s:?}");
    }
    assert!(HIDDEN_FILES_NOTE.contains("restarts"));
}

#[cfg(target_os = "macos")]
#[test]
fn hidden_files_state_reads_without_side_effects() {
    // Only reads `defaults`; whatever the answer, it's a bool.
    let _ = hidden_files_shown();
}

fn vol(name: &str) -> VolumeInfo {
    VolumeInfo {
        name: name.into(),
        mount_point: PathBuf::from(format!("/Volumes/{name}")),
        device: "disk4s1".into(),
        whole_disk: "disk4".into(),
        fs_type: "msdos".into(),
        internal: false,
        removable: true,
        ejectable: true,
        ..Default::default()
    }
}

#[test]
fn volume_filtering() {
    let f = EjectFilter::default();
    let usb = vol("USB");
    assert!(is_ejectable_external(&usb, f));
    let boot = VolumeInfo {
        root: true,
        internal: true,
        ..vol("Macintosh HD")
    };
    assert!(!is_ejectable_external(&boot, f));
    let data = VolumeInfo {
        system: true,
        internal: true,
        ..vol("Data")
    };
    assert!(!is_ejectable_external(&data, f));
    // Internal partition (Boot Camp): no.
    let bc = VolumeInfo {
        internal: true,
        removable: false,
        ejectable: false,
        ..vol("BOOTCAMP")
    };
    assert!(!is_ejectable_external(&bc, f));
    // Built-in SD card reader: internal but removable + ejectable.
    let sd = VolumeInfo {
        internal: true,
        ..vol("SD")
    };
    assert!(is_ejectable_external(&sd, f));
    // External fixed disk (not "removable" but external): yes.
    let ssd = VolumeInfo {
        removable: false,
        ejectable: true,
        ..vol("T7")
    };
    assert!(is_ejectable_external(&ssd, f));
    // Disk images and network shares follow the filter.
    let dmg = VolumeInfo {
        disk_image: true,
        ..vol("Installer")
    };
    assert!(is_ejectable_external(&dmg, f));
    assert!(!is_ejectable_external(
        &dmg,
        EjectFilter {
            disk_images: false,
            network: false
        }
    ));
    let share = VolumeInfo {
        network: true,
        whole_disk: String::new(),
        device: "//me@nas/share".into(),
        ..vol("share")
    };
    assert!(!is_ejectable_external(&share, f));
    assert!(is_ejectable_external(
        &share,
        EjectFilter {
            disk_images: true,
            network: true
        }
    ));

    // Commands: whole disk for local volumes, unmount for shares.
    assert_eq!(
        eject_command(&usb).join(" "),
        "/usr/sbin/diskutil eject disk4"
    );
    assert_eq!(
        eject_command(&share).join(" "),
        "/usr/sbin/diskutil unmount /Volumes/share"
    );
    // Two partitions of one disk are ejected once.
    let p2 = VolumeInfo {
        device: "disk4s2".into(),
        ..vol("USB2")
    };
    let other = VolumeInfo {
        device: "disk6s1".into(),
        whole_disk: "disk6".into(),
        ..vol("Card")
    };
    let g = eject_groups(vec![usb.clone(), other.clone(), p2.clone(), share.clone()]);
    assert_eq!(g.len(), 3);
    assert_eq!(g[0], vec![usb, p2]);
    assert_eq!(g[1], vec![other]);

    // diskutil plist (as JSON) → flags.
    let mut v = VolumeInfo::default();
    let j: serde_json::Value = serde_json::from_str(
        r#"{"VolumeName":"Untitled","Internal":false,"RemovableMedia":true,"Ejectable":true,
            "BusProtocol":"USB","ParentWholeDisk":"disk7","TotalSize":31914983424}"#,
    )
    .unwrap();
    apply_diskutil_json(&mut v, &j);
    assert_eq!(
        (v.name.as_str(), v.whole_disk.as_str()),
        ("Untitled", "disk7")
    );
    assert!(!v.internal && v.removable && v.ejectable && !v.disk_image);
    assert_eq!(v.size, 31914983424);
    let j: serde_json::Value =
        serde_json::from_str(r#"{"Internal":false,"Ejectable":true,"BusProtocol":"Disk Image"}"#)
            .unwrap();
    apply_diskutil_json(&mut v, &j);
    assert!(v.disk_image);
    assert_eq!(volumes::human_size(31914983424), "31.9 GB");
    assert!(is_network_fs("smbfs") && !is_network_fs("apfs"));
}

#[test]
fn process_parsing() {
    let out = "p123\ncPreview\nf4\np123\ncPreview\np456\ncmdworker_shared\n";
    let p = volumes::parse_lsof(out);
    assert_eq!(p.len(), 2);
    assert_eq!((p[0].pid, p[0].name.as_str()), (123, "Preview"));
    assert_eq!((p[1].pid, p[1].name.as_str()), (456, "mdworker_shared"));
    let msg = "Volume USB on disk4s1 failed to unmount: dissented by PID 789 (/System/Applications/Preview.app/Contents/MacOS/Preview)\nDissenter parent PPID 1 (launchd)";
    assert_eq!(
        volumes::parse_dissenter(msg),
        Some((789, Some("Preview".into())))
    );
    assert_eq!(volumes::parse_dissenter("Unmount failed for disk4"), None);
    let d = tempfile::tempdir().unwrap();
    let a = volumes::lsof_args(d.path(), false);
    assert!(a.contains(&"+D".to_string()));
    let a = volumes::lsof_args(Path::new("/Volumes/USB"), true);
    assert_eq!(a[a.len() - 3..].join(" "), "+f -- /Volumes/USB");
    let rep = EjectReport {
        ejected: vec![vol("A")],
        failed: vec![EjectFailure {
            volume: vol("B"),
            error: "busy".into(),
            blockers: vec![ProcessInfo {
                pid: 1,
                name: "Preview".into(),
                bundle_id: None,
            }],
        }],
    };
    assert_eq!(
        rep.summary(),
        "Ejected 1 volume; B couldn't be ejected (in use by Preview)"
    );
    assert_eq!(
        EjectReport::default().summary(),
        "No external volumes to eject"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn list_real_volumes_and_lsof() {
    // Listing only; nothing is ejected.
    let all = volumes::all_volumes();
    assert!(all.iter().any(|v| v.root), "{all:#?}");
    for v in external_volumes() {
        assert!(!v.root && !v.system);
    }
    // A file this test holds open shows up with our pid excluded… so check
    // with a child process holding it.
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("held.txt");
    std::fs::write(&f, "x").unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("exec 3<'{}'; sleep 5", f.display()))
        .spawn()
        .unwrap();
    let mut found = false;
    for _ in 0..20 {
        let who = who_is_using(&f);
        if who.iter().any(|p| p.pid == child.id()) {
            found = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(found, "lsof didn't see the child");
}

#[test]
fn cli_parsing_and_dry_runs() {
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let c = parse_cli(&s(&["copy-path", "--style", "url", "/a", "/b"])).unwrap();
    assert_eq!(c.action, FinderAction::CopyPath);
    assert_eq!(c.style, Some(CopyStyle::FileUrl));
    assert_eq!(c.paths.len(), 2);
    let c = parse_cli(&s(&[
        "open-editor",
        "--app",
        "dev.zed.Zed",
        "--front-window",
    ]))
    .unwrap();
    assert_eq!(c.app.as_deref(), Some("dev.zed.Zed"));
    assert!(c.front_window);
    let c = parse_cli(&s(&["eject-all", "--dry-run", "--no-images"])).unwrap();
    assert!(c.dry_run);
    assert_eq!(
        c.filter,
        Some(EjectFilter {
            disk_images: false,
            network: false
        })
    );
    assert!(parse_cli(&s(&["frobnicate"])).is_err());
    assert!(parse_cli(&s(&[])).is_err());
    assert!(parse_cli(&s(&["copy-path", "--style", "weird"])).is_err());
    // Without paths and without the front-window opt-in: a helpful error,
    // and no AppleScript is run.
    let cfg = crate::config::FinderConfig::default();
    let e = run_cli_with(&s(&["open-terminal"]), &cfg)
        .unwrap_err()
        .to_string();
    assert!(e.contains("Automation"), "{e}");
    // Dry-run eject only lists.
    let _ = run_cli_with(&s(&["eject-all", "--dry-run"]), &cfg).unwrap();
    let _ = run_cli_with(&s(&["volumes"]), &cfg).unwrap();
    let out = run_cli_with(&s(&["apps"]), &cfg).unwrap();
    assert!(out.contains("terminal:") && out.contains("editor:"));
    assert!(FRONT_FOLDER_SCRIPT.contains("POSIX path of (target of front Finder window as alias)"));
}

/// Opens Terminal on the scratch folder once (opt-in; it opens a window):
/// `PM_OPEN_TERMINAL_TEST=<dir> cargo test -p file-tools open_terminal_for_real -- --ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn open_terminal_for_real() {
    let Some(d) = std::env::var_os("PM_OPEN_TERMINAL_TEST") else {
        return;
    };
    open_in_terminal_with(Path::new(&d), "com.apple.Terminal").unwrap();
}

/// Writes the clipboard (opt-in; replaces what you copied):
/// `PM_CLIPBOARD_TEST=1 cargo test -p file-tools clipboard_for_real -- --ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn clipboard_for_real() {
    if std::env::var_os("PM_CLIPBOARD_TEST").is_none() {
        return;
    }
    let t = copy_paths(
        &[PathBuf::from("/tmp/a b"), PathBuf::from("/usr")],
        CopyStyle::Posix,
    )
    .unwrap();
    let out = std::process::Command::new("/usr/bin/pbpaste")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), t);
}

/// Creates, mounts and ejects a 4 MB disk image of its own (never touches
/// other volumes), first with a process holding a file open on it to check
/// the blocker report. Opt-in, it shows briefly in Finder's sidebar:
/// `PM_DMG_TEST=1 cargo test -p file-tools eject_own_disk_image -- --ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn eject_own_disk_image() {
    if std::env::var_os("PM_DMG_TEST").is_none() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let img = d.path().join("pm-eject-test.dmg");
    let name = format!("PMEjectTest{}", std::process::id());
    let st = std::process::Command::new("/usr/bin/hdiutil")
        .args([
            "create", "-quiet", "-size", "4m", "-fs", "HFS+", "-volname", &name,
        ])
        .arg(&img)
        .status()
        .unwrap();
    assert!(st.success());
    let st = std::process::Command::new("/usr/bin/hdiutil")
        .args(["attach", "-quiet"])
        .arg(&img)
        .status()
        .unwrap();
    assert!(st.success());
    // Detach whatever happens.
    struct Detach(String);
    impl Drop for Detach {
        fn drop(&mut self) {
            let m = format!("/Volumes/{}", self.0);
            if Path::new(&m).exists() {
                let _ = std::process::Command::new("/usr/sbin/diskutil")
                    .args(["eject", &m])
                    .output();
            }
        }
    }
    let _guard = Detach(name.clone());
    let find = || {
        external_volumes_with(EjectFilter {
            disk_images: true,
            network: false,
        })
        .into_iter()
        .find(|v| v.name == name)
    };
    let v = find().expect("the test image is listed as ejectable");
    assert!(v.disk_image, "{v:?}");
    assert!(
        !external_volumes_with(EjectFilter {
            disk_images: false,
            network: false
        })
        .iter()
        .any(|x| x.name == name),
        "disk images can be excluded"
    );
    // Hold a file open on it: who_is_using sees the holder.
    let held = v.mount_point.join("held.txt");
    std::fs::write(&held, "x").unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("exec 3<'{}'; exec sleep 30", held.display()))
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let who = who_is_using(&v.mount_point);
    eprintln!("using {}: {who:?}", v.mount_point.display());
    assert!(who.iter().any(|p| p.pid == child.id()), "{who:?}");
    let busy = eject_volume(&v);
    eprintln!("eject while busy: {busy:?}");
    let _ = child.kill();
    let _ = child.wait();
    if busy.is_err() {
        // Now it's free.
        std::thread::sleep(std::time::Duration::from_millis(300));
        eject_volume(&v).unwrap();
    }
    assert!(find().is_none(), "ejected");
}
