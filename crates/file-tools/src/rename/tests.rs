use super::*;
use rand::SeedableRng;
use std::fs;
use std::time::{Duration, UNIX_EPOCH};

fn rng() -> rand::rngs::StdRng {
    rand::rngs::StdRng::seed_from_u64(1)
}

fn touch(dir: &Path, rel: &str) -> PathBuf {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, rel.as_bytes()).unwrap();
    p
}

fn mkdir(dir: &Path, rel: &str) -> PathBuf {
    let p = dir.join(rel);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Leaf names in `dir`, sorted.
fn ls(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn opts(search: &str, replace: &str) -> RenameOptions {
    RenameOptions {
        search: search.into(),
        replace: replace.into(),
        ..Default::default()
    }
}

fn names(items: &[RenameItem], plan: &Plan) -> Vec<(String, Option<String>, Status)> {
    items
        .iter()
        .zip(&plan.items)
        .map(|(i, p)| (i.name.clone(), p.new_name.clone(), p.status))
        .collect()
}

fn fake(name: &str, is_dir: bool, depth: usize) -> RenameItem {
    RenameItem {
        path: PathBuf::from("/nonexistent-pm-test").join(name),
        name: name.into(),
        is_dir,
        depth,
        selected: true,
        created: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        modified: Some(UNIX_EPOCH + Duration::from_secs(1_710_000_000)),
        accessed: None,
    }
}

fn new_names(items: &[RenameItem], o: &RenameOptions) -> Vec<Option<String>> {
    plan_with_rng(items, o, &mut rng())
        .items
        .into_iter()
        .map(|p| p.new_name)
        .collect()
}

#[test]
fn plain_and_regex_preview() {
    let items = vec![fake("IMG_001.JPG", false, 0), fake("notes.txt", false, 0)];
    assert_eq!(
        new_names(&items, &opts("img_", "Photo ")),
        vec![Some("Photo 001.JPG".into()), None]
    );
    let mut o = opts(r"^IMG_(\d+)\.(\w+)$", "$2-$1");
    o.use_regex = true;
    assert_eq!(new_names(&items, &o), vec![Some("JPG-001".into()), None]);
    o.case_sensitive = true;
    o.search = r"^img_(\d+)".into();
    assert_eq!(new_names(&items, &o), vec![None, None]);
    // Invalid regex: no renames, error reported.
    let mut o = opts("(", "x");
    o.use_regex = true;
    let p = plan(&items, &o);
    assert!(p.error.is_some());
    assert_eq!(p.rename_count, 0);
}

#[test]
fn match_all_toggle() {
    let items = vec![fake("a-a-a.txt", false, 0)];
    let mut o = opts("a", "b");
    o.apply_to = ApplyTo::NameOnly;
    assert_eq!(new_names(&items, &o), vec![Some("b-b-b.txt".into())]);
    o.match_all = false;
    assert_eq!(new_names(&items, &o), vec![Some("b-a-a.txt".into())]);
}

#[test]
fn apply_to_parts() {
    let items = vec![
        fake("photo.jpeg", false, 0),
        fake("jpeg folder", true, 0),
        fake("README", false, 0),
    ];
    let mut o = opts("jpeg", "jpg");
    assert_eq!(
        new_names(&items, &o),
        vec![Some("photo.jpg".into()), Some("jpg folder".into()), None]
    );
    o.apply_to = ApplyTo::ExtensionOnly;
    let p = plan(&items, &o);
    assert_eq!(
        names(&items, &p),
        vec![
            (
                "photo.jpeg".into(),
                Some("photo.jpg".into()),
                Status::Rename
            ),
            ("jpeg folder".into(), None, Status::Skipped),
            ("README".into(), None, Status::Unchanged),
        ]
    );
    o.apply_to = ApplyTo::NameOnly;
    o.search = "o".into();
    o.replace = "0".into();
    assert_eq!(
        new_names(&items, &o)[0],
        Some("ph0t0.jpeg".into()),
        "extension untouched"
    );
}

#[test]
fn exclusions_and_selection() {
    let mut items = vec![
        fake("a file", false, 0),
        fake("a folder", true, 0),
        fake("a child", false, 1),
    ];
    let base = opts("a ", "b ");
    let run = |items: &[RenameItem], f: &dyn Fn(&mut RenameOptions)| {
        let mut o = base.clone();
        f(&mut o);
        new_names(items, &o)
            .into_iter()
            .map(|n| n.is_some())
            .collect::<Vec<_>>()
    };
    assert_eq!(run(&items, &|_| {}), vec![true, true, true]);
    assert_eq!(
        run(&items, &|o| o.exclude_files = true),
        vec![false, true, false]
    );
    assert_eq!(
        run(&items, &|o| o.exclude_folders = true),
        vec![true, false, true]
    );
    assert_eq!(
        run(&items, &|o| o.exclude_subfolder_items = true),
        vec![true, true, false]
    );
    items[0].selected = false;
    assert_eq!(run(&items, &|_| {}), vec![false, true, true]);
}

#[test]
fn text_formatting() {
    let items = vec![fake("the QUICK brown fox.TXT", false, 0)];
    let mut o = opts("", "");
    o.text_case = TextCase::Upper;
    assert_eq!(
        new_names(&items, &o),
        vec![Some("THE QUICK BROWN FOX.TXT".into())]
    );
    o.text_case = TextCase::Lower;
    assert_eq!(
        new_names(&items, &o),
        vec![Some("the quick brown fox.txt".into())]
    );
    o.text_case = TextCase::Title;
    assert_eq!(
        new_names(&items, &o),
        vec![Some("The Quick Brown Fox.TXT".into())]
    );
    o.text_case = TextCase::Capitalize;
    o.search = "fox".into();
    o.replace = "dog of".into();
    assert_eq!(
        new_names(&items, &o),
        vec![Some("The Quick Brown Dog Of.TXT".into())]
    );
    o.text_case = TextCase::Lower;
    o.apply_to = ApplyTo::ExtensionOnly;
    o.search.clear();
    assert_eq!(
        new_names(&items, &o),
        vec![Some("the QUICK brown fox.txt".into())]
    );
}

#[test]
fn trims_result() {
    let items = vec![fake("draft final.txt", false, 0)];
    let mut o = opts("final.txt", " ");
    o.apply_to = ApplyTo::NameAndExtension;
    assert_eq!(new_names(&items, &o), vec![Some("draft".into())]);
    let o = opts("draft final.txt", "");
    let p = plan(&items, &o);
    assert_eq!(p.items[0].status, Status::InvalidName);
}

#[test]
fn enumeration() {
    let items: Vec<_> = ["a.jpg", "b.jpg", "skip.txt", "c.jpg"]
        .iter()
        .map(|n| fake(n, false, 0))
        .collect();
    let mut o = opts(r"^.*(?=\.jpg$)", "photo_${start=10,increment=2,padding=4}");
    o.use_regex = true;
    assert_eq!(
        new_names(&items, &o),
        vec![
            Some("photo_0010.jpg".into()),
            Some("photo_0012.jpg".into()),
            None,
            Some("photo_0014.jpg".into()),
        ]
    );
    // Default counter, two counters in one template.
    let o = opts(".jpg", "-${}-${start=1}.jpg");
    assert_eq!(new_names(&items, &o)[1], Some("b-1-2.jpg".into()));
    // Enumerate off: literal.
    let mut o = opts(".jpg", "${}.jpg");
    o.enumerate = false;
    assert_eq!(new_names(&items, &o)[0], Some("a${}.jpg".into()));
}

#[test]
fn random_values() {
    let items = vec![fake("a.txt", false, 0), fake("b.txt", false, 0)];
    let mut o = opts(r"^[^.]+", "${rstringalnum=9}");
    o.use_regex = true;
    let n = new_names(&items, &o);
    let a = n[0].as_ref().unwrap();
    assert_eq!(a.len(), 9 + 4);
    assert!(a.ends_with(".txt"));
    assert_ne!(n[0], n[1]);
    o.replace = "${ruuidv4}".into();
    let n = new_names(&items, &o);
    assert_eq!(n[0].as_ref().unwrap().len(), 36 + 4);
    o.replace = "${rstringdigit=4}-${rstringalpha=3}".into();
    let s = new_names(&items, &o)[0].clone().unwrap();
    let (d, rest) = s.split_once('-').unwrap();
    assert!(d.len() == 4 && d.chars().all(|c| c.is_ascii_digit()));
    assert!(rest[..3].chars().all(|c| c.is_ascii_alphabetic()));
}

#[test]
fn date_variables_from_file_times() {
    use chrono::TimeZone;
    let created = chrono::Local.with_ymd_and_hms(2021, 5, 4, 9, 8, 7).unwrap();
    let modified = chrono::Local
        .with_ymd_and_hms(2023, 12, 25, 18, 30, 0)
        .unwrap();
    let mut it = fake("scan.pdf", false, 0);
    it.created = Some(SystemTime::from(created));
    it.modified = Some(SystemTime::from(modified));
    let items = vec![it];
    let mut o = opts("scan", "$YYYY-$MM-$DD $hh.$mm.$ss");
    assert_eq!(
        new_names(&items, &o),
        vec![Some("2021-05-04 09.08.07.pdf".into())]
    );
    o.time_source = TimeSource::Modified;
    o.replace = "$DDD $D $MMM $YY $HH$tt".into();
    assert_eq!(
        new_names(&items, &o),
        vec![Some("Mon 25 Dec 23 06pm.pdf".into())]
    );
    // Accessed missing: falls back to modified.
    o.time_source = TimeSource::Accessed;
    o.replace = "$YYYY".into();
    assert_eq!(new_names(&items, &o), vec![Some("2023.pdf".into())]);
    // Regex mode: $$ is a literal dollar.
    o.use_regex = true;
    o.replace = "$$YYYY".into();
    assert_eq!(new_names(&items, &o), vec![Some("$YYYY.pdf".into())]);
}

#[test]
fn invalid_names_and_duplicates() {
    let items = vec![fake("a1.txt", false, 0), fake("a2.txt", false, 0)];
    let mut o = opts(r"\d", "");
    o.use_regex = true;
    let p = plan(&items, &o);
    assert_eq!(p.items[0].status, Status::Duplicate);
    assert_eq!(p.items[1].status, Status::Duplicate);
    assert_eq!(p.rename_count, 0);
    assert_eq!(p.error_count, 2);
    let o = opts("1", "/");
    assert_eq!(plan(&items, &o).items[0].status, Status::InvalidChars);
    let o = opts("1", &"x".repeat(300));
    assert_eq!(plan(&items, &o).items[0].status, Status::TooLong);
}

#[test]
fn unicode_nfd_names_match() {
    // "é" decomposed, as macOS often stores it.
    let items = vec![fake("Cafe\u{301}.txt", false, 0)];
    assert_eq!(
        new_names(&items, &opts("café", "Bar")),
        vec![Some("Bar.txt".into())]
    );
    // Normalization alone is not a change.
    let mut o = opts("", "");
    o.text_case = TextCase::None;
    assert_eq!(new_names(&items, &o), vec![None]);
}

#[test]
fn natural_sort() {
    let mut v = vec!["file10", "File2", "file1", "a"];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(v, vec!["a", "file1", "File2", "file10"]);
}

#[test]
fn collect_recursive() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    let top = mkdir(root, "Top");
    touch(root, "Top/b.txt");
    touch(root, "Top/a10.txt");
    touch(root, "Top/a2.txt");
    touch(root, "Top/.hidden");
    touch(root, "Top/Sub/deep.txt");
    let other = touch(root, "z.txt");
    // A child selected along with its parent appears once.
    let items = collect_items(&[other.clone(), top.join("b.txt"), top.clone()]);
    let got: Vec<(String, usize, bool)> = items
        .iter()
        .map(|i| (i.name.clone(), i.depth, i.is_dir))
        .collect();
    assert_eq!(
        got,
        vec![
            ("Top".into(), 0, true),
            ("a2.txt".into(), 1, false),
            ("a10.txt".into(), 1, false),
            ("b.txt".into(), 1, false),
            ("Sub".into(), 1, true),
            ("deep.txt".into(), 2, false),
            ("z.txt".into(), 0, false),
        ]
    );
}

fn run(paths: &[PathBuf], o: &RenameOptions, journal: &Path) -> ApplyReport {
    let items = collect_items(paths);
    let p = plan_with_rng(&items, o, &mut rng());
    assert_eq!(p.error_count, 0, "{:?}", names(&items, &p));
    let mut n = 0;
    let r = apply(&items, &p, Some(journal), &mut |d| n = d);
    assert_eq!(n, p.rename_count);
    r
}

#[test]
fn apply_and_undo_simple() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let j = d.path().join("undo.json");
    let a = touch(&w, "IMG_1.jpg");
    let b = touch(&w, "IMG_2.jpg");
    let r = run(&[a, b], &opts("IMG_", "Holiday "), &j);
    assert_eq!(r.renamed, 2);
    assert!(r.errors.is_empty());
    assert_eq!(ls(&w), vec!["Holiday 1.jpg", "Holiday 2.jpg"]);
    // Content followed the rename.
    assert_eq!(
        fs::read_to_string(w.join("Holiday 1.jpg")).unwrap(),
        "IMG_1.jpg"
    );
    let u = undo_last(&j).unwrap();
    assert_eq!(u.renamed, 2);
    assert!(u.errors.is_empty());
    assert_eq!(ls(&w), vec!["IMG_1.jpg", "IMG_2.jpg"]);
    assert!(!j.exists());
    assert!(undo_last(&j).is_err());
}

#[test]
fn swap_and_rotate() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let j = d.path().join("undo.json");
    let a = touch(&w, "a.txt");
    let b = touch(&w, "b.txt");
    let c = touch(&w, "c.txt");
    // a→b, b→c, c→a in one batch (a rotation; a swap is the 2-cycle).
    let items = collect_items(&[a, b, c]);
    let mut p = plan(&items, &opts("zzz", ""));
    for (i, n) in ["b.txt", "c.txt", "a.txt"].iter().enumerate() {
        p.items[i] = Planned {
            new_name: Some(n.to_string()),
            status: Status::Rename,
        };
    }
    let r = apply(&items, &p, Some(&j), &mut |_| {});
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(fs::read_to_string(w.join("b.txt")).unwrap(), "a.txt");
    assert_eq!(fs::read_to_string(w.join("c.txt")).unwrap(), "b.txt");
    assert_eq!(fs::read_to_string(w.join("a.txt")).unwrap(), "c.txt");
    assert_eq!(ls(&w), vec!["a.txt", "b.txt", "c.txt"]);
    undo_last(&j).unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(fs::read_to_string(w.join(n)).unwrap(), n);
    }
}

#[test]
fn swap_through_plan() {
    // The planner accepts a swap (both names are renamed away).
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let a = touch(&w, "x_1.txt");
    let b = touch(&w, "x_2.txt");
    let items = collect_items(&[a, b]);
    // Regex that maps 1→2 and 2→1 via a template isn't expressible; use a
    // counter that walks backwards: x_1 → x_2, x_2 → x_1.
    let mut o = opts(r"\d", "${start=2,increment=-1}");
    o.use_regex = true;
    let p = plan(&items, &o);
    assert_eq!(p.rename_count, 2, "{:?}", names(&items, &p));
    let r = apply(&items, &p, None, &mut |_| {});
    assert!(r.errors.is_empty());
    assert_eq!(fs::read_to_string(w.join("x_2.txt")).unwrap(), "x_1.txt");
    assert_eq!(fs::read_to_string(w.join("x_1.txt")).unwrap(), "x_2.txt");
}

#[test]
fn case_only_rename() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let j = d.path().join("undo.json");
    let a = touch(&w, "photo.jpg");
    let dir = mkdir(&w, "my folder");
    let mut o = opts("", "");
    o.text_case = TextCase::Capitalize;
    let r = run(&[a, dir], &o, &j);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.renamed, 2);
    assert_eq!(ls(&w), vec!["My Folder", "Photo.jpg"]);
    undo_last(&j).unwrap();
    assert_eq!(ls(&w), vec!["my folder", "photo.jpg"]);
}

#[test]
fn recursive_children_before_parents() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let j = d.path().join("undo.json");
    let top = mkdir(&w, "old top");
    touch(&top, "old a.txt");
    touch(&top, "old sub/old b.txt");
    touch(&top, "old sub/old deeper/old c.txt");
    let r = run(&[top], &opts("old", "new"), &j);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.renamed, 6);
    assert_eq!(
        fs::read_to_string(w.join("new top/new sub/new deeper/new c.txt")).unwrap(),
        "old sub/old deeper/old c.txt"
    );
    assert_eq!(ls(&w.join("new top")), vec!["new a.txt", "new sub"]);
    // Journal: deepest first.
    let depths: Vec<usize> = r
        .journal
        .levels
        .iter()
        .map(|l| l[0].from.components().count())
        .collect();
    assert!(depths.windows(2).all(|w| w[0] > w[1]), "{depths:?}");
    let u = undo_last(&j).unwrap();
    assert!(u.errors.is_empty(), "{:?}", u.errors);
    assert_eq!(
        fs::read_to_string(w.join("old top/old sub/old deeper/old c.txt")).unwrap(),
        "old sub/old deeper/old c.txt"
    );
    assert_eq!(ls(&w), vec!["old top"]);
}

#[test]
fn never_overwrites() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let a = touch(&w, "a.txt");
    touch(&w, "b.txt");
    // Not selected, so b.txt stays: the plan reports the conflict.
    let items = collect_items(std::slice::from_ref(&a));
    let p = plan(&items, &opts("a", "b"));
    assert_eq!(p.items[0].status, Status::Exists);
    // Case-insensitively too, where the FS is.
    let p = plan(&items, &opts("a", "B"));
    if CASE_INSENSITIVE_FS {
        assert_eq!(p.items[0].status, Status::Exists);
    }
    // A file appearing after the preview isn't overwritten either.
    let p = plan(&items, &opts("a", "c"));
    assert_eq!(p.items[0].status, Status::Rename);
    fs::write(w.join("c.txt"), "late").unwrap();
    let r = apply(&items, &p, None, &mut |_| {});
    assert_eq!(r.renamed, 0);
    assert_eq!(r.errors.len(), 1);
    assert_eq!(fs::read_to_string(w.join("c.txt")).unwrap(), "late");
    assert_eq!(fs::read_to_string(&a).unwrap(), "a.txt");
}

#[test]
fn conflict_cascades() {
    // a→b is fine only if b moves away; b's rename fails (exists), so a
    // must not be renamed either.
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let a = touch(&w, "1.txt");
    let b = touch(&w, "2.txt");
    touch(&w, "3.txt"); // not selected
    let items = collect_items(&[a, b]);
    let mut o = opts(r"\d", "${start=2}");
    o.use_regex = true;
    let p = plan(&items, &o);
    assert_eq!(
        names(&items, &p).iter().map(|x| x.2).collect::<Vec<_>>(),
        vec![Status::Exists, Status::Exists]
    );
}

#[test]
fn undo_partial_keeps_rest() {
    let d = tempfile::tempdir().unwrap();
    let w = mkdir(d.path(), "w");
    let j = d.path().join("undo.json");
    let a = touch(&w, "a1.txt");
    let b = touch(&w, "a2.txt");
    let r = run(&[a, b], &opts("a", "b"), &j);
    assert_eq!(r.renamed, 2);
    // Block one undo: a1.txt reappears.
    fs::write(w.join("a1.txt"), "blocker").unwrap();
    let u = undo_last(&j).unwrap();
    assert_eq!(u.renamed, 1);
    assert_eq!(u.errors.len(), 1);
    let rest = Journal::load(&j).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(fs::read_to_string(w.join("a1.txt")).unwrap(), "blocker");
    assert!(w.join("b1.txt").exists());
}

#[test]
fn journal_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let j = d.path().join("x/undo.json");
    let jr = Journal {
        time: 5,
        levels: vec![vec![JournalOp {
            from: "/a".into(),
            to: "/b".into(),
        }]],
    };
    jr.save(&j).unwrap();
    assert_eq!(Journal::load(&j), Some(jr));
    let s = fs::read_to_string(&j).unwrap();
    assert!(s.contains("\"levels\""));
}
