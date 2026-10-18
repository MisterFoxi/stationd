use stationdeck_services::{manifest::Capability, registry::{Grants, Policy}};
use stationdeck_ui::DeckModule;

pub fn modules() -> Vec<Box<dyn DeckModule>> {
    vec![
        Box::<stationdeck_demo::DemoModule>::default(),
        Box::<stationdeck_grpc::GrpcModule>::default(),
        Box::<stationdeck_config_ui::ConfigModule>::default(),
        Box::<crate::overview::OverviewModule>::default(),
    ]
}

/// Local deployment policy. These grants convey no server identity or authoring rights.
/// Change the overview binding to "grpc" to use the same consumer against the gateway.

pub fn policy_with_provider(provider: &str) -> Policy {
    let mut policy = Policy::default();
    for (module, caps, permissions) in [
        ("demo", vec![("overview", "service", "stationdeck.admin.overview", vec!["read", "watch", "command"])], vec!["consulter", "simuler"]),
        ("grpc", vec![("connection", "network", "connection:station", vec!["connect"]), ("overview", "service", "stationdeck.admin.overview", vec!["read", "watch", "command"])], vec!["consulter", "connecter"]),
        ("configuration", vec![("connection", "network", "connection:configuration", vec!["connect"])], vec!["consulter", "connecter"]),
        ("overview", vec![("status", "service", "stationdeck.admin.overview", vec!["read"])], vec!["consulter"]),
    ] {
        policy.grants.insert(module.into(), Grants {
            capabilities: caps.into_iter().map(|(id, kind, resource, ops)| Capability {
                id: format!("{module}.{id}"), kind: kind.into(), resource: resource.into(),
                operations: ops.into_iter().map(String::from).collect(), max_rows: None, timeout_ms: None,
            }).collect(),
            permissions: permissions.into_iter().map(|id| format!("{module}.{id}")).collect(),
        });
    }
    policy.bindings.insert(("overview".into(), "stationdeck.admin.overview".into()), provider.into());
    policy
}


#[cfg(test)]
mod tests {
    use super::*;
    use stationdeck_services::registry::State;
    use stationdeck_ui::ModuleCatalog;
    #[test]
    fn the_same_consumer_runs_with_either_provider_even_when_both_are_enabled() {
        let enabled = vec!["demo".into(), "grpc".into(), "overview".into(), "configuration".into()];
        let mut catalog = ModuleCatalog::new(modules(), &enabled, policy_with_provider("demo"));
        for id in ["demo", "grpc", "overview", "configuration"] {
            assert!(matches!(catalog.status(id), Some((State::Running, None))));
        }
        catalog.set_binding("overview", "stationdeck.admin.overview", "grpc").unwrap();
        catalog.set_enabled("demo", false).unwrap();
        assert!(matches!(catalog.status("overview"), Some((State::Running, None))));
        assert!(catalog.views().iter().any(|v| v.id == "overview.station"));
        catalog.set_enabled("grpc", false).unwrap();
        assert!(matches!(catalog.status("overview"), Some((State::Blocked, Some(_)))));
        assert!(!catalog.views().iter().any(|v| v.id == "overview.station"));
        assert!(catalog.views().iter().any(|v| v.id == "configuration.authoring"));
    }
}
