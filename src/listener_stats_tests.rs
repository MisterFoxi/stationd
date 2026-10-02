use super::*;

#[test]
fn geoip_contract_checks_capability_and_input() {
    let denied = Host::new("test", &[], None);
    let allowed = Host::new("test", &[Capability::Geoip], None);
    let reply = |h: &Host, input| -> serde_json::Value {
        serde_json::from_str(&wasm_geoip_lookup(h, input)).unwrap()
    };
    assert_eq!(reply(&denied, r#"{"ip":"192.0.2.1"}"#)["ok"], false);
    assert_eq!(reply(&allowed, r#"{"ip":"2001:db8::1"}"#), serde_json::json!({
        "ok": true, "status": "unavailable", "country": null, "city": null, "region": null
    }));
    let bad = reply(&allowed, r#"{"ip":"secret-invalid-input"}"#);
    assert_eq!(bad["ok"], false);
    assert!(!bad.to_string().contains("secret-invalid-input"));
}

#[test]
fn detailed_snapshots_only_reach_opted_in_plugins() {
    struct Counter(Arc<std::sync::atomic::AtomicUsize>);
    impl Plugin for Counter {
        fn name(&self) -> &str { "counter" }
        fn on_event(&mut self, _: &PluginEvent) { self.0.fetch_add(1, Ordering::SeqCst); }
    }
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let declaration: PluginDecl = toml::from_str("name = \"counter\"\nenabled = true").unwrap();
    let mut slot = Slot { schema: Vec::new(), tabs: Vec::new(),
        decl: declaration, state: PluginState::Loaded,
        plugin: Some(Box::new(Counter(count.clone()))), failures: VecDeque::new(), host: None,
    };
    let event = PluginEvent::ListenerSnapshot { mount: "/a".into(), at: 1, listeners: Some(vec![]) };
    dispatch_event(std::slice::from_mut(&mut slot), &event);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    slot.decl.capabilities.push(Capability::ListenerDetails);
    dispatch_event(std::slice::from_mut(&mut slot), &event);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    slot.decl.capabilities.clear();
    dispatch_event(std::slice::from_mut(&mut slot), &PluginEvent::ListenersSampled { count: 0, at: 1 });
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
#[ignore = "requires STATIOND_TEST_DBIP and STATIOND_TEST_LISTENER_WASM"]
fn real_dbip_wasm_host_and_database_roundtrip() {
    let mmdb = std::env::var_os("STATIOND_TEST_DBIP").expect("STATIOND_TEST_DBIP");
    let wasm = std::env::var("STATIOND_TEST_LISTENER_WASM").expect("STATIOND_TEST_LISTENER_WASM");
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(PluginDb::open(dir.path(), "listener-stats", Default::default()).unwrap());
    let mut host = Host::new("listener-stats", &[Capability::Db, Capability::Geoip, Capability::ListenerDetails], None)
        .with_db(db.clone());
    host.geoip = Some(Arc::new(crate::geoip::Geoip::open(std::path::Path::new(&mmdb)).unwrap()));
    let mut guest = WasmPlugin::new("listener-stats".into(), &wasm, &toml::Table::new(), &host).unwrap();
    db.migrate(&guest.db_migrations().unwrap()).unwrap();
    let event = PluginEvent::ListenerSnapshot {
        mount: "/radio".into(), at: 1000,
        listeners: Some(vec![crate::listener_snapshot::Listener {
            id: "42".into(), ip: "8.8.8.8".parse().unwrap(), connected_seconds: 15, user_agent: None,
        }]),
    };
    guest.on_event(&event);
    guest.on_event(&event);
    let result = db.query("SELECT status, country, listeners FROM listener_geo", &Default::default()).unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["rows"].as_array().unwrap().len(), 1);
    assert_eq!(result["rows"][0][0], "found");
    assert_eq!(result["rows"][0][1].as_str().unwrap().len(), 2);
    assert_eq!(result["rows"][0][2], 1);
}
