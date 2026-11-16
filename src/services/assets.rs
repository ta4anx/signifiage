//! Asset library: the `assets/` directory of the open project.
//!
//! The library is derived by scanning the directory; `project.json` stores only
//! relative references (`assets/<name>`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::project::{self, Project};

/// Media kind inferred from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Video,
    Audio,
    Other,
}

pub const IMAGE_EXTENSIONS: [&str; 9] = [
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "svg", "tif", "tiff",
];
pub const VIDEO_EXTENSIONS: [&str; 8] = ["mp4", "webm", "mkv", "mov", "avi", "m4v", "ts", "ogv"];
pub const AUDIO_EXTENSIONS: [&str; 9] = [
    "wav", "mp3", "ogg", "oga", "flac", "m4a", "aac", "opus", "wma",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// Path stored in the project, e.g. `assets/clip.mp4`.
    pub rel: String,
    /// File name only.
    pub name: String,
    pub kind: AssetKind,
    pub bytes: u64,
}

pub fn kind_of(name: &str) -> AssetKind {
    let extension = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        AssetKind::Image
    } else if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        AssetKind::Video
    } else if AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        AssetKind::Audio
    } else {
        AssetKind::Other
    }
}

/// True when the extension is one the app can import.
pub fn is_supported(name: &str) -> bool {
    kind_of(name) != AssetKind::Other
}

/// Scan `<project>/assets`, sorted by name.
pub fn list(project_dir: &Path) -> Vec<AssetEntry> {
    let dir = Project::asset_dir(project_dir);
    let mut entries = Vec::new();
    let Ok(read) = fs::read_dir(&dir) else {
        return entries;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        entries.push(AssetEntry {
            rel: format!("assets/{name}"),
            kind: kind_of(&name),
            bytes: meta.len(),
            name,
        });
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries
}

/// Sanitise an uploaded or imported file name.
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_string();
    let replaced: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let replaced = replaced.replace(' ', "_");
    let trimmed = replaced.trim_matches('_').to_string();
    let stem = Path::new(&trimmed)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || trimmed.is_empty()
        || trimmed.starts_with('.')
    {
        return match Path::new(&trimmed).extension() {
            Some(ext) => format!("asset.{}", ext.to_string_lossy().to_lowercase()),
            None => "asset".to_string(),
        };
    }
    match Path::new(&trimmed).extension() {
        Some(ext) => format!("{stem}.{}", ext.to_string_lossy().to_lowercase()),
        None => stem,
    }
}

/// Copy one file into `assets/` with a collision suffix; returns the stored path.
pub fn import_file(project_dir: &Path, source: &Path) -> anyhow::Result<String> {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("source has no file name"))?;
    let sanitized = sanitize_file_name(&name);
    let assets = Project::asset_dir(project_dir);
    fs::create_dir_all(&assets)?;
    let target_name = project::unique_asset_name(project_dir, &sanitized);
    let tmp = assets.join(format!(".import-{}", uuid::Uuid::new_v4()));
    fs::copy(source, &tmp)?;
    fs::rename(&tmp, assets.join(&target_name))?;
    Ok(format!("assets/{target_name}"))
}

/// Import many files, skipping unsupported ones. Returns stored paths.
pub fn import_files(project_dir: &Path, sources: &[PathBuf]) -> (Vec<String>, Vec<String>) {
    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    for source in sources {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !is_supported(&name) {
            skipped.push(name);
            continue;
        }
        match import_file(project_dir, source) {
            Ok(rel) => imported.push(rel),
            Err(err) => {
                tracing::warn!("cannot import {}: {err}", source.display());
                skipped.push(name);
            }
        }
    }
    (imported, skipped)
}

/// True when a scene layer or soundboard pad references this asset path.
pub fn is_referenced(project: &Project, rel: &str) -> bool {
    let layers = project
        .scenes
        .iter()
        .flat_map(|scene| scene.layers.iter())
        .any(|layer| layer.kind.asset() == Some(rel));
    let pads = project
        .soundboard
        .pages
        .iter()
        .flat_map(|page| page.pads.iter())
        .any(|pad| pad.asset.as_deref() == Some(rel));
    layers || pads
}

/// Delete an asset file; refuses while referenced.
pub fn remove(project_dir: &Path, rel: &str, project: &Project) -> anyhow::Result<()> {
    if is_referenced(project, rel) {
        anyhow::bail!("asset is in use");
    }
    let path = project_dir.join(rel);
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("signifiage-assets-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(Project::asset_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn kinds_follow_extensions() {
        assert_eq!(kind_of("a.PNG"), AssetKind::Image);
        assert_eq!(kind_of("clip.webm"), AssetKind::Video);
        assert_eq!(kind_of("beep.wav"), AssetKind::Audio);
        assert_eq!(kind_of("notes.txt"), AssetKind::Other);
        assert!(is_supported("photo.jpeg"));
        assert!(!is_supported("archive.zip"));
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("My Clip.MP4"), "My_Clip.mp4");
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("/abs/path/a:b.png"), "a_b.png");
        assert_eq!(sanitize_file_name("weird*.png"), "weird_.png");
        assert_eq!(sanitize_file_name(".."), "asset");
        assert_eq!(sanitize_file_name(""), "asset");
        assert_eq!(sanitize_file_name(".png"), "asset");
    }

    #[test]
    fn import_copies_and_dedupes() {
        let dir = temp("import");
        let source = temp("source").join("clip.mp4");
        fs::write(&source, b"video").unwrap();
        let first = import_file(&dir, &source).unwrap();
        assert_eq!(first, "assets/clip.mp4");
        let second = import_file(&dir, &source).unwrap();
        assert_eq!(second, "assets/clip-2.mp4");
        assert_eq!(list(&dir).len(), 2);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(source.parent().unwrap()).ok();
    }

    #[test]
    fn remove_refuses_referenced_assets() {
        let dir = temp("referenced");
        fs::write(dir.join("assets/logo.png"), b"png").unwrap();
        let mut project = Project::from_template(crate::model::Template::Blank, "P");
        let layer = crate::model::Layer::new(
            crate::model::LayerKind::Image(crate::model::ImageProps {
                asset: Some("assets/logo.png".into()),
                ..Default::default()
            }),
            "img",
            crate::model::Rect::new(0.0, 0.0, 10.0, 10.0),
        );
        project.scenes[0].layers.push(layer);
        assert!(is_referenced(&project, "assets/logo.png"));
        assert!(remove(&dir, "assets/logo.png", &project).is_err());
        project.scenes[0].layers.clear();
        remove(&dir, "assets/logo.png", &project).unwrap();
        assert!(!dir.join("assets/logo.png").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_files_are_skipped_on_import() {
        let dir = temp("skip");
        let source = temp("skip-src");
        let good = source.join("beep.wav");
        let bad = source.join("notes.txt");
        fs::write(&good, b"wav").unwrap();
        fs::write(&bad, b"txt").unwrap();
        let (imported, skipped) = import_files(&dir, &[good, bad]);
        assert_eq!(imported, vec!["assets/beep.wav".to_string()]);
        assert_eq!(skipped, vec!["notes.txt".to_string()]);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&source).ok();
    }
}
