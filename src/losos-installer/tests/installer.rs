use std::fs;
use std::path::PathBuf;

use losos_installer::disks::{self, Disk};
use losos_installer::github;
use losos_installer::install;
use losos_installer::report;
use losos_installer::serve::{self, Request};
use losos_installer::stage;

/// A scratch directory per test, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("losos-installer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fake_disk(sys: &Scratch, name: &str, sectors: u64, model: &str) {
    sys.write(&format!("block/{name}/size"), &format!("{sectors}\n"));
    sys.write(
        &format!("block/{name}/device/model"),
        &format!("{model}   \n"),
    );
    sys.write(&format!("block/{name}/removable"), "0\n");
    sys.write(&format!("block/{name}/ro"), "0\n");
}

#[test]
fn disks_leave_out_what_cannot_be_installed_onto() {
    let sys = Scratch::new("disks");
    fake_disk(&sys, "sda", 1_000_215_216, "Samsung SSD 870");
    fake_disk(&sys, "sdb", 60_063_744, "Flash Disk");
    sys.write("block/sdb/removable", "1\n");
    fake_disk(&sys, "nvme0n1", 2_000_409_264, "WD_BLACK SN850X");
    fake_disk(&sys, "loop0", 2048, "");
    fake_disk(&sys, "sr0", 2048, "DVD");
    fake_disk(&sys, "mmcblk0boot0", 8192, "");
    fake_disk(&sys, "zram0", 8192, "");
    fake_disk(&sys, "sdc", 0, "Card reader");
    fake_disk(&sys, "sdd", 2048, "Write protected");
    sys.write("block/sdd/ro", "1\n");
    fake_disk(&sys, "nvme1c1n1", 2048, "multipath");
    sys.write("block/nvme1c1n1/hidden", "1\n");

    let found = disks::list(&sys.0, Some("sdb"));
    assert_eq!(
        found,
        [
            Disk {
                name: "nvme0n1".into(),
                path: "/dev/nvme0n1".into(),
                size: 2_000_409_264 * 512,
                model: "WD_BLACK SN850X".into(),
                removable: false,
            },
            Disk {
                name: "sda".into(),
                path: "/dev/sda".into(),
                size: 1_000_215_216 * 512,
                model: "Samsung SSD 870".into(),
                removable: false,
            },
        ]
    );
    assert!(
        disks::list(&sys.0, None)
            .iter()
            .any(|d| d.name == "sdb" && d.removable)
    );
}

#[test]
fn the_medium_is_found_from_its_mount() {
    let sys = Scratch::new("medium");
    fs::create_dir_all(sys.0.join("block/sdb/sdb1")).unwrap();
    sys.write("block/sdb/sdb1/partition", "1\n");
    fs::create_dir_all(sys.0.join("block/sr0")).unwrap();
    fs::create_dir_all(sys.0.join("dev/block")).unwrap();
    std::os::unix::fs::symlink("../../block/sdb/sdb1", sys.0.join("dev/block/8:17")).unwrap();
    std::os::unix::fs::symlink("../../block/sr0", sys.0.join("dev/block/11:0")).unwrap();

    let stick = "22 1 0:21 / / rw - tmpfs tmpfs rw\n\
        35 22 8:17 / /iso ro,relatime - iso9660 /dev/sdb1 ro\n";
    assert_eq!(
        disks::backing_disk(stick, &sys.0, "/iso").as_deref(),
        Some("sdb")
    );
    let optical = "35 22 11:0 / /iso ro,relatime - iso9660 /dev/sr0 ro\n";
    assert_eq!(
        disks::backing_disk(optical, &sys.0, "/iso").as_deref(),
        Some("sr0")
    );
    assert_eq!(disks::backing_disk(stick, &sys.0, "/elsewhere"), None);
}

#[test]
fn esp_node_from_repart_json() {
    let json = r#"[
        {"type":"esp","label":"esp","uuid":"x","node":"/dev/sda1","activity":"create"},
        {"type":"usr-x86-64-verity","label":"_empty","node":"/dev/sda2"}
    ]"#;
    assert_eq!(install::esp_node(json).unwrap(), PathBuf::from("/dev/sda1"));
    assert!(install::esp_node(r#"[{"type":"usr-x86-64","node":"/dev/sda2"}]"#).is_err());
    assert!(install::esp_node("not json").is_err());
}

#[test]
fn transfers_name_the_chosen_disk() {
    let dir = Scratch::new("transfers");
    dir.write(
        "templates/20-usr.transfer",
        "[Target]\nType=partition\nPath=@TARGET@\n",
    );
    dir.write(
        "templates/30-uki.transfer",
        "[Target]\nType=regular-file\nPath=/run/esp/EFI/Linux\n",
    );
    dir.write("templates/README", "not a transfer @TARGET@\n");
    // A stale file from an earlier attempt must not survive into this one.
    dir.write("out/10-old.transfer", "stale\n");

    install::render_transfers(
        &dir.0.join("templates"),
        &dir.0.join("out"),
        "/dev/nvme0n1",
        install::Source::Channel,
    )
    .unwrap();
    let mut names: Vec<_> = fs::read_dir(dir.0.join("out"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["20-usr.transfer", "30-uki.transfer"]);
    assert_eq!(
        fs::read_to_string(dir.0.join("out/20-usr.transfer")).unwrap(),
        "[Target]\nType=partition\nPath=/dev/nvme0n1\n"
    );
    assert!(
        install::render_transfers(
            &dir.0.join("out/none"),
            &dir.0.join("o2"),
            "/dev/sda",
            install::Source::Channel
        )
        .is_err()
    );
}

#[test]
fn a_release_disk_replaces_only_the_source() {
    let dir = Scratch::new("local");
    dir.write(
        "templates/20-usr.transfer",
        "[Transfer]\nVerify=true\n\n[Source]\nMatchPattern=losos_@v.raw.xz\n\
         Path=https://proxy.example/updates/nightly/x86_64/\nType=url-file\n\n\
         [Target]\nPath=@TARGET@\nType=partition\n",
    );
    let release = dir.0.join("release");
    install::render_transfers(
        &dir.0.join("templates"),
        &dir.0.join("out"),
        "/dev/vda",
        install::Source::Disk(install::Local {
            url: "https://proxy.example/updates/nightly/x86_64/",
            dir: &release,
        }),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.0.join("out/20-usr.transfer")).unwrap(),
        format!(
            "[Transfer]\nVerify=true\n\n[Source]\nMatchPattern=losos_@v.raw.xz\n\
             Path={}\nType=regular-file\n\n[Target]\nPath=/dev/vda\nType=partition\n",
            release.display()
        )
    );
}

#[test]
fn the_github_release_replaces_only_the_url() {
    let dir = Scratch::new("mirror");
    let template = "[Transfer]\nVerify=true\n\n[Source]\nMatchPattern=losos_@v.raw.xz\n\
         Path=https://proxy.example/updates/nightly/x86_64/\nType=url-file\n\n\
         [Target]\nPath=@TARGET@\nType=partition\n";
    dir.write("templates/20-usr.transfer", template);
    install::render_transfers(
        &dir.0.join("templates"),
        &dir.0.join("out"),
        "/dev/vda",
        install::Source::Mirror {
            url: "https://proxy.example/updates/nightly/x86_64/",
            mirror: "http://127.0.0.1:4321/",
        },
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.0.join("out/20-usr.transfer")).unwrap(),
        template
            .replace(
                "https://proxy.example/updates/nightly/x86_64/",
                "http://127.0.0.1:4321/"
            )
            .replace("@TARGET@", "/dev/vda")
    );
}

fn release_json(tag: &str, prerelease: bool, names: &[&str]) -> serde_json::Value {
    let assets: Vec<_> = names
        .iter()
        .map(|n| {
            serde_json::json!({
                "name": n,
                "browser_download_url": format!("https://github.example/{tag}/{n}"),
                "size": 10,
            })
        })
        .collect();
    serde_json::json!({"tag_name": tag, "prerelease": prerelease, "draft": false, "assets": assets})
}

#[test]
fn release_tags_come_from_the_cadence_and_version() {
    assert_eq!(
        github::release_tag("nightly", "20261010.175322").as_deref(),
        Some("nightly-20261010T1753")
    );
    assert_eq!(
        github::release_tag("weekly", "20261010.175322").as_deref(),
        Some("weekly-20261010T1753")
    );
    assert_eq!(github::release_tag("nightly", "0.1"), None);
    assert_eq!(github::release_tag("nightly", "20261010.17532x"), None);
}

#[test]
fn a_release_carries_its_version_for_its_architecture() {
    let v = "20261010.175322";
    let files = [
        "SHA256SUMS-x86_64",
        "SHA256SUMS-x86_64.gpg",
        "losos-desktop_20261010.175322_x86_64.efi",
    ];
    let draft = {
        let mut json = release_json("v9", false, &files);
        json["draft"] = true.into();
        json
    };
    assert!(github::parse_release(&draft).is_none());
    let release =
        github::parse_release(&release_json("weekly-20261010T1753", false, &files)).unwrap();
    assert!(github::carries(&release, v, "x86_64"));
    assert!(!github::carries(&release, v, "aarch64"));
    assert!(!github::carries(&release, "20261011.000000", "x86_64"));
}

#[test]
fn split_files_answer_under_their_own_name() {
    let release = github::parse_release(&release_json(
        "t",
        true,
        &[
            "SHA256SUMS-x86_64",
            "SHA256SUMS-x86_64.gpg",
            "SHA256SUMS-aarch64",
            "SHA256SUMS-aarch64.gpg",
            "a_x86_64.efi",
            "a_usr.raw.xz.part-01",
            "a_usr.raw.xz.part-00",
            "a_usr.raw.xz.part-02",
        ],
    ))
    .unwrap();
    let files = github::files(&release, "x86_64");
    let names: Vec<_> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "SHA256SUMS",
            "SHA256SUMS.gpg",
            "a_usr.raw.xz",
            "a_x86_64.efi"
        ]
    );
    assert_eq!(
        files["SHA256SUMS"].urls,
        ["https://github.example/t/SHA256SUMS-x86_64"]
    );
    let usr = &files["a_usr.raw.xz"];
    assert_eq!(usr.size, 30);
    assert_eq!(
        usr.urls,
        [
            "https://github.example/t/a_usr.raw.xz.part-00",
            "https://github.example/t/a_usr.raw.xz.part-01",
            "https://github.example/t/a_usr.raw.xz.part-02",
        ]
    );
}

#[test]
fn sums_parse_in_both_forms() {
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let text = format!("{a}  one.raw.xz\n{b} *two.efi\nnot a line\n");
    assert_eq!(
        install::parse_sums(&text),
        [(a, "one.raw.xz".to_owned()), (b, "two.efi".to_owned())]
    );
}

#[test]
fn a_release_is_checked_file_by_file() {
    let dir = Scratch::new("verify");
    let (tx, _rx) = std::sync::mpsc::channel();
    let pubring = dir.0.join("no-such-pubring.gpg");
    // sha256("hello\n")
    let hello = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
    dir.write("release/one.efi", "hello\n");
    dir.write("release/SHA256SUMS", &format!("{hello}  one.efi\n"));
    assert!(install::has_release(&dir.0.join("release")));
    install::verify_release(&dir.0.join("release"), &pubring, &dir.0, &tx).unwrap();

    dir.write("release/one.efi", "tampered\n");
    assert!(install::verify_release(&dir.0.join("release"), &pubring, &dir.0, &tx).is_err());

    dir.write("release/one.efi", "hello\n");
    dir.write("release/extra.efi", "hello\n");
    assert!(install::verify_release(&dir.0.join("release"), &pubring, &dir.0, &tx).is_err());
    assert!(!install::has_release(&dir.0.join("templates")));
}

#[test]
fn download_progress_is_the_last_percentage() {
    assert_eq!(
        serve::percent("Got 45% of https://example/usr.raw"),
        Some(0.45)
    );
    assert_eq!(serve::percent("10% then 100%"), Some(1.0));
    assert_eq!(serve::percent("no number here"), None);
    assert_eq!(serve::percent("Got % of it"), None);
    assert_eq!(serve::percent("250%"), None);
}

#[test]
fn requests_parse_and_name_a_device() {
    assert_eq!(
        serve::parse_request(r#"{"method":"disks"}"#),
        Ok(Request::Disks)
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"install","disk":"/dev/vda"}"#),
        Ok(Request::Install("/dev/vda".into(), None))
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"install","disk":"/dev/vda","pin":"246813"}"#),
        Ok(Request::Install(
            "/dev/vda".into(),
            Some(install::Pin("246813".into()))
        ))
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"reboot"}"#),
        Ok(Request::Reboot)
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"power_off"}"#),
        Ok(Request::PowerOff)
    );
    assert!(serve::parse_request(r#"{"method":"install","disk":"vda"}"#).is_err());
    assert!(serve::parse_request(r#"{"method":"install"}"#).is_err());
    assert!(serve::parse_request(r#"{"method":"format"}"#).is_err());
    assert!(serve::parse_request("not json").is_err());
}

#[test]
fn steps_and_disks_as_derisk_reads_them() {
    use install::Step;
    // Without a PIN there is nothing to encrypt, and no such step.
    let plain = [Step::Partition, Step::Mount, Step::Download, Step::Finish];
    let steps = serve::steps_event(&plain);
    assert_eq!(steps["event"], "steps");
    assert_eq!(steps["labels"].as_array().unwrap().len(), 4);
    let step = serve::step_event(&plain, Step::Download, Some(0.5));
    assert_eq!(step["index"], 2);
    assert_eq!(step["count"], 4);
    assert_eq!(step["fraction"], 0.5);
    assert!(serve::step_event(&plain, Step::Partition, None)["fraction"].is_null());
    let finish = serve::step_event(&Step::ALL, Step::Finish, None);
    assert_eq!(finish["index"], 4);
    assert_eq!(finish["count"], 5);

    let disk = Disk {
        path: "/dev/vda".into(),
        name: "vda".into(),
        model: "QEMU".into(),
        size: 21_474_836_480,
        removable: false,
    };
    let event = serve::disks_event(&[disk]);
    assert_eq!(event["event"], "disks");
    assert_eq!(event["disks"][0]["path"], "/dev/vda");
    assert_eq!(event["disks"][0]["size"], 21_474_836_480u64);
}

#[test]
fn an_install_is_refused_for_a_disk_not_offered() {
    let config = serve::Config {
        name: "LosOS Desktop".into(),
        repart_definitions: PathBuf::from("/nonexistent"),
        sysupdate_templates: PathBuf::from("/nonexistent"),
        esp: PathBuf::from("/nonexistent"),
        medium: "/nonexistent".into(),
        work: PathBuf::from("/nonexistent"),
        source: None,
        index: None,
        local_source: None,
        bios_boot: None,
        release_repo: None,
        release_version: None,
        release_channel: None,
        report_repo: None,
        report_client_id: None,
        report_login: None,
        report_api: None,
        encryption: None,
    };
    let input = "{\"method\":\"install\",\"disk\":\"/dev/not-a-disk\"}\nbogus\n";
    let (tx, rx) = std::sync::mpsc::channel();
    struct Sink(std::sync::mpsc::Sender<u8>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            buf.iter().for_each(|b| {
                let _ = self.0.send(*b);
            });
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serve::run(&config, input.as_bytes(), Sink(tx));
    let out = String::from_utf8(rx.iter().collect()).unwrap();
    let events: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[0]["event"], "hello");
    assert_eq!(events[0]["name"], "LosOS Desktop");
    assert_eq!(events[1]["event"], "failed");
    assert!(
        events[1]["message"]
            .as_str()
            .unwrap()
            .contains("/dev/not-a-disk")
    );
    assert_eq!(events[2]["event"], "failed");
    assert_eq!(events.len(), 3);
}

#[test]
fn hello_names_a_release_disk_only_when_it_holds_a_release() {
    let dir = Scratch::new("hello");
    let mut config = serve::Config {
        name: "LosOS Desktop".into(),
        repart_definitions: PathBuf::from("/nonexistent"),
        sysupdate_templates: PathBuf::from("/nonexistent"),
        esp: PathBuf::from("/nonexistent"),
        medium: "/nonexistent".into(),
        work: PathBuf::from("/nonexistent"),
        source: Some("https://proxy.example/".into()),
        index: None,
        local_source: Some(dir.0.join("release")),
        bios_boot: None,
        release_repo: None,
        release_version: None,
        release_channel: None,
        report_repo: None,
        report_client_id: None,
        report_login: None,
        report_api: None,
        encryption: None,
    };
    assert!(serve::hello(&config).get("release").is_none());
    dir.write("release/SHA256SUMS", "");
    assert_eq!(
        serve::hello(&config)["release"],
        dir.0.join("release").to_string_lossy().as_ref()
    );
    config.local_source = None;
    assert!(serve::hello(&config).get("release").is_none());
}

#[test]
fn sources_are_read_from_the_transfers() {
    let dir = Scratch::new("patterns");
    dir.write(
        "10-usr.transfer",
        "[Source]\nType=url-file\nPath=@URL@\nMatchPattern=os_@v_usr_@u.raw.xz\n\
         [Target]\nType=partition\nMatchPattern=os_@v\n",
    );
    dir.write(
        "30-uki.transfer",
        "[Transfer]\nProtectVersion=%A\n[Source]\nMatchPattern=os_@v_x86_64.efi\n\
         [Target]\nMatchPattern=os_@v+@l-@d.efi os_@v.efi\n",
    );
    dir.write("README", "MatchPattern=not_a_transfer\n");
    assert_eq!(
        stage::source_patterns(&dir.0).unwrap(),
        ["os_@v_usr_@u.raw.xz", "os_@v_x86_64.efi"]
    );
}

#[test]
fn a_pattern_gives_the_version_it_matched() {
    let v = |p, n| stage::version_of(p, n);
    assert_eq!(
        v(
            "os_@v_usr-x86-64_@u.raw.xz",
            "os_20261016.232901_usr-x86-64_0b8e4a52-6c5c-4a1e-9f4e-0d1c2b3a4f5e.raw.xz"
        )
        .as_deref(),
        Some("20261016.232901")
    );
    // The version has dots in it, and so does what follows it.
    assert_eq!(
        v("os_@v.grub", "os_20261016.232901.grub").as_deref(),
        Some("20261016.232901")
    );
    assert_eq!(
        v("os_@v_x86_64.efi", "os_20261016.232901_aarch64.efi"),
        None
    );
    assert_eq!(v("os_@v_x86_64.efi", "os__x86_64.efi"), None);
    assert_eq!(v("a@@b", "a@b").as_deref(), Some(""));
}

fn file(urls: &[&str], size: u64) -> github::File {
    github::File {
        urls: urls.iter().map(|u| (*u).to_owned()).collect(),
        size,
    }
}

#[test]
fn only_what_the_transfers_read_is_fetched() {
    let mut files = std::collections::BTreeMap::new();
    for name in [
        "SHA256SUMS",
        "SHA256SUMS.gpg",
        "os_1.0_usr_u1.raw.xz",
        "os_1.0_x86_64.efi",
        "os_1.0_x86_64-installer.iso",
        "os_1.1_usr_u2.raw.xz",
        "os_1.1_x86_64.efi",
        "os_1.2_usr_u3.raw.xz",
    ] {
        files.insert(
            name.to_owned(),
            file(&[&format!("https://g.example/{name}")], 1),
        );
    }
    let patterns = [
        "os_@v_usr_@u.raw.xz".to_owned(),
        "os_@v_x86_64.efi".to_owned(),
    ];

    let names = |picked: std::collections::BTreeMap<String, github::File>| {
        picked.into_keys().collect::<Vec<_>>()
    };
    // The release's own version, and nothing else of it: no ISO.
    assert_eq!(
        names(stage::wanted(&files, &patterns, Some("1.0")).unwrap()),
        [
            "SHA256SUMS",
            "SHA256SUMS.gpg",
            "os_1.0_usr_u1.raw.xz",
            "os_1.0_x86_64.efi"
        ]
    );
    // The channel's newest with every file: 1.2 has no UKI.
    assert_eq!(
        names(stage::wanted(&files, &patterns, None).unwrap()),
        [
            "SHA256SUMS",
            "SHA256SUMS.gpg",
            "os_1.1_usr_u2.raw.xz",
            "os_1.1_x86_64.efi"
        ]
    );
    assert!(stage::wanted(&files, &patterns, Some("1.2")).is_err());
    files.remove("SHA256SUMS.gpg");
    assert!(stage::wanted(&files, &patterns, Some("1.0")).is_err());
}

#[test]
fn the_channel_index_reads_as_files() {
    let files = stage::index_files(
        "SHA256SUMS 1096 https://g.example/t/SHA256SUMS-x86_64\n\
         os_usr.raw.xz 30 https://g.example/t/os_usr.raw.xz.part-00 https://g.example/t/os_usr.raw.xz.part-01\n\
         broken\n\
         nosize x https://g.example/t/nosize\n",
    );
    assert_eq!(files.len(), 2);
    assert_eq!(files["SHA256SUMS"].size, 1096);
    assert_eq!(
        files["os_usr.raw.xz"].urls,
        [
            "https://g.example/t/os_usr.raw.xz.part-00",
            "https://g.example/t/os_usr.raw.xz.part-01"
        ]
    );
}

#[test]
fn aria2_saves_every_part_under_its_own_name() {
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "os_usr.raw.xz".to_owned(),
        file(
            &[
                "https://g.example/t/os_usr.raw.xz.part-00",
                "https://g.example/t/os_usr.raw.xz.part-01?x=1",
            ],
            30,
        ),
    );
    files.insert(
        "SHA256SUMS".to_owned(),
        file(&["https://g.example/t/SHA256SUMS-x86_64"], 1),
    );
    assert_eq!(
        stage::aria2_input(&files),
        "https://g.example/t/SHA256SUMS-x86_64\n  out=SHA256SUMS-x86_64\n\
         https://g.example/t/os_usr.raw.xz.part-00\n  out=os_usr.raw.xz.part-00\n\
         https://g.example/t/os_usr.raw.xz.part-01?x=1\n  out=os_usr.raw.xz.part-01\n"
    );
    // A 2.5 GiB /usr gets its own size, ext4's share and some room on top.
    let mut usr = std::collections::BTreeMap::new();
    usr.insert(
        "u".to_owned(),
        file(&["https://g.example/u"], 2_621_440_000),
    );
    let size = stage::scratch_size(&usr);
    assert_eq!(size % (1 << 20), 0);
    assert!(size > 2_621_440_000 + (256 << 20));
    assert!(size < 4 << 30);
    assert!((1..=stage::MAX_CONNECTIONS).contains(&stage::connections()));
}

#[test]
fn the_scratch_partition_is_found_in_reparts_table() {
    let table = r#"[
        {"type":"esp","label":"ESP","node":"/dev/nvme0n1p1","activity":"unchanged"},
        {"type":"linux-generic","label":"losos-download","node":"/dev/nvme0n1p6","activity":"create"}
    ]"#;
    let node = stage::scratch_node(table).unwrap();
    assert_eq!(node, PathBuf::from("/dev/nvme0n1p6"));
    assert_eq!(stage::partition_number(&node), Some(6));
    assert_eq!(stage::partition_number("/dev/vda12".as_ref()), Some(12));
    assert_eq!(stage::partition_number("/dev/vda".as_ref()), None);
    assert!(stage::scratch_node(r#"[{"type":"esp","node":"/dev/vda1"}]"#).is_err());
}

#[test]
fn the_mirror_serves_a_split_file_whole() {
    use std::io::{Read, Write};

    let dir = Scratch::new("served");
    dir.write("u.part-00", "hello, ");
    dir.write("u.part-01", "world");
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "u.raw.xz".to_owned(),
        stage::Staged {
            paths: vec![dir.0.join("u.part-00"), dir.0.join("u.part-01")],
            size: 12,
        },
    );
    let mirror = github::Mirror::new(files).unwrap();
    let addr = mirror.url().unwrap();
    let addr = addr
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let (events, _log) = std::sync::mpsc::channel();
    let get = |path: &str| {
        let mut stream = std::net::TcpStream::connect(&addr).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    };
    let (whole, missing) = mirror.serve(&events, || (get("/u.raw.xz"), get("/other")));
    assert!(whole.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(whole.contains("Content-Length: 12\r\n"));
    assert!(whole.ends_with("\r\n\r\nhello, world"), "{whole:?}");
    assert!(missing.starts_with("HTTP/1.1 404"));
}

fn quiet_config() -> serve::Config {
    serve::Config {
        name: "LosOS Desktop".into(),
        repart_definitions: PathBuf::from("/nonexistent"),
        sysupdate_templates: PathBuf::from("/nonexistent"),
        esp: PathBuf::from("/nonexistent"),
        medium: "/nonexistent".into(),
        work: PathBuf::from("/nonexistent"),
        source: None,
        index: None,
        local_source: None,
        bios_boot: None,
        release_repo: Some("losos-project/desktop".into()),
        release_version: Some("20261027.081500".into()),
        release_channel: None,
        report_repo: None,
        report_client_id: None,
        report_login: None,
        report_api: None,
        encryption: None,
    }
}

#[test]
fn reports_are_offered_only_with_an_app_to_sign_in_with() {
    let mut config = quiet_config();
    assert!(serve::hello(&config).get("report").is_none());
    config.report_client_id = Some("Iv23liExample".into());
    assert_eq!(serve::hello(&config)["report"], "losos-project/desktop");
    config.report_repo = Some("someone/fork".into());
    assert_eq!(serve::hello(&config)["report"], "someone/fork");
    let target = serve::report_target(&config).unwrap();
    assert_eq!(target.login, "https://github.com");
    assert_eq!(target.api, "https://api.github.com");
}

#[test]
fn report_requests_parse() {
    assert_eq!(
        serve::parse_request(r#"{"method":"report"}"#),
        Ok(Request::Report)
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"report_send","note":"it froze"}"#),
        Ok(Request::ReportSend("it froze".into()))
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"report_send"}"#),
        Ok(Request::ReportSend(String::new()))
    );
    assert_eq!(
        serve::parse_request(r#"{"method":"report_cancel"}"#),
        Ok(Request::ReportCancel)
    );
}

#[test]
fn nothing_is_reported_before_an_install_fails() {
    let mut config = quiet_config();
    config.report_client_id = Some("Iv23liExample".into());
    let input = "{\"method\":\"report\"}\n{\"method\":\"report_send\"}\n";
    let out = SharedSink::default();
    serve::run(&config, input.as_bytes(), out.clone());
    let events: Vec<serde_json::Value> = String::from_utf8(out.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[1]["event"], "report_failed");
    assert_eq!(events[2]["event"], "report_failed");
    assert_eq!(events.len(), 3);
}

/// A writer `serve::run` can own, read through a clone once it returns.
#[derive(Clone, Default)]
struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// GitHub's login and API on 127.0.0.1, answering each request from
/// `answer` and keeping what was asked: the request line, whether a token
/// came with it, and the body.
fn fake_github(
    answer: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
) -> (
    String,
    std::sync::mpsc::Receiver<(String, Option<String>, String)>,
) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut length, mut token) = (0, None);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(n) = lower.strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    token = Some(line.trim()["authorization:".len()..].trim().to_owned());
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let mut words = request.split_whitespace();
            let (method, path) = (words.next().unwrap(), words.next().unwrap());
            let (status, json) = answer(method, path);
            let _ = tx.send((format!("{method} {path}"), token, body));
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            )
            .unwrap();
        }
    });
    (url, rx)
}

fn a_report() -> report::Report {
    let failure = report::Failure {
        step: "Downloading and writing the system".into(),
        position: (3, 4),
        message: "oras failed (exit status: 1)".into(),
        log: ["$ oras pull", "Error: connection reset"]
            .map(String::from)
            .into(),
        disk: "QEMU HARDDISK, 21.5 GB".into(),
    };
    report::build(&failure, &report::System::default(), &[])
}

#[test]
fn a_report_signs_in_then_files_an_issue() {
    let polls = std::sync::atomic::AtomicUsize::new(0);
    let (url, asked) = fake_github(move |method, path| {
        match (method, path) {
        ("POST", "/login/device/code") => (
            200,
            r#"{"device_code":"dev","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":0}"#.into(),
        ),
        ("POST", "/login/oauth/access_token") => {
            if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, r#"{"error":"authorization_pending"}"#.into())
            } else {
                (200, r#"{"access_token":"ghu_secret","token_type":"bearer"}"#.into())
            }
        }
        ("GET", p) if p.starts_with("/search/issues?q=") => (200, r#"{"items":[]}"#.into()),
        ("POST", "/repos/losos-project/desktop/issues") => (
            201,
            r#"{"number":7,"html_url":"https://github.com/losos-project/desktop/issues/7"}"#.into(),
        ),
        _ => (404, r#"{"message":"Not Found"}"#.into()),
    }
    });
    let target = report::Target {
        repo: "losos-project/desktop".into(),
        client_id: "Iv23liExample".into(),
        login: url.clone(),
        api: url,
    };
    let scratch = Scratch::new("report");
    let report = a_report();
    let said = std::sync::Mutex::new(Vec::new());
    let cancel = std::sync::atomic::AtomicBool::new(false);
    report::send(
        &target,
        &report,
        &report::with_note(&report, report::NOTE_HEADING, "It stopped at 3%"),
        &scratch.0,
        &cancel,
        |p| said.lock().unwrap().push(p),
    )
    .unwrap();
    assert_eq!(
        said.into_inner().unwrap(),
        [
            report::Progress::SignIn {
                uri: "https://github.com/login/device".into(),
                code: "WDJB-MJHT".into(),
            },
            report::Progress::Sent {
                url: "https://github.com/losos-project/desktop/issues/7".into(),
                existing: false,
            },
        ]
    );
    let asked: Vec<_> = asked.try_iter().collect();
    assert_eq!(asked.len(), 5);
    // The sign-in never carries a token; the API always does.
    assert_eq!(asked[0].0, "POST /login/device/code");
    assert_eq!(asked[0].1, None);
    assert_eq!(asked[0].2, "client_id=Iv23liExample");
    assert!(asked[2].2.contains("device_code=dev"));
    assert!(asked[3].0.contains(&report.fingerprint));
    assert_eq!(asked[3].1.as_deref(), Some("Bearer ghu_secret"));
    let issue: serde_json::Value = serde_json::from_str(&asked[4].2).unwrap();
    assert_eq!(issue["title"], report.title);
    assert!(
        issue["body"]
            .as_str()
            .unwrap()
            .starts_with("**From the person installing:**\n\n> It stopped at 3%")
    );
    assert_eq!(asked[4].1.as_deref(), Some("Bearer ghu_secret"));
    // Nothing of the request is left behind.
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
}

#[test]
fn the_same_failure_again_is_a_comment() {
    let report = a_report();
    let title = report.title.clone();
    let (url, asked) = fake_github(move |method, path| {
        match (method, path) {
        ("POST", "/login/device/code") => (
            200,
            r#"{"device_code":"dev","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","interval":0}"#.into(),
        ),
        ("POST", "/login/oauth/access_token") => (200, r#"{"access_token":"ghu_secret"}"#.into()),
        ("GET", _) => (
            200,
            serde_json::json!({"items": [{"number": 3, "title": title}]}).to_string(),
        ),
        ("POST", "/repos/losos-project/desktop/issues/3/comments") => (
            201,
            r#"{"html_url":"https://github.com/losos-project/desktop/issues/3#issuecomment-1"}"#.into(),
        ),
        _ => (404, r#"{"message":"Not Found"}"#.into()),
    }
    });
    let target = report::Target {
        repo: "losos-project/desktop".into(),
        client_id: "Iv23liExample".into(),
        login: url.clone(),
        api: url,
    };
    let scratch = Scratch::new("report-again");
    let said = std::sync::Mutex::new(Vec::new());
    let cancel = std::sync::atomic::AtomicBool::new(false);
    report::send(&target, &report, &report.body, &scratch.0, &cancel, |p| {
        said.lock().unwrap().push(p)
    })
    .unwrap();
    assert_eq!(
        said.into_inner().unwrap()[1],
        report::Progress::Sent {
            url: "https://github.com/losos-project/desktop/issues/3#issuecomment-1".into(),
            existing: true,
        }
    );
    assert_eq!(asked.try_iter().count(), 4);
}

#[test]
fn a_declined_sign_in_says_so() {
    let (url, _asked) = fake_github(|method, path| {
        match (method, path) {
        ("POST", "/login/device/code") => (
            200,
            r#"{"device_code":"dev","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","interval":0}"#.into(),
        ),
        _ => (200, r#"{"error":"access_denied"}"#.into()),
    }
    });
    let target = report::Target {
        repo: "losos-project/desktop".into(),
        client_id: "Iv23liExample".into(),
        login: url.clone(),
        api: url,
    };
    let scratch = Scratch::new("report-declined");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let report = a_report();
    let error =
        report::send(&target, &report, &report.body, &scratch.0, &cancel, |_| {}).unwrap_err();
    assert!(error.contains("declined"), "{error}");
}
