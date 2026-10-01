//! Render typed indications published by loaded plugins, never infer policy from names.
use std::time::Duration;
use stationd_proto::plugin::{operator_notice::Code, OperatorNotice};
use crate::store::{human_duration, Store};
use crate::tr;

pub fn text(notice: &OperatorNotice, compact: bool) -> String {
    match Code::try_from(notice.code).unwrap_or(Code::Unspecified) {
        Code::AutoSleep => match notice.max_connection_age {
            Some(age) => {
                let age = human_duration(Duration::from_secs(age));
                if compact { tr!("mode-auto-sleep-age-short", age = age) }
                else { tr!("mode-auto-sleep-age", age = age) }
            }
            None => if compact { tr!("mode-auto-sleep-zero-short") } else { tr!("mode-auto-sleep-zero") },
        },
        Code::Unspecified => tr!("mode-unknown"),
    }
}

pub fn active(store: &Store, compact: bool) -> Vec<(String, String)> {
    store.plugins.value.as_ref().into_iter().flatten()
        .filter(|p| p.state == "loaded")
        .filter_map(|p| p.operator_notice.as_ref().map(|notice| (p.name.clone(), text(notice, compact))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::plugin::PluginInfo;

    #[test]
    fn active_modes_are_name_independent_and_disappear_when_disabled() {
        let mut store = Store::new("http://localhost:50051");
        store.plugins.value = Some(vec![PluginInfo {
            name: "renamed-radio-policy".into(), state: "loaded".into(),
            operator_notice: Some(OperatorNotice {
                code: Code::AutoSleep as i32, max_connection_age: Some(600),
            }), ..Default::default()
        }]);
        let modes = active(&store, false);
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].0, "renamed-radio-policy");
        assert!(modes[0].1.contains("10"));
        assert!(!modes[0].1.contains("⟦"));
        store.plugins.value.as_mut().unwrap()[0].state = "disabled".into();
        assert!(active(&store, false).is_empty());
        store.plugins.value.as_mut().unwrap()[0].state = "failed".into();
        assert!(active(&store, false).is_empty());
    }
}
