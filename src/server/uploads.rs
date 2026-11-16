//! Multipart uploads from the web remote.
//!
//! Files stream to `assets/.upload-<uuid>`, are renamed atomically into
//! `assets/<sanitised-name>` with a `-2`, `-3`, … suffix on collision, and are
//! announced on the action channel so the desktop asset list refreshes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::Multipart;
use axum::extract::multipart::Field;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::io::AsyncWriteExt;

use crate::model::project::{self, Project};
use crate::server::actions::RemoteAction;
use crate::server::routes::{AppState, json_error};
use crate::services::assets;

/// Refuse bodies larger than 4 GiB (the client gets 413).
const MAX_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Fields are `file`, maybe a few extra; stop runaway multipart bodies.
const MAX_FIELDS: usize = 16;

/// Extensions accepted by `/api/upload` (plan list, lowercase).
const IMAGE_EXTENSIONS: [&str; 9] = [
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "svg", "tif", "tiff",
];
const VIDEO_EXTENSIONS: [&str; 7] = ["mp4", "webm", "mkv", "mov", "avi", "m4v", "ts"];
const AUDIO_EXTENSIONS: [&str; 8] = ["wav", "mp3", "ogg", "oga", "flac", "m4a", "aac", "opus"];

fn extension(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

/// True when the (already sanitised) file name carries an allowed extension.
fn allowed_extension(name: &str) -> bool {
    let Some(extension) = extension(name) else {
        return false;
    };
    IMAGE_EXTENSIONS.contains(&extension.as_str())
        || VIDEO_EXTENSIONS.contains(&extension.as_str())
        || AUDIO_EXTENSIONS.contains(&extension.as_str())
}

pub(crate) async fn handle(state: Arc<AppState>, mut multipart: Multipart) -> Response {
    let Some(project_dir) = state.shared.project_dir() else {
        return json_error(StatusCode::CONFLICT, "no project is open");
    };

    for _ in 0..MAX_FIELDS {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("file") {
                    return process_field(state, project_dir, field).await;
                }
                // Other fields are ignored; dropping them drains their part.
            }
            Ok(None) => {
                return json_error(StatusCode::BAD_REQUEST, "missing multipart field \"file\"");
            }
            Err(error) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    &format!("invalid multipart body: {error}"),
                );
            }
        }
    }
    json_error(StatusCode::BAD_REQUEST, "too many multipart fields")
}

async fn process_field(
    state: Arc<AppState>,
    project_dir: PathBuf,
    mut field: Field<'_>,
) -> Response {
    let raw_name = field.file_name().unwrap_or_default().to_string();
    if raw_name.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "upload has no file name");
    }
    let sanitized = assets::sanitize_file_name(&raw_name);
    if !allowed_extension(&sanitized) {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "file type is not allowed",
        );
    }

    let assets_dir = Project::asset_dir(&project_dir);
    if let Err(error) = tokio::fs::create_dir_all(&assets_dir).await {
        tracing::warn!("cannot create assets directory: {error}");
        return internal_error();
    }
    let tmp_path = assets_dir.join(format!(".upload-{}", uuid::Uuid::new_v4()));
    let mut file = match tokio::fs::File::create(&tmp_path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!("cannot create upload temp file: {error}");
            return internal_error();
        }
    };

    let mut total: u64 = 0;
    let mut shutdown = state.shutdown.clone();
    loop {
        let chunk = tokio::select! {
            _ = shutdown.changed() => {
                drop(file);
                cleanup(&tmp_path).await;
                return json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the server is shutting down",
                );
            }
            chunk = field.chunk() => match chunk {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(error) => {
                    drop(file);
                    cleanup(&tmp_path).await;
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        &format!("upload was interrupted: {error}"),
                    );
                }
            },
        };
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_UPLOAD_BYTES {
            drop(file);
            cleanup(&tmp_path).await;
            return json_error(StatusCode::PAYLOAD_TOO_LARGE, "upload exceeds 4 GiB");
        }
        if let Err(error) = file.write_all(&chunk).await {
            tracing::warn!("cannot write upload: {error}");
            drop(file);
            cleanup(&tmp_path).await;
            return internal_error();
        }
    }
    if let Err(error) = file.flush().await {
        tracing::warn!("cannot flush upload: {error}");
        drop(file);
        cleanup(&tmp_path).await;
        return internal_error();
    }
    drop(file);

    // One upload at a time: the stored name and the rename must not race.
    let _guard = state.upload_lock.lock().await;
    let stored = match store(&project_dir, &assets_dir, &tmp_path, &sanitized) {
        Ok(name) => name,
        Err(error) => {
            tracing::warn!("cannot store upload: {error}");
            cleanup(&tmp_path).await;
            return internal_error();
        }
    };

    let asset = format!("assets/{stored}");
    let announced = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.sender.send(RemoteAction::AssetUploaded {
            asset: asset.clone(),
        }),
    )
    .await;
    if !matches!(announced, Ok(Ok(()))) {
        tracing::warn!(%asset, "uploaded asset was not announced to the desktop UI");
    }
    tracing::info!(%asset, "remote upload stored");

    let body = serde_json::json!({ "ok": true, "asset": asset }).to_string();
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

/// Link the temp file to a fresh name (never overwriting), falling back to a
/// rename on filesystems without hard links. Returns the stored file name.
fn store(
    project_dir: &Path,
    assets_dir: &Path,
    tmp_path: &PathBuf,
    sanitized: &str,
) -> io::Result<String> {
    let mut name = project::unique_asset_name(project_dir, sanitized);
    for _ in 0..64 {
        let target = assets_dir.join(&name);
        match std::fs::hard_link(tmp_path, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(tmp_path);
                return Ok(name);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                name = project::unique_asset_name(project_dir, sanitized);
            }
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                // FAT/exotic mounts: rename is still atomic, and `name` was
                // free an instant ago.
                std::fs::rename(tmp_path, &target)?;
                return Ok(name);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("cannot find a free asset name"))
}

async fn cleanup(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

fn internal_error() -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "cannot store the uploaded file",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_allowlist_matches_the_plan() {
        for name in [
            "photo.png",
            "photo.JPG",
            "art.svg",
            "clip.mp4",
            "clip.webm",
            "clip.ts",
            "beep.wav",
            "song.FLAC",
            "voice.opus",
        ] {
            assert!(allowed_extension(name), "{name} should be allowed");
        }
        for name in ["notes.txt", "run.sh", "sheet.xlsx", "noext", ".hidden"] {
            assert!(!allowed_extension(name), "{name} should be rejected");
        }
    }

    #[test]
    fn sanitized_names_are_path_free_and_lowercase() {
        assert_eq!(
            assets::sanitize_file_name("../../My Clip.MP4"),
            "My_Clip.mp4"
        );
        assert_eq!(assets::sanitize_file_name(".."), "asset");
        assert_eq!(assets::sanitize_file_name("C:\\temp\\a b.png"), "a_b.png");
        assert!(allowed_extension(&assets::sanitize_file_name(
            "evil name.PNG"
        )));
    }
}
