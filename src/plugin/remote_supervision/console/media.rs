//! Session-scoped private audio for the admin console, using the selected
//! station's LibraryService. No public media URL and no broadcast action.
use super::super::web::{answer, cookie};
use super::{Reservation, Web, SESSION};
use crate::proto::library::{self, MediaPreviewRequest};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::{io, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{ClientTlsConfig, Endpoint};

pub(super) fn routes() -> Router<Web> {
    Router::new().route("/api/stations/:id/console/:console/media/*path", get(audio))
}
fn range(value: Option<&str>) -> Result<MediaPreviewRequest, StatusCode> {
    let mut r = MediaPreviewRequest::default();
    let Some(value) = value else { return Ok(r) };
    let (a, b) = value
        .strip_prefix("bytes=")
        .and_then(|s| s.split_once('-'))
        .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;
    let number = |s: &str| -> Result<u64, StatusCode> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        s.parse().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)
    };
    if a.is_empty() {
        r.suffix_length = number(b)?;
        if r.suffix_length == 0 {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
    } else {
        r.offset = number(a)?;
        if !b.is_empty() {
            let end = number(b)?;
            r.length = end
                .checked_sub(r.offset)
                .and_then(|n| n.checked_add(1))
                .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;
        }
    }
    Ok(r)
}
pub(super) fn entry(
    console: &super::Console,
    id: &str,
    session: &str,
    station: &str,
) -> Result<Reservation, String> {
    if !console.settings.enabled {
        return Err("permission denied".into());
    }
    let registry = console.registry.lock().map_err(|_| "busy")?;
    registry
        .entries
        .get(id)
        .filter(|e| {
            !registry.stopping
                && e.active
                && e.session == session
                && e.station == station
                && !*e.cancel.borrow()
        })
        .cloned()
        .ok_or_else(|| "permission denied".into())
}
fn rpc_error(error: tonic::Status) -> Response {
    let status = match error.code() {
        tonic::Code::NotFound => StatusCode::NOT_FOUND,
        tonic::Code::OutOfRange => StatusCode::RANGE_NOT_SATISFIABLE,
        tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
        tonic::Code::ResourceExhausted => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    };
    let mut response = status.into_response();
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        if let Some(size) = error
            .metadata()
            .get("media-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{size}").parse().unwrap(),
            );
        }
    }
    response
}
async fn audio(
    State(web): State<Web>,
    Path((station, id, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    let entry = match entry(&web.console, &id, &raw, &station) {
        Ok(entry) => entry,
        Err(e) => return answer(Err(e)),
    };
    let check = entry.clone();
    if let Err(e) = web
        .work(move |auth| {
            auth.console_owner(&check.session, &check.station, &check.csrf)
                .map(|_| ())
        })
        .await
    {
        return answer(Err(e));
    }
    let Ok(permit) = entry.preview_slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let range_header = match headers.get(header::RANGE) {
        Some(v) => match v.to_str() {
            Ok(v) => Some(v),
            Err(_) => return StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
        },
        None => None,
    };
    let mut request = match range(range_header) {
        Ok(r) => r,
        Err(status) => return status.into_response(),
    };
    request.rel_path = path;
    let Some(endpoint) = web.console.endpoints.get(&station) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut endpoint = match Endpoint::from_shared(endpoint.clone()) {
        Ok(e) => e.connect_timeout(Duration::from_secs(5)),
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    if endpoint.uri().scheme_str() == Some("https") {
        endpoint = match endpoint.tls_config(ClientTlsConfig::new().with_native_roots()) {
            Ok(e) => e,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
    }
    let channel = match endpoint.connect().await {
        Ok(c) => c,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let mut client = library::library_service_client::LibraryServiceClient::new(channel)
        .max_decoding_message_size(131072);
    let response =
        match tokio::time::timeout(Duration::from_secs(10), client.preview_audio(request)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return rpc_error(e),
            Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
        };
    let field = |name| {
        response
            .metadata()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let (Some(size), Some(offset), Some(length)) = (
        field("media-size"),
        field("media-offset"),
        field("media-length"),
    ) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if length == 0 || offset.checked_add(length).is_none_or(|end| end > size) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut stream = response.into_inner();
    let mut cancel = entry.cancel.subscribe();
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, io::Error>>(2);
    tokio::spawn(async move {
        let _permit = permit;
        loop {
            if *cancel.borrow() {
                break;
            }
            let next = tokio::select! {_=tx.closed()=>break,_=cancel.changed()=>break,r=tokio::time::timeout(Duration::from_secs(10),stream.message())=>r};
            let data = match next {
                Ok(Ok(Some(c))) => Ok(bytes::Bytes::from(c.data)),
                Ok(Ok(None)) => break,
                _ => Err(io::Error::other("media preview interrupted")),
            };
            let failed = data.is_err();
            let sent =
                tokio::select! {_=cancel.changed()=>false,result=tx.send(data)=>result.is_ok()};
            if !sent || failed {
                break;
            }
        }
    });
    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    *response.status_mut() = if range_header.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, "audio/mpeg".parse().unwrap());
    h.insert(header::CONTENT_LENGTH, length.to_string().parse().unwrap());
    h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store, private".parse().unwrap());
    if range_header.is_some() {
        h.insert(
            header::CONTENT_RANGE,
            format!("bytes {offset}-{}/{size}", offset + length - 1)
                .parse()
                .unwrap(),
        );
    }
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webmin_media_preview_range_parser_is_bounded() {
        let r = range(Some("bytes=12-20")).unwrap();
        assert_eq!((r.offset, r.length), (12, 9));
        let r = range(Some("bytes=4-")).unwrap();
        assert_eq!((r.offset, r.length), (4, 0));
        assert_eq!(range(Some("bytes=-80")).unwrap().suffix_length, 80);
        for value in [
            "bytes=8-2",
            "bytes=0-1,3-4",
            "bytes=-0",
            "bytes=-",
            "bytes=0-18446744073709551615",
            "units=0-1",
        ] {
            assert!(range(Some(value)).is_err(), "{value}");
        }
    }
}
