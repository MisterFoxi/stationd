//! Narrow gateway: authenticated Station.Status only; no station mutations.
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use sha2::{Digest, Sha256};
use stationd_proto::station::{station_client::StationClient, StatusRequest};
use stationdeck_services::{StationStatus, StatusResponse};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tonic::transport::{ClientTlsConfig, Endpoint};

#[derive(Clone)]
pub struct Gateway {
    token_digest: [u8; 32],
    endpoint: Option<Endpoint>,
}

impl Gateway {
    pub fn new(token: &str, address: Option<&str>) -> Result<Self, &'static str> {
        if token.len() < 32 || token.len() > 512 || token.chars().any(char::is_control) {
            return Err("Gateway token must contain 32 to 512 printable characters.");
        }
        let endpoint = address
            .map(|address| {
                let mut endpoint = Endpoint::from_shared(address.to_owned())
                    .map_err(|_| "Invalid gRPC target configuration.")?;
                let uri = endpoint.uri();
                if !matches!(uri.scheme_str(), Some("http" | "https"))
                    || uri
                        .authority()
                        .is_none_or(|authority| authority.as_str().contains('@'))
                    || uri.path() != "/"
                    || uri.query().is_some()
                {
                    return Err("Invalid gRPC target configuration.");
                }
                if uri.scheme_str() == Some("https") {
                    endpoint = endpoint
                        .tls_config(ClientTlsConfig::new().with_native_roots())
                        .map_err(|_| "Invalid gRPC TLS configuration.")?;
                }
                Ok(endpoint
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(3)))
            })
            .transpose()?;
        Ok(Self {
            token_digest: Sha256::digest(token.as_bytes()).into(),
            endpoint,
        })
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/api/station/status", get(status))
            .with_state(self)
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(token) = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return false;
        };
        if token.len() > 512 {
            return false;
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        bool::from(self.token_digest.ct_eq(&digest))
    }

    async fn read(&self) -> Result<StationStatus, (&'static str, &'static str)> {
        let Some(endpoint) = &self.endpoint else {
            return Err((
                "not_configured",
                "No station connection is configured on the server.",
            ));
        };
        let channel = endpoint
            .connect()
            .await
            .map_err(|_| ("unavailable", "StationD could not be reached."))?;
        let reply = StationClient::new(channel)
            .max_decoding_message_size(64 * 1024)
            .status(StatusRequest {})
            .await
            .map_err(|error| match error.code() {
                tonic::Code::Unauthenticated => (
                    "unauthenticated",
                    "StationD rejected the gateway credentials.",
                ),
                tonic::Code::PermissionDenied => {
                    ("permission_denied", "StationD denied access to its status.")
                }
                tonic::Code::DeadlineExceeded => {
                    ("timeout", "The StationD status request timed out.")
                }
                _ => ("unavailable", "The StationD status request failed."),
            })?
            .into_inner();
        Ok(StationStatus {
            station_name: reply.station_name.chars().take(256).collect(),
            uptime_seconds: reply.uptime_seconds,
            pid: reply.pid,
            timezone: reply.timezone.chars().take(64).collect(),
        })
    }
}

async fn status(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
) -> (StatusCode, Json<StatusResponse>) {
    if !gateway.authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(StatusResponse::Unavailable {
                code: "unauthenticated".into(),
                message: "A valid gateway access token is required.".into(),
            }),
        );
    }
    let result = tokio::time::timeout(Duration::from_secs(4), gateway.read())
        .await
        .unwrap_or(Err(("timeout", "The station connection timed out.")));
    // An authenticated probe can legitimately report an unavailable station.
    let response = match result {
        Ok(status) => StatusResponse::Connected { status },
        Err((code, message)) => StatusResponse::Unavailable {
            code: code.into(),
            message: message.into(),
        },
    };
    (StatusCode::OK, Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    const TOKEN: &str = "fixture-token-0123456789-0123456789";

    #[tokio::test]
    async fn status_is_denied_by_default_and_mutations_are_absent() {
        let router = Gateway::new(TOKEN, None).unwrap().router();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/station/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        for method in ["POST", "DELETE"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/api/station/status")
                        .header("Authorization", format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/station/quit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn configured_access_reports_missing_station_without_faking_a_connection() {
        let router = Gateway::new(TOKEN, None).unwrap().router();
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/station/status")
                    .header("Authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let result: StatusResponse = serde_json::from_slice(&body).unwrap();
        assert!(
            matches!(result, StatusResponse::Unavailable { code, .. } if code == "not_configured")
        );
        assert!(!String::from_utf8_lossy(&body).contains(TOKEN));
    }

    #[test]
    fn target_credentials_and_non_grpc_urls_are_rejected() {
        for target in [
            "file:///etc/passwd",
            "http://user:secret@localhost:50051",
            "http://localhost:50051/path",
            "http://localhost:50051/?token=secret",
        ] {
            assert!(Gateway::new(TOKEN, Some(target)).is_err());
        }
    }
}
