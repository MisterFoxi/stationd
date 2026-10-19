//! Explicit management endpoint. Read tokens never authorize this route.
use crate::{icecast::IcecastTarget, Gateway};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use stationdeck_services::icecast::{ControlResponse, IcecastCommand};
use std::time::Duration;
impl Gateway {
    pub(crate) async fn icecast_target(
        &self,
    ) -> Result<IcecastTarget, (&'static str, &'static str)> {
        if let Some(target) = &self.icecast {
            return Ok(target.clone());
        }
        let policy = self.services.as_ref().ok_or((
            "not_configured",
            "Icecast service discovery is not configured.",
        ))?;
        let resolved = policy
            .resolve("stationdeck.icecast.streams")
            .await
            .map_err(|e| (e.code(), e.message()))?;
        let token = resolved
            .credential_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .ok_or((
                "credentials_missing",
                "The service credential is not provisioned.",
            ))?;
        IcecastTarget::with_tls_name(
            &resolved.endpoint,
            token.trim(),
            resolved.tls_server_name.as_deref(),
        )
        .map_err(|_| {
            (
                "invalid_configuration",
                "Invalid service transport or credentials.",
            )
        })
    }
}
pub(crate) async fn command(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    Json(command): Json<IcecastCommand>,
) -> (StatusCode, Json<ControlResponse>) {
    if !gateway.control_authorized(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(ControlResponse::Unavailable {
                code: "forbidden".into(),
                message: "An Icecast control token is required.".into(),
            }),
        );
    }
    if let Err(message) = command.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ControlResponse::Unavailable {
                code: "invalid_request".into(),
                message: message.into(),
            }),
        );
    }
    let result = tokio::time::timeout(Duration::from_secs(9), async {
        gateway.icecast_target().await?.control(command).await
    })
    .await
    .unwrap_or(Err((
        "timeout",
        "Operation timed out; its outcome may be unknown. Refresh before retrying.",
    )));
    match result {
        Ok(result) => (StatusCode::OK, Json(ControlResponse::Completed { result })),
        Err((code, message)) => (
            StatusCode::BAD_GATEWAY,
            Json(ControlResponse::Unavailable {
                code: code.into(),
                message: message.into(),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::Gateway;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    const READ: &str = "read-fixture-012345678901234567890123456789";
    const CONTROL: &str = "control-fixture-012345678901234567890123456789";
    #[tokio::test]
    async fn read_credentials_cannot_authorize_management() {
        let app = Gateway::new(READ, None)
            .unwrap()
            .with_control_token(CONTROL)
            .unwrap()
            .router();
        for token in [None, Some(READ), Some("invalid-fixture")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/icecast/control")
                .header("content-type", "application/json");
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let result = app
                .clone()
                .oneshot(request.body(Body::from(r#"{"action":"route"}"#)).unwrap())
                .await
                .unwrap();
            assert_eq!(result.status(), StatusCode::FORBIDDEN);
        }
        assert!(Gateway::new(READ, None)
            .unwrap()
            .with_control_token(READ)
            .is_err());
    }
    #[tokio::test]
    async fn invalid_command_is_rejected_before_resolving_an_upstream() {
        let app = Gateway::new(READ, None)
            .unwrap()
            .with_control_token(CONTROL)
            .unwrap()
            .router();
        let result = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/icecast/control")
                    .header("authorization", format!("Bearer {CONTROL}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"action":"kick_source","mount":"http://arbitrary/"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
    }
}
