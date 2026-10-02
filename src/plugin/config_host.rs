// Host-owned operations, serialized by the plugin actor.
use super::*;
pub(super) struct ConfigPlugin;
impl Plugin for ConfigPlugin {
    fn name(&self) -> &str {
        "plugin-config"
    }
    fn ui_tabs(&mut self) -> Result<Vec<crate::plugin_ui::UiTab>, String> {
        Ok(vec![crate::plugin_ui::UiTab {
            id: "configuration".into(),
            title: "Configuration".into(),
            description: "Configuration des plugins".into(),
            sql: String::new(),
            kind: "plugin_config".into(),
        }])
    }
}

pub(super) fn validate_candidate(
    slot: &Slot,
    env: &PluginEnv,
    config: &toml::Table,
) -> Result<(), String> {
    crate::plugin_config::patch(&slot.schema, config, &[])?;
    let mut decl = slot.decl.clone();
    decl.config = config.clone();
    let host = slot
        .host
        .clone()
        .unwrap_or_else(|| Host::new(&decl.name, &decl.capabilities, env.control.clone()));
    host.set_simulating(true);
    let result =
        catch(|| build_plugin(&decl, &host).and_then(|mut p| p.validate_config(config, &host)))
            .and_then(|r| r);
    host.set_simulating(false);
    // A plugin's error can contain a secret value. Do not forward guest errors.
    result.map_err(|e| {
        if slot.schema.iter().any(|f| f.secret) {
            "plugin refused configuration".into()
        } else {
            e
        }
    })
}

pub(super) fn config_operation(
    slots: &mut [Slot],
    env: &PluginEnv,
    path: Option<&std::path::Path>,
    req: crate::proto::plugin::PluginConfigUpdateRequest,
    read: bool,
) -> Result<crate::proto::plugin::PluginConfigResponse, String> {
    use crate::{plugin_config as cfg, proto::plugin as wire};
    let path = path.ok_or("configuration editing is unavailable in this daemon")?;
    let slot = slots
        .iter_mut()
        .find(|s| s.decl.name == req.name)
        .ok_or("unknown plugin")?;
    if slot.schema.is_empty() {
        return Err(
            "plugin does not declare a configuration schema; load an updated plugin first".into(),
        );
    }
    let (bytes, current) = cfg::read(path, &req.name)?;
    let schema_revision =
        cfg::revision(&serde_json::to_vec(&slot.schema).map_err(|_| "cannot encode schema")?);
    let revision = cfg::revision(&bytes);
    let mut response = wire::PluginConfigResponse {
        name: req.name.clone(),
        revision: revision.clone(),
        schema_revision: schema_revision.clone(),
        ..Default::default()
    };
    let mut current = current;
    if !read {
        if req.mode < 0 || req.mode > 2 {
            return Err("invalid configuration action".into());
        }
        if req.revision != revision || req.schema_revision != schema_revision {
            return Err("conflict: configuration or schema changed; refresh before saving".into());
        }
        let edits: Vec<_> = req
            .edits
            .iter()
            .map(|e| (e.key.clone(), e.present.then(|| e.value.clone())))
            .collect();
        let candidate = cfg::patch(&slot.schema, &current, &edits)?;
        validate_candidate(slot, env, &candidate)?;
        for f in &slot.schema {
            if current.get(&f.key) != candidate.get(&f.key) {
                let show = |v: Option<&toml::Value>| match v {
                    None => "(default / absent)".into(),
                    Some(_) if f.secret => "••••".into(),
                    Some(v) => cfg::display(v)
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect::<String>(),
                };
                response.changes.push(format!(
                    "{}: {} → {}",
                    f.label,
                    show(current.get(&f.key)),
                    show(candidate.get(&f.key))
                ));
            }
        }
        if req.mode != 0 {
            response.revision = cfg::save(path, &bytes, &req.name, &edits, &slot.schema)?;
            response.saved = true;
            current = candidate;
            if req.mode == 2 {
                let loaded = matches!(slot.state, PluginState::Loaded);
                slot.decl.config = current.clone();
                if loaded {
                    apply_action(slot, Action::Reload, env);
                    response.applied = matches!(slot.state, PluginState::Loaded);
                    response.message = if response.applied {
                        "Configuration enregistrée et appliquée".into()
                    } else {
                        "Configuration enregistrée ; échec du rechargement. Consulter l’état du plugin.".into()
                    };
                } else {
                    response.message =
                        "Configuration enregistrée ; plugin arrêté, démarrage explicite nécessaire"
                            .into();
                }
            } else {
                response.message = "Configuration enregistrée ; rechargement nécessaire".into();
            }
        }
    }
    response.pending = !matches!(slot.state, PluginState::Loaded) || current != slot.decl.config;
    for f in &slot.schema {
        response.fields.push(wire::PluginConfigField {
            key: f.key.clone(),
            label: f.label.clone(),
            kind: f.kind.clone(),
            optional: f.optional,
            default_value: f.default.clone(),
            minimum: f.minimum,
            maximum: f.maximum,
            secret: f.secret,
        });
        let value = current.get(&f.key);
        response.values.push(wire::PluginConfigValue {
            key: f.key.clone(),
            present: value.is_some(),
            redacted: f.secret && value.is_some(),
            value: if f.secret {
                String::new()
            } else {
                value.map(cfg::display).unwrap_or_default()
            },
        });
    }
    Ok(response)
}

#[cfg(test)]
mod config_editor_tests {
    use super::*;
    use crate::proto::plugin::{PluginConfigUpdateRequest, PluginConfigValue};
    fn setup(enabled: bool) -> (tempfile::TempDir, PathBuf, Vec<PluginDecl>, PluginEnv) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stationd.toml");
        let text = format!("{}\n[[plugin]]\nname = 'plugin-config'\nenabled = true\n[[plugin]]\nname = 'stop-when-idle'\nenabled = {enabled}\ncapabilities = ['control']\n[plugin.config]\nmin_zero_samples = 1 # preserved\nfuture = 'keep'\n", include_str!("../../stationd.example.toml"));
        std::fs::write(&path, &text).unwrap();
        let config: crate::config::Config = toml::from_str(&text).unwrap();
        let env = PluginEnv {
            control: Some(StationControl::new_in_memory()),
            ..Default::default()
        };
        (dir, path, config.plugins, env)
    }
    fn edit(
        data: &crate::proto::plugin::PluginConfigResponse,
        value: &str,
        mode: i32,
    ) -> PluginConfigUpdateRequest {
        PluginConfigUpdateRequest {
            name: data.name.clone(),
            revision: data.revision.clone(),
            schema_revision: data.schema_revision.clone(),
            edits: vec![PluginConfigValue {
                key: "min_zero_samples".into(),
                value: value.into(),
                present: true,
                ..Default::default()
            }],
            mode,
        }
    }
    #[tokio::test]
    async fn save_only_keeps_runtime_until_explicit_reload_and_conflicts_do_not_write() {
        let (_dir, path, decls, env) = setup(true);
        let control = env.control.clone().unwrap();
        let h = spawn_configured(decls, env, Some(path.clone()));
        let data = h
            .config(
                PluginConfigUpdateRequest {
                    name: "stop-when-idle".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        assert_eq!(h.list().await[0].tabs[0].kind, "plugin_config");
        let preview = h.config(edit(&data, "3", 0), false).await.unwrap();
        assert!(!preview.saved);
        assert_eq!(preview.changes.len(), 1);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("min_zero_samples = 1"));
        let saved = h.config(edit(&data, "3", 1), false).await.unwrap();
        assert!(saved.saved && saved.pending && !saved.applied);
        h.control("stop-when-idle", Action::Start).await.unwrap();
        let after_noop = h
            .config(
                PluginConfigUpdateRequest {
                    name: "stop-when-idle".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        assert!(
            after_noop.pending,
            "starting an already loaded plugin must remain a no-op"
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(h
            .config(edit(&data, "4", 1), false)
            .await
            .unwrap_err()
            .starts_with("conflict:"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // Still running old min=1 after saving only.
        h.emit(PluginEvent::ListenersSampled { count: 0, at: 0 });
        let _ = h.list().await;
        assert_eq!(
            control.state(),
            crate::station_control::BroadcastState::Draining
        );
        h.control("stop-when-idle", Action::Reload).await.unwrap();
        let live = h
            .config(
                PluginConfigUpdateRequest {
                    name: "stop-when-idle".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        assert!(!live.pending);
        let fresh = h.config(edit(&live, "2", 2), false).await.unwrap();
        assert!(fresh.saved && fresh.applied && !fresh.pending);
        let mut wrong = edit(&fresh, "0", 1);
        assert!(h.config(wrong.clone(), false).await.is_err());
        wrong.edits[0].key = "max_connection_age".into();
        wrong.edits[0].value = "12h".into();
        let original = std::fs::read(&path).unwrap();
        assert!(h
            .config(wrong, false)
            .await
            .unwrap_err()
            .contains("listener_snapshots"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[tokio::test]
    async fn saving_with_reload_does_not_start_a_disabled_plugin() {
        let (_dir, path, decls, env) = setup(false);
        let h = spawn_configured(decls, env, Some(path));
        let data = h
            .config(
                PluginConfigUpdateRequest {
                    name: "stop-when-idle".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let saved = h.config(edit(&data, "2", 2), false).await.unwrap();
        assert!(saved.saved && !saved.applied && saved.pending);
        assert_eq!(
            h.list()
                .await
                .iter()
                .find(|p| p.name == "stop-when-idle")
                .unwrap()
                .state,
            "disabled"
        );
    }
    #[tokio::test]
    async fn manual_file_edits_and_schema_changes_reject_stale_drafts() {
        let (_dir, path, decls, env) = setup(true);
        let h = spawn_configured(decls, env, Some(path.clone()));
        let data = h
            .config(
                PluginConfigUpdateRequest {
                    name: "stop-when-idle".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let mut request = edit(&data, "2", 1);
        request.schema_revision = "stale".into();
        assert!(h
            .config(request, false)
            .await
            .unwrap_err()
            .starts_with("conflict:"));
        let text = std::fs::read_to_string(&path).unwrap() + "\n# operator edit\n";
        std::fs::write(&path, &text).unwrap();
        assert!(h
            .config(edit(&data, "2", 1), false)
            .await
            .unwrap_err()
            .starts_with("conflict:"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn rpc_values_and_previews_never_return_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stationd.toml");
        let text = format!(
            "{}\n[[plugin]]\nname = 'logger'\n[plugin.config]\ntoken = 'old-secret'\n",
            include_str!("../../stationd.example.toml")
        );
        std::fs::write(&path, &text).unwrap();
        let parsed: crate::config::Config = toml::from_str(&text).unwrap();
        let decl = parsed.plugins[0].clone();
        let field = crate::plugin_config::Field {
            key: "token".into(),
            label: "Token".into(),
            kind: "text".into(),
            secret: true,
            optional: true,
            default: None,
            minimum: None,
            maximum: None,
        };
        let mut slots = vec![Slot {
            schema: vec![field],
            tabs: vec![],
            decl,
            state: PluginState::Disabled,
            plugin: None,
            failures: VecDeque::new(),
            host: None,
        }];
        let env = PluginEnv::default();
        let data = config_operation(
            &mut slots,
            &env,
            Some(&path),
            PluginConfigUpdateRequest {
                name: "logger".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert!(
            data.values[0].present && data.values[0].redacted && data.values[0].value.is_empty()
        );
        let req = PluginConfigUpdateRequest {
            name: "logger".into(),
            revision: data.revision,
            schema_revision: data.schema_revision,
            edits: vec![PluginConfigValue {
                key: "token".into(),
                value: "new-secret".into(),
                present: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let preview = config_operation(&mut slots, &env, Some(&path), req, false).unwrap();
        assert_eq!(preview.changes, vec!["Token: •••• → ••••"]);
        assert!(preview.values[0].value.is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }
    #[tokio::test]
    #[ignore = "requires compiled stop-when-idle WASM"]
    async fn renamed_wasm_schema_can_be_edited_and_reloaded() {
        let (_dir, path, mut decls, env) = setup(true);
        let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/stop-when-idle-wasm/target/wasm32-unknown-unknown/release/stop_when_idle_wasm.wasm");
        let d = decls
            .iter_mut()
            .find(|d| d.name == "stop-when-idle")
            .unwrap();
        d.name = "my-policy".into();
        d.wasm = Some(wasm.to_str().unwrap().into());
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("name = 'stop-when-idle'", "name = 'my-policy'");
        std::fs::write(&path, text).unwrap();
        let h = spawn_configured(decls, env, Some(path));
        let data = h
            .config(
                PluginConfigUpdateRequest {
                    name: "my-policy".into(),
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        assert_eq!(data.fields.len(), 2);
        let preview = h.config(edit(&data, "2", 0), false).await.unwrap();
        assert_eq!(preview.changes.len(), 1);
        let saved = h.config(edit(&data, "2", 2), false).await.unwrap();
        assert!(
            saved.saved && saved.applied && !saved.pending,
            "{}",
            saved.message
        );
        assert_eq!(
            h.list()
                .await
                .iter()
                .find(|p| p.name == "my-policy")
                .unwrap()
                .state,
            "loaded"
        );
    }

    #[test]
    fn reload_failure_reports_saved_configuration_without_claiming_it_was_applied() {
        let (_dir, path, decls, env) = setup(false);
        env.control
            .as_ref()
            .unwrap()
            .configure_connection_sampling(true);
        let mut decl = decls
            .into_iter()
            .find(|d| d.name == "stop-when-idle")
            .unwrap();
        decl.enabled = true;
        let mut slot = Slot {
            schema: crate::plugin_config::stop_fields(),
            tabs: vec![],
            decl,
            state: PluginState::Disabled,
            plugin: None,
            failures: VecDeque::new(),
            host: None,
        };
        slot.start(&env);
        assert!(matches!(slot.state, PluginState::Loaded));
        let mut slots = vec![slot];
        let data = config_operation(
            &mut slots,
            &env,
            Some(&path),
            PluginConfigUpdateRequest {
                name: "stop-when-idle".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap();
        // The old instance can validate, but its station dependency is unavailable on reload.
        let unavailable = PluginEnv::default();
        let req = PluginConfigUpdateRequest {
            name: data.name,
            revision: data.revision,
            schema_revision: data.schema_revision,
            edits: vec![PluginConfigValue {
                key: "max_connection_age".into(),
                value: "12h".into(),
                present: true,
                ..Default::default()
            }],
            mode: 2,
        };
        let result = config_operation(&mut slots, &unavailable, Some(&path), req, false).unwrap();
        assert!(result.saved && !result.applied && result.pending);
        assert!(result.message.contains("échec"));
        assert!(matches!(slots[0].state, PluginState::Failed { .. }));
        let (_, stored) = crate::plugin_config::read(&path, "stop-when-idle").unwrap();
        assert_eq!(stored["max_connection_age"].as_str(), Some("12h"));
    }
}
