//! Package (.signifiage) tests: round trip and extraction safety.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use signifiage::model::new_id;
use signifiage::model::project::{self, Project, Template};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("signifiage-pkg-{tag}-{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_project(dir: &Path) -> Project {
    let project = Project::from_template(Template::StageShow, "Pkg");
    project::create(&project, dir).unwrap();
    fs::create_dir_all(dir.join("assets")).unwrap();
    fs::write(dir.join("assets/clip.mp4"), b"fake video").unwrap();
    fs::write(dir.join("assets/logo.png"), b"fake image").unwrap();
    project
}

#[test]
fn export_and_extract_round_trip_includes_all_assets() {
    let dir = temp_dir("export");
    sample_project(&dir);
    let out = dir.join("../exported.signifiage");
    let written = project::export_package(&dir, &out).unwrap();
    assert!(written.is_file());

    let dest = temp_dir("extract");
    project::extract_package(&written, &dest).unwrap();
    assert!(dest.join("project.json").is_file());
    assert_eq!(
        fs::read(dest.join("assets/clip.mp4")).unwrap(),
        b"fake video"
    );
    assert_eq!(
        fs::read(dest.join("assets/logo.png")).unwrap(),
        b"fake image"
    );

    let original = project::load(&dir).unwrap().project;
    let extracted = project::load(&dest).unwrap().project;
    assert_eq!(original, extracted);

    fs::remove_file(&out).ok();
    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&dest).ok();
}

#[test]
fn export_replaces_existing_package_atomically() {
    let dir = temp_dir("replace");
    sample_project(&dir);
    let out = dir.join("../replace.signifiage");
    fs::write(&out, b"old package").unwrap();
    project::export_package(&dir, &out).unwrap();
    assert!(zip_len(&out) >= 3);
    assert!(!out.with_extension("signifiage.tmp").exists());
    fs::remove_file(&out).ok();
    fs::remove_dir_all(&dir).ok();
}

fn zip_len(path: &Path) -> usize {
    let file = fs::File::open(path).unwrap();
    zip::ZipArchive::new(file).unwrap().len()
}

fn write_zip(path: &Path, entries: &[(&str, &[u8], Option<u32>)]) {
    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    for (name, body, mode) in entries {
        let mut options = zip::write::SimpleFileOptions::default();
        if let Some(mode) = mode {
            // unix_permissions masks to 0o777, so set the raw external attributes:
            // the file-type bits have to survive to model a symlink entry.
            options = options.external_attributes(mode << 16);
        }
        writer.start_file(*name, options).unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn extraction_rejects_parent_traversal() {
    let dir = temp_dir("traversal");
    let pkg = dir.join("evil.signifiage");
    write_zip(
        &pkg,
        &[
            ("project.json", b"{}", None),
            ("../escaped.txt", b"nope", None),
        ],
    );
    let dest = temp_dir("traversal-dest");
    let err = project::extract_package(&pkg, &dest)
        .unwrap_err()
        .to_string();
    assert!(err.contains("../escaped.txt"), "{err}");
    assert!(!dest.join("escaped.txt").exists());
    assert!(!dir.join("escaped.txt").exists());
    fs::remove_dir_all(&dest).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extraction_rejects_absolute_and_windows_paths() {
    for name in ["/etc/passwd", "C:/Windows/system32", "assets/../../x"] {
        let dir = temp_dir("absolute");
        let pkg = dir.join("evil.signifiage");
        write_zip(&pkg, &[("project.json", b"{}", None), (name, b"x", None)]);
        let dest = temp_dir("absolute-dest");
        assert!(
            project::extract_package(&pkg, &dest).is_err(),
            "entry {name} must be rejected"
        );
        fs::remove_dir_all(&dest).ok();
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn extraction_rejects_symlink_entries() {
    let dir = temp_dir("symlink");
    let pkg = dir.join("evil.signifiage");
    write_zip(
        &pkg,
        &[
            ("project.json", b"{}", None),
            ("assets/link", b"/etc/passwd", Some(0o120777)),
        ],
    );
    let dest = temp_dir("symlink-dest");
    let err = project::extract_package(&pkg, &dest)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("symlink") || err.contains("assets/link"),
        "{err}"
    );
    fs::remove_dir_all(&dest).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extraction_requires_project_json() {
    let dir = temp_dir("nojson");
    let pkg = dir.join("empty.signifiage");
    write_zip(&pkg, &[("assets/clip.mp4", b"x", None)]);
    let dest = temp_dir("nojson-dest");
    assert!(project::extract_package(&pkg, &dest).is_err());
    fs::remove_dir_all(&dest).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn sanitize_entry_path_accepts_only_normal_components() {
    assert_eq!(
        project::sanitize_entry_path("assets/clip.mp4"),
        Some(PathBuf::from("assets/clip.mp4"))
    );
    assert_eq!(
        project::sanitize_entry_path("./assets//clip.mp4"),
        Some(PathBuf::from("assets/clip.mp4"))
    );
    assert_eq!(project::sanitize_entry_path("../x"), None);
    assert_eq!(project::sanitize_entry_path("/x"), None);
    assert_eq!(project::sanitize_entry_path(""), None);
    assert_eq!(project::sanitize_entry_path("C:/x"), None);
    assert_eq!(project::sanitize_entry_path("assets/../../x"), None);
}

#[test]
fn demo_fixture_packages_and_extracts() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo-project");
    let work = temp_dir("demo");
    let copy = work.join("demo-project");
    copy_dir(&fixture, &copy);
    let out = work.join("demo.signifiage");
    project::export_package(&copy, &out).unwrap();

    let dest = work.join("extracted");
    project::extract_package(&out, &dest).unwrap();
    let loaded = project::load(&dest).unwrap();
    assert_eq!(loaded.project.queue.rows.len(), 3);
    fs::remove_dir_all(&work).ok();
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
