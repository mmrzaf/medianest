#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Cli {
    home: tempfile::TempDir,
    library: PathBuf,
}

impl Cli {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let library = home.path().join("Library");
        Self { home, library }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_medianest-cli"))
            .arg("--library")
            .arg(&self.library)
            .args(args)
            .env("HOME", self.home.path())
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .output()
            .expect("run medianest-cli")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "medianest-cli {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.ok(&all)).expect("json output")
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../medianest-media/tests/fixtures")
}

#[test]
fn full_workflow_from_the_command_line() {
    let cli = Cli::new();
    let init = Command::new(env!("CARGO_BIN_EXE_medianest-cli"))
        .arg("init")
        .arg(&cli.library)
        .output()
        .expect("init");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let card = cli.home.path().join("card");
    std::fs::create_dir_all(&card).expect("card");
    for name in ["camera.jpg", "phone.jpg", "rotated.heic"] {
        std::fs::copy(fixtures().join(name), card.join(name)).expect("fixture");
    }
    let dry = cli.ok(&["import", "--dry-run", card.to_str().unwrap()]);
    assert!(dry.contains("3 new items"), "{dry}");
    assert!(dry.contains("Dry run"));
    let imported = cli.ok(&["import", card.to_str().unwrap()]);
    assert!(imported.contains("Imported 3 items"), "{imported}");
    assert!(cli.library.join("Media/2019/08/10/camera.jpg").is_file());

    // Thumbnails for every format, including HEIC through the helper process.
    let thumbs = cli.ok(&["thumbnails"]);
    assert!(thumbs.contains("up to date"), "{thumbs}");
    let health = cli.json(&["health"]);
    assert_eq!(health["stats"]["thumbs_failed"], 0, "{health:#}");
    assert_eq!(health["stats"]["thumbs_pending"], 0);

    let found = cli.json(&["search", "camera:nikon"]);
    let id = found[0]["id"].as_i64().expect("id").to_string();
    cli.ok(&["tag", "add", "Paris trip", &id]);
    cli.ok(&["set", "rating", "5", &id]);
    cli.ok(&["set", "shift", "-1h", &id]);
    let tagged = cli.json(&["search", "tag:\"paris trip\"", "rating:5"]);
    assert_eq!(tagged.as_array().map(Vec::len), Some(1));
    let shown = cli.ok(&["show", &id]);
    assert!(shown.contains("Paris, France"), "{shown}");
    assert!(shown.contains("2019-08-10 13:30"), "{shown}");

    let album = cli.ok(&["album", "create", "Best"]);
    let album_id = album
        .trim()
        .trim_start_matches("Created album #")
        .to_owned();
    cli.ok(&["album", "add", &album_id, &id]);
    assert!(cli.ok(&["album", "list"]).contains("Best"));

    let metadata = cli.home.path().join("meta.json");
    assert!(
        cli.ok(&["export", "metadata", metadata.to_str().unwrap()])
            .contains("1 items")
    );
    assert!(cli.ok(&["backup", "create"]).contains("Backed up"));
    assert!(
        cli.ok(&["review", "list"])
            .contains("Nothing needs your attention")
    );
    assert!(cli.ok(&["info"]).contains("3 photos"));
}

#[test]
fn errors_are_reported_plainly() {
    let cli = Cli::new();
    let output = cli.run(&["info"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("is not a MediaNest library"), "{message}");
}
