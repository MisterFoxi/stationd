use axum::{body::Body, http::Request};
use stationdeck_control::{
    server,
    store::{Bootstrap, Store},
    wire::{
        self, configuration_client::ConfigurationClient, configuration_server::ConfigurationServer,
    },
};
use stationdeck_services::config::*;
use std::fs;
use tower::ServiceExt;
fn fixture() -> (tempfile::TempDir, Bootstrap) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("authors")).unwrap();
    fs::create_dir_all(dir.path().join("agents/demo-1")).unwrap();
    for (name, token) in [
        ("authors/editor", EDITOR),
        ("authors/reader", READER),
        ("agents/demo-1/token", AGENT),
    ] {
        fs::write(dir.path().join(name), token).unwrap();
    }
    let bootstrap: Bootstrap =
        serde_json::from_str(include_str!("../../../config/stationdeck/catalog.json")).unwrap();
    (dir, bootstrap)
}
const EDITOR: &str = "editor-only-fixture-01234567890123456789";
const READER: &str = "reader-only-fixture-01234567890123456789";
const AGENT: &str = "agent-only-fixture-012345678901234567890";
fn documents(message: &str) -> Vec<Document> {
    vec![Document {
        plugin: "demo.settings".into(),
        instance: "default".into(),
        version: 1,
        values: [
            ("message".into(), serde_json::json!(message)),
            ("interval_seconds".into(), serde_json::json!(3)),
        ]
        .into(),
    }]
}
fn publish(store: &Store, message: &str) -> TargetView {
    let v = store.view("demo-1").unwrap();
    let v = store
        .save(
            "demo-1",
            DraftRequest {
                base_draft: v.draft_version,
                documents: documents(message),
            },
        )
        .unwrap();
    store
        .publish(
            "demo-1",
            PublishRequest {
                draft_version: v.draft_version,
                base_revision: v.published.as_ref().map_or(0, |b| b.revision),
            },
        )
        .unwrap()
}
#[test]
fn revisions_conflicts_restart_and_rollback_are_durable() {
    let (dir, bootstrap) = fixture();
    let path = dir.path().join("state.json");
    let store = Store::open(bootstrap.clone(), &path, dir.path()).unwrap();
    assert!(
        Store::open(bootstrap.clone(), &path, dir.path()).is_err(),
        "single writer lock"
    );
    let first = publish(&store, "first").published.unwrap();
    let second = publish(&store, "second").published.unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(
        store
            .save(
                "demo-1",
                DraftRequest {
                    base_draft: 0,
                    documents: documents("stale")
                }
            )
            .unwrap_err()
            .0,
        409
    );
    assert_eq!(
        store
            .publish(
                "demo-1",
                PublishRequest {
                    draft_version: 2,
                    base_revision: 0
                }
            )
            .unwrap_err()
            .0,
        409
    );
    store.report("demo-1", 2, true, "applied").unwrap();
    assert!(store.report("demo-1", 1, true, "applied").is_err());
    assert!(store.report("demo-1", 3, true, "applied").is_err());
    let rollback = publish(&store, "first");
    assert_eq!(rollback.published.unwrap().revision, 3);
    assert_eq!(rollback.history[0], first);
    store.report("demo-1", 3, false, "apply_failed").unwrap();
    assert_eq!(store.view("demo-1").unwrap().applied_revision, 2);
    drop(store);
    let store = Store::open(bootstrap, &path, dir.path()).unwrap();
    assert_eq!(store.view("demo-1").unwrap().history.len(), 3);
    assert_eq!(store.view("demo-1").unwrap().report.unwrap().revision, 3);
    assert_eq!(store.view("demo-1").unwrap().applied_revision, 2);
}
#[test]
fn invalid_schema_dependencies_and_corrupt_store_are_not_applied() {
    let (dir, bootstrap) = fixture();
    let path = dir.path().join("state.json");
    let store = Store::open(bootstrap.clone(), &path, dir.path()).unwrap();
    let mut wrong_version = documents("test");
    wrong_version[0].version = 999;
    let mut unauthorized = documents("test");
    unauthorized[0].plugin = "foreign.plugin".into();
    let mut missing = documents("test");
    missing[0].values.remove("message");
    for docs in [wrong_version, unauthorized, missing] {
        assert_eq!(
            store
                .save(
                    "demo-1",
                    DraftRequest {
                        base_draft: 0,
                        documents: docs
                    }
                )
                .unwrap_err()
                .0,
            422
        );
    }
    let mut docs = documents("test");
    docs[0]
        .values
        .insert("interval_seconds".into(), serde_json::json!(0));
    assert!(store
        .save(
            "demo-1",
            DraftRequest {
                base_draft: 0,
                documents: docs
            }
        )
        .is_err());
    let mut catalog = bootstrap.clone();
    catalog.schemas[0].requires.push("required.plugin".into());
    assert!(validate(
        &catalog.schemas,
        &["demo.settings".into()],
        &documents("test")
    )
    .is_err());
    let mut docs = documents("test");
    docs[0]
        .values
        .insert("extra".into(), serde_json::json!(true));
    assert!(validate(&bootstrap.schemas, &["demo.settings".into()], &docs).is_err());
    drop(store);
    fs::write(&path, b"not-json").unwrap();
    assert!(Store::open(bootstrap, &path, dir.path()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not-json");
}
#[tokio::test]
async fn http_permissions_and_agent_identity_cannot_be_forged() {
    let (dir, bootstrap) = fixture();
    let store = Store::open(bootstrap, &dir.path().join("state.json"), dir.path()).unwrap();
    let router = server::router(store);
    for (token, path, method, expected) in [
        ("invalid", "/api/config/catalog", "GET", 401),
        (AGENT, "/api/config/catalog", "GET", 403),
        (READER, "/api/config/catalog", "GET", 200),
        (READER, "/api/config/targets/demo-1/publish", "POST", 403),
        (EDITOR, "/api/config/targets/other", "GET", 403),
    ] {
        let body = if method == "POST" {
            r#"{"draft_version":0,"base_revision":0}"#
        } else {
            ""
        };
        let r = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .method(method)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), expected, "{path}");
    }
}
#[tokio::test]
async fn real_grpc_pull_and_report_are_bound_to_agent_credentials() {
    let (dir, bootstrap) = fixture();
    let store = Store::open(bootstrap, &dir.path().join("state.json"), dir.path()).unwrap();
    publish(&store, "grpc fixture");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let grpc = server::Grpc(store.clone());
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ConfigurationServer::new(grpc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap()
    });
    let mut client = ConfigurationClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    assert!(client.pull(wire::PullRequest {}).await.is_err());
    let mut req = tonic::Request::new(wire::PullRequest {});
    req.metadata_mut()
        .insert("authorization", format!("Bearer {EDITOR}").parse().unwrap());
    assert!(client.pull(req).await.is_err());
    let mut req = tonic::Request::new(wire::PullRequest {});
    req.metadata_mut()
        .insert("authorization", format!("Bearer {AGENT}").parse().unwrap());
    let bundle: Bundle =
        serde_json::from_str(&client.pull(req).await.unwrap().into_inner().bundle_json).unwrap();
    assert_eq!(bundle.target, "demo-1");
    let mut req = tonic::Request::new(wire::ReportRequest {
        revision: 1,
        applied: true,
        code: "applied".into(),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {AGENT}").parse().unwrap());
    client.report(req).await.unwrap();
    assert!(store.view("demo-1").unwrap().report.unwrap().applied);
    task.abort();
}
