//! External volumes, "Eject all", and which processes are using a file or
//! volume.
//!
//! **macOS:** mounted file systems come from `getfsstat`; for local disks
//! `diskutil info -plist` tells internal / removable / ejectable / disk
//! image. Ejecting runs `diskutil eject <whole disk>` (or `diskutil
//! unmount` for network shares); when it fails, the dissenting process
//! from diskutil's message and the processes `lsof` sees on that volume are
//! reported.
//!
//! **Windows:** removable / CD drives from `GetLogicalDrives`; ejecting
//! locks, dismounts and ejects the volume (`IOCTL_STORAGE_EJECT_MEDIA`);
//! "who is using" asks the Restart Manager about files.

use std::path::{Path, PathBuf};

/// A mounted volume.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VolumeInfo {
    pub name: String,
    pub mount_point: PathBuf,
    /// `disk4s1` (macOS), `E:` (Windows), or the server path of a share.
    pub device: String,
    /// The whole disk (`disk4`); ejecting it ejects all of its volumes.
    pub whole_disk: String,
    pub fs_type: String,
    pub internal: bool,
    pub removable: bool,
    pub ejectable: bool,
    pub disk_image: bool,
    pub network: bool,
    /// The boot volume.
    pub root: bool,
    /// Hidden system mounts (`/System/Volumes/…`, `/dev`).
    pub system: bool,
    pub size: u64,
}

/// Which volumes "Eject all" takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EjectFilter {
    pub disk_images: bool,
    pub network: bool,
}

impl Default for EjectFilter {
    fn default() -> Self {
        Self {
            disk_images: true,
            network: false,
        }
    }
}

/// Whether "Eject all" should eject `v`.
pub fn is_ejectable_external(v: &VolumeInfo, f: EjectFilter) -> bool {
    if v.root || v.system {
        return false;
    }
    if v.network {
        return f.network;
    }
    if v.disk_image {
        return f.disk_images;
    }
    !v.internal || (v.removable && v.ejectable)
}

/// A process holding a file or volume open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub bundle_id: Option<String>,
}

/// The result of "Eject all".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EjectReport {
    pub ejected: Vec<VolumeInfo>,
    pub failed: Vec<EjectFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EjectFailure {
    pub volume: VolumeInfo,
    pub error: String,
    /// Processes using the volume (best effort).
    pub blockers: Vec<ProcessInfo>,
}

impl EjectReport {
    /// One line for a notification: "Ejected 2 volumes" / "… ; USB couldn't
    /// be ejected (in use by Preview)".
    pub fn summary(&self) -> String {
        let n = self.ejected.len();
        let mut s = if n == 0 && self.failed.is_empty() {
            "No external volumes to eject".to_string()
        } else {
            format!("Ejected {n} volume{}", if n == 1 { "" } else { "s" })
        };
        for f in &self.failed {
            let who: Vec<&str> = f.blockers.iter().map(|p| p.name.as_str()).collect();
            if who.is_empty() {
                s.push_str(&format!(
                    "; {} couldn't be ejected ({})",
                    f.volume.name, f.error
                ));
            } else {
                s.push_str(&format!(
                    "; {} couldn't be ejected (in use by {})",
                    f.volume.name,
                    who.join(", ")
                ));
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers (platform independent, unit tested)

/// `lsof -F pc` output → processes (deduplicated by pid).
pub fn parse_lsof(out: &str) -> Vec<ProcessInfo> {
    let mut v: Vec<ProcessInfo> = Vec::new();
    let mut pid: Option<u32> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let Some(c) = line.strip_prefix('c') {
            if let Some(p) = pid {
                if !v.iter().any(|x| x.pid == p) {
                    v.push(ProcessInfo {
                        pid: p,
                        name: c.to_string(),
                        bundle_id: None,
                    });
                }
            }
        }
    }
    v
}

/// The pid in diskutil's "… dissented by PID 1234 (/path/to/proc)".
pub fn parse_dissenter(msg: &str) -> Option<(u32, Option<String>)> {
    let i = msg.find("PID ")?;
    let rest = &msg[i + 4..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let pid = digits.parse().ok()?;
    let after = rest[digits.len()..].trim_start();
    let name = after.strip_prefix('(').and_then(|s| {
        let end = s.find(')')?;
        let p = &s[..end];
        Some(
            Path::new(p)
                .file_name()
                .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned()),
        )
    });
    Some((pid, name))
}

/// The `lsof` arguments listing processes using `path`: the whole file
/// system for a mount point, the folder's tree for a folder, else the file.
pub fn lsof_args(path: &Path, mount_point: bool) -> Vec<String> {
    let p = path.to_string_lossy().into_owned();
    let mut v: Vec<String> = ["-w", "-n", "-P", "+c", "0", "-F", "pc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if mount_point {
        v.extend(["+f".into(), "--".into(), p]);
    } else if path.is_dir() {
        v.extend(["+D".into(), p]);
    } else {
        v.extend(["--".into(), p]);
    }
    v
}

/// The command that ejects (or unmounts) `v` (macOS).
pub fn eject_command(v: &VolumeInfo) -> Vec<String> {
    let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    if v.network || v.whole_disk.is_empty() {
        let mut c = s(&["/usr/sbin/diskutil", "unmount"]);
        c.push(v.mount_point.to_string_lossy().into_owned());
        c
    } else {
        let mut c = s(&["/usr/sbin/diskutil", "eject"]);
        c.push(v.whole_disk.clone());
        c
    }
}

/// Network file systems.
pub fn is_network_fs(fs: &str) -> bool {
    matches!(
        fs,
        "smbfs" | "afpfs" | "nfs" | "webdav" | "ftp" | "cifs" | "macfuse_smb"
    )
}

/// Fill a [`VolumeInfo`] from `diskutil info -plist` converted to JSON.
pub fn apply_diskutil_json(v: &mut VolumeInfo, j: &serde_json::Value) {
    let b = |k: &str| j.get(k).and_then(serde_json::Value::as_bool);
    let s = |k: &str| {
        j.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    if let Some(n) = s("VolumeName").filter(|n| !n.is_empty()) {
        v.name = n;
    }
    if let Some(x) = b("Internal") {
        v.internal = x;
    }
    v.removable = b("Removable")
        .or(b("RemovableMedia"))
        .unwrap_or(v.removable)
        || b("RemovableMediaOrExternalDevice").unwrap_or(false) && !v.internal;
    v.ejectable = b("Ejectable").unwrap_or(v.ejectable);
    let bus = s("BusProtocol").unwrap_or_default();
    v.disk_image = bus == "Disk Image" || s("DeviceModel").is_some_and(|m| m == "Disk Image");
    if let Some(w) = s("ParentWholeDisk") {
        v.whole_disk = w;
    }
    if let Some(sz) = j.get("TotalSize").and_then(serde_json::Value::as_u64) {
        v.size = sz;
    }
}

// ---------------------------------------------------------------------------
// macOS

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::ffi::CStr;

    /// All mounted file systems (getfsstat), without diskutil details.
    pub fn mounts() -> Vec<VolumeInfo> {
        let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if n <= 0 {
            return Vec::new();
        }
        let mut buf: Vec<libc::statfs> = Vec::with_capacity(n as usize + 8);
        let size = (buf.capacity() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if n <= 0 {
            return Vec::new();
        }
        unsafe { buf.set_len(n as usize) };
        buf.iter()
            .map(|s| {
                let c = |a: &[libc::c_char]| {
                    unsafe { CStr::from_ptr(a.as_ptr()) }
                        .to_string_lossy()
                        .into_owned()
                };
                let mount = PathBuf::from(c(&s.f_mntonname));
                let from = c(&s.f_mntfromname);
                let fs = c(&s.f_fstypename);
                let flags = s.f_flags;
                let root = flags & libc::MNT_ROOTFS as u32 != 0 || mount == Path::new("/");
                let dontbrowse = flags & libc::MNT_DONTBROWSE as u32 != 0;
                let network = is_network_fs(&fs)
                    || flags & libc::MNT_LOCAL as u32 == 0 && fs != "devfs" && fs != "autofs";
                let device = from.strip_prefix("/dev/").unwrap_or(&from).to_string();
                let system = dontbrowse
                    || mount.starts_with("/System/Volumes")
                    || matches!(fs.as_str(), "devfs" | "autofs" | "nullfs")
                    || mount.starts_with("/private/var/folders") && !mount.starts_with("/Volumes");
                VolumeInfo {
                    name: mount
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".into()),
                    mount_point: mount,
                    whole_disk: if network {
                        String::new()
                    } else {
                        whole_disk_of(&device)
                    },
                    device,
                    fs_type: fs,
                    internal: !network,
                    removable: false,
                    ejectable: false,
                    disk_image: false,
                    network,
                    root,
                    system,
                    size: s.f_blocks * u64::from(s.f_bsize),
                }
            })
            .collect()
    }

    /// `disk4s1` → `disk4`.
    pub fn whole_disk_of(dev: &str) -> String {
        if !dev.starts_with("disk") {
            return String::new();
        }
        let rest = &dev[4..];
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        format!("disk{n}")
    }

    fn diskutil_info(dev: &str) -> Option<serde_json::Value> {
        let out = std::process::Command::new("/usr/sbin/diskutil")
            .args(["info", "-plist", dev])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut child = std::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-r", "-o", "-", "--", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take()?;
            // diskutil plists can hold <data>, which JSON can't; strip it.
            let xml = String::from_utf8_lossy(&out.stdout);
            let mut cleaned = String::with_capacity(xml.len());
            let mut rest = xml.as_ref();
            while let Some(i) = rest.find("<data>") {
                cleaned.push_str(&rest[..i]);
                cleaned.push_str("<string></string>");
                match rest[i..].find("</data>") {
                    Some(j) => rest = &rest[i + j + 7..],
                    None => {
                        rest = "";
                    }
                }
            }
            cleaned.push_str(rest);
            stdin.write_all(cleaned.as_bytes()).ok()?;
        }
        let out = child.wait_with_output().ok()?;
        serde_json::from_slice(&out.stdout).ok()
    }

    pub fn volumes() -> Vec<VolumeInfo> {
        let mut v = mounts();
        for m in &mut v {
            if m.system || m.root || m.network || !m.device.starts_with("disk") {
                continue;
            }
            if let Some(j) = diskutil_info(&m.device) {
                apply_diskutil_json(m, &j);
            }
        }
        v
    }

    pub fn bundle_id_of(pid: u32) -> Option<String> {
        use objc2_app_kit::NSRunningApplication;
        objc2::rc::autoreleasepool(|_| {
            let app =
                NSRunningApplication::runningApplicationWithProcessIdentifier(pid as libc::pid_t)?;
            app.bundleIdentifier().map(|s| s.to_string())
        })
    }

    pub fn app_name_of(pid: u32) -> Option<String> {
        use objc2_app_kit::NSRunningApplication;
        objc2::rc::autoreleasepool(|_| {
            let app =
                NSRunningApplication::runningApplicationWithProcessIdentifier(pid as libc::pid_t)?;
            app.localizedName().map(|s| s.to_string())
        })
    }

    pub fn who_is_using(path: &Path) -> Vec<ProcessInfo> {
        let is_mount = mounts().iter().any(|m| m.mount_point == path);
        let out = std::process::Command::new("/usr/sbin/lsof")
            .args(lsof_args(path, is_mount))
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(out) = out else { return Vec::new() };
        let me = std::process::id();
        let mut v: Vec<ProcessInfo> = parse_lsof(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .filter(|p| p.pid != me)
            .collect();
        for p in &mut v {
            p.bundle_id = bundle_id_of(p.pid);
            if let Some(n) = app_name_of(p.pid) {
                p.name = n;
            }
        }
        v
    }

    pub fn eject(v: &VolumeInfo) -> Result<(), (String, Vec<ProcessInfo>)> {
        let cmd = eject_command(v);
        let out = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| (e.to_string(), Vec::new()))?;
        if out.status.success() {
            return Ok(());
        }
        let msg = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        let msg = msg.trim().to_string();
        let mut blockers = who_is_using(&v.mount_point);
        if let Some((pid, name)) = parse_dissenter(&msg) {
            if pid != 0 && !blockers.iter().any(|b| b.pid == pid) {
                blockers.insert(
                    0,
                    ProcessInfo {
                        pid,
                        name: app_name_of(pid)
                            .or(name)
                            .unwrap_or_else(|| format!("PID {pid}")),
                        bundle_id: bundle_id_of(pid),
                    },
                );
            }
        }
        let short = msg
            .lines()
            .find(|l| l.contains("dissented") || l.contains("failed") || l.contains("busy"))
            .or_else(|| msg.lines().last())
            .unwrap_or("eject failed")
            .trim()
            .to_string();
        Err((short, blockers))
    }
}

// ---------------------------------------------------------------------------
// Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
        IOCTL_STORAGE_MEDIA_REMOVAL,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn volumes() -> Vec<VolumeInfo> {
        let mask = unsafe { GetLogicalDrives() };
        let mut v = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            let t = unsafe { GetDriveTypeW(wide(&root).as_ptr()) };
            let mut name = [0u16; 261];
            let mut fs = [0u16; 261];
            unsafe {
                GetVolumeInformationW(
                    wide(&root).as_ptr(),
                    name.as_mut_ptr(),
                    name.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            };
            let s = |b: &[u16]| {
                let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
                String::from_utf16_lossy(&b[..n])
            };
            let label = s(&name);
            let sysdrive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
            v.push(VolumeInfo {
                name: if label.is_empty() {
                    format!("{letter}:")
                } else {
                    format!("{label} ({letter}:)")
                },
                mount_point: PathBuf::from(&root),
                device: format!("{letter}:"),
                whole_disk: format!("{letter}:"),
                fs_type: s(&fs),
                internal: !matches!(t, DRIVE_REMOVABLE | DRIVE_CDROM),
                removable: matches!(t, DRIVE_REMOVABLE | DRIVE_CDROM),
                ejectable: matches!(t, DRIVE_REMOVABLE | DRIVE_CDROM),
                disk_image: false,
                network: t == DRIVE_REMOTE,
                root: sysdrive.eq_ignore_ascii_case(&format!("{letter}:")),
                system: false,
                size: 0,
            });
        }
        v
    }

    pub fn who_is_using(path: &Path) -> Vec<ProcessInfo> {
        use windows_sys::Win32::System::RestartManager::{
            RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
            RM_PROCESS_INFO,
        };
        if path.is_dir() {
            return Vec::new();
        }
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        if unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) } != 0 {
            return Vec::new();
        }
        let file = wide(&path.to_string_lossy());
        let files = [file.as_ptr()];
        let mut out = Vec::new();
        unsafe {
            if RmRegisterResources(
                session,
                1,
                files.as_ptr(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            ) == 0
            {
                let mut needed = 0u32;
                let mut n = 16u32;
                let mut infos: Vec<RM_PROCESS_INFO> = vec![std::mem::zeroed(); n as usize];
                let mut reasons = 0u32;
                if RmGetList(
                    session,
                    &mut needed,
                    &mut n,
                    infos.as_mut_ptr(),
                    &mut reasons,
                ) == 0
                {
                    for i in &infos[..n as usize] {
                        let len = i
                            .strAppName
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(i.strAppName.len());
                        out.push(ProcessInfo {
                            pid: i.Process.dwProcessId,
                            name: String::from_utf16_lossy(&i.strAppName[..len]),
                            bundle_id: None,
                        });
                    }
                }
            }
            RmEndSession(session);
        }
        out
    }

    pub fn eject(v: &VolumeInfo) -> Result<(), (String, Vec<ProcessInfo>)> {
        let path = wide(&format!("\\\\.\\{}", v.device));
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err((std::io::Error::last_os_error().to_string(), Vec::new()));
        }
        let io = |code: u32, input: *const core::ffi::c_void, len: u32| -> bool {
            let mut ret = 0u32;
            unsafe {
                DeviceIoControl(
                    h,
                    code,
                    input,
                    len,
                    std::ptr::null_mut(),
                    0,
                    &mut ret,
                    std::ptr::null_mut(),
                ) != 0
            }
        };
        let r = (|| {
            let mut locked = false;
            for _ in 0..10 {
                if io(FSCTL_LOCK_VOLUME, std::ptr::null(), 0) {
                    locked = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if !locked {
                return Err("the drive is in use".to_string());
            }
            if !io(FSCTL_DISMOUNT_VOLUME, std::ptr::null(), 0) {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let prevent: u8 = 0;
            io(
                IOCTL_STORAGE_MEDIA_REMOVAL,
                (&prevent as *const u8).cast(),
                1,
            );
            if !io(IOCTL_STORAGE_EJECT_MEDIA, std::ptr::null(), 0) {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(())
        })();
        unsafe { CloseHandle(h) };
        r.map_err(|e| (e, Vec::new()))
    }
}

// ---------------------------------------------------------------------------
// Public API

/// All mounted volumes (including internal ones).
pub fn all_volumes() -> Vec<VolumeInfo> {
    #[cfg(any(target_os = "macos", windows))]
    {
        imp::volumes()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// The volumes "Eject all" would eject with `filter`.
pub fn external_volumes_with(filter: EjectFilter) -> Vec<VolumeInfo> {
    all_volumes()
        .into_iter()
        .filter(|v| is_ejectable_external(v, filter))
        .collect()
}

/// External, removable volumes and disk images (not network shares).
pub fn external_volumes() -> Vec<VolumeInfo> {
    external_volumes_with(EjectFilter::default())
}

/// Processes that have `path` open: a file, a folder (anything below it)
/// or a mount point (anything on that volume). macOS uses `lsof`, which
/// only sees your own processes without admin rights.
pub fn who_is_using(path: &Path) -> Vec<ProcessInfo> {
    #[cfg(any(target_os = "macos", windows))]
    {
        imp::who_is_using(path)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        Vec::new()
    }
}

/// Eject one volume; on failure the error and the processes using it.
pub fn eject_volume(v: &VolumeInfo) -> Result<(), Box<EjectFailure>> {
    #[cfg(any(target_os = "macos", windows))]
    {
        imp::eject(v).map_err(|(error, blockers)| {
            Box::new(EjectFailure {
                volume: v.clone(),
                error,
                blockers,
            })
        })
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err(Box::new(EjectFailure {
            volume: v.clone(),
            error: "not supported on this platform".into(),
            blockers: Vec::new(),
        }))
    }
}

/// Group volumes by whole disk: ejecting one volume of a disk ejects all
/// of them, so each disk is ejected once (network shares one by one).
pub fn eject_groups(vols: Vec<VolumeInfo>) -> Vec<Vec<VolumeInfo>> {
    let mut groups: Vec<Vec<VolumeInfo>> = Vec::new();
    for v in vols {
        let key = (!v.network && !v.whole_disk.is_empty()).then(|| v.whole_disk.clone());
        match key.and_then(|k| {
            groups
                .iter_mut()
                .find(|g| g[0].whole_disk == k && !g[0].network)
        }) {
            Some(g) => g.push(v),
            None => groups.push(vec![v]),
        }
    }
    groups
}

/// Eject every volume `filter` selects.
pub fn eject_all_with(filter: EjectFilter) -> EjectReport {
    let mut rep = EjectReport::default();
    for g in eject_groups(external_volumes_with(filter)) {
        match eject_volume(&g[0]) {
            Ok(()) => rep.ejected.extend(g),
            Err(f) => {
                // Report every volume of that disk, sharing the reason.
                for v in g {
                    rep.failed.push(EjectFailure {
                        volume: v,
                        error: f.error.clone(),
                        blockers: f.blockers.clone(),
                    });
                }
            }
        }
    }
    rep
}

/// Eject all external volumes using the settings in `file-tools.json`.
pub fn eject_all() -> EjectReport {
    let c = crate::config::Config::load().finder;
    eject_all_with(EjectFilter {
        disk_images: c.eject_disk_images,
        network: c.eject_network_volumes,
    })
}

/// "12.3 GB".
pub fn human_size(b: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} bytes")
    } else {
        format!("{v:.1} {}", units[i])
    }
}
