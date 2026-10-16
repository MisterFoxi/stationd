//! Private audio audition: indexed files only, read-only, with bounded streaming.
use crate::{
    library_actor::{LibraryError, LibraryHandle},
    media_tags::TagError,
    proto::library,
};
use std::{path::Path, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};

pub(crate) struct PreviewFile {
    file: std::fs::File,
    size: u64,
    offset: u64,
    length: u64,
}
fn bounds(size: u64, r: &library::MediaPreviewRequest) -> Result<(u64, u64), LibraryError> {
    if size == 0 {
        return Err(LibraryError::AudioRange { size });
    }
    if r.suffix_length > 0 {
        if r.offset != 0 || r.length != 0 {
            return Err(LibraryError::BadFilter(
                "incompatible audio range fields".into(),
            ));
        }
        let length = r.suffix_length.min(size);
        return Ok((size - length, length));
    }
    if size == 0 || r.offset >= size {
        return Err(LibraryError::AudioRange { size });
    }
    let remaining = size - r.offset;
    Ok((
        r.offset,
        if r.length == 0 {
            remaining
        } else {
            r.length.min(remaining)
        },
    ))
}
pub(crate) async fn open(
    pool: &sqlx::SqlitePool,
    root: &Path,
    request: library::MediaPreviewRequest,
) -> Result<PreviewFile, LibraryError> {
    let path = &request.rel_path;
    let missing = || LibraryError::Tags(TagError::NotFound("selected media".into()));
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\\', '\0'])
        || !Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        || !Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
    {
        return Err(missing());
    }
    if !crate::media_index::row(pool, path)
        .await?
        .is_some_and(|m| m.available)
    {
        return Err(missing());
    }
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let not_found = || LibraryError::Tags(TagError::NotFound("selected media".into()));
        let root = root.canonicalize().map_err(|_| not_found())?;
        let full = root
            .join(&request.rel_path)
            .canonicalize()
            .map_err(|_| not_found())?;
        if !full.starts_with(&root) {
            return Err(not_found());
        }
        let file = std::fs::File::open(full).map_err(|_| not_found())?;
        let metadata = file.metadata().map_err(|_| not_found())?;
        if !metadata.is_file() {
            return Err(not_found());
        }
        let (offset, length) = bounds(metadata.len(), &request)?;
        Ok(PreviewFile {
            file,
            size: metadata.len(),
            offset,
            length,
        })
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))?
}
pub(crate) async fn stream(
    handle: &LibraryHandle,
    request: library::MediaPreviewRequest,
    mut stopping: Option<watch::Receiver<bool>>,
    slots: Arc<Semaphore>,
) -> Result<Response<ReceiverStream<Result<library::MediaPreviewChunk, Status>>>, Status> {
    let permit = slots
        .try_acquire_owned()
        .map_err(|_| Status::resource_exhausted("too many media previews"))?;
    let opened = handle
        .preview_audio(request)
        .await
        .map_err(crate::library_grpc::map_error)?;
    let (size, offset, length) = (opened.size, opened.offset, opened.length);
    let mut file = tokio::fs::File::from_std(opened.file);
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|_| Status::unavailable("media unavailable"))?;
    let (tx, rx) = mpsc::channel(2);
    tokio::spawn(async move {
        let _permit = permit;
        let mut remaining = length;
        let mut buffer = vec![0u8; 65536];
        while remaining > 0 {
            let cap = remaining.min(buffer.len() as u64) as usize;
            let shutdown = async {
                match &mut stopping {
                    Some(rx) => {
                        let _ = rx.wait_for(|stop| *stop).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let read = tokio::select! { _=tx.closed()=>break, _=shutdown=>break, read=file.read(&mut buffer[..cap])=>read };
            let value = match read {
                Ok(0) => Err(Status::aborted("media changed during preview")),
                Ok(n) => {
                    remaining -= n as u64;
                    Ok(library::MediaPreviewChunk {
                        data: buffer[..n].to_vec(),
                    })
                }
                Err(_) => Err(Status::unavailable("cannot read selected media")),
            };
            let failed = value.is_err();
            let shutdown = async {
                match &mut stopping {
                    Some(rx) => {
                        let _ = rx.wait_for(|stop| *stop).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let sent = tokio::select! {_=shutdown=>false,result=tx.send(value)=>result.is_ok()};
            if !sent || failed {
                break;
            }
        }
    });
    let mut response = Response::new(ReceiverStream::new(rx));
    for (key, value) in [
        ("media-size", size),
        ("media-offset", offset),
        ("media-length", length),
    ] {
        response
            .metadata_mut()
            .insert(key, value.to_string().parse().unwrap());
    }
    Ok(response)
}
#[cfg(test)]
#[path = "media_preview_tests.rs"]
mod tests;
