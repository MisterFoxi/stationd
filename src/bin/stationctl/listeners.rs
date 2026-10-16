//! Convenience view over the plugin's read-only DB RPC. No new core storage.
use anyhow::Context;
use stationd::proto::plugin::{
    plugin_db_value::Kind, plugin_service_client::PluginServiceClient,
    PluginDbQueryRequest, PluginDbQueryResponse,
};

#[derive(clap::Subcommand, Debug)]
pub enum ListenersCommand {
    /// Latest listener counts per administrative region and mount
    Regions {
        /// Only this Icecast mount (otherwise show each mount separately)
        #[arg(long)]
        mount: Option<String>,
        /// Name of the listener-stats plugin declaration
        #[arg(long, default_value = "listener-stats")]
        plugin: String,
    },
    /// Geographic audience averages and peaks by UTC hour, day or week
    Stats {
        #[arg(long, value_enum, default_value = "hour")]
        by: Period,
        /// History window: e.g. 24h, 7d or 4w
        #[arg(long, default_value = "24h", value_parser = parse_window)]
        since: u64,
        #[arg(long)]
        mount: Option<String>,
        #[arg(long, default_value = "listener-stats")]
        plugin: String,
    },
}

pub use stationd::listener_history::{parse_window, regions_query, stats_query, Period};

pub async fn run(addr: &str, command: ListenersCommand) -> anyhow::Result<()> {
    let (plugin, sql, history) = match command {
        ListenersCommand::Regions { mount, plugin } => (plugin, regions_query(mount.as_deref()), false),
        ListenersCommand::Stats { by, since, mount, plugin } => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
            (plugin, stats_query(by, now - since as i64, now, mount.as_deref()), true)
        }
    };
    let mut client = PluginServiceClient::connect(addr.to_owned()).await?;
    let response = client.db_query(PluginDbQueryRequest {
        name: plugin,
        sql,
    }).await.context(
        "Cannot read listener regions. Enable listener-stats with capability db; rebuild its WASM and reload the plugin to apply migration 2."
    )?.into_inner();
    print!("{}", if history { render_stats(&response)? } else { render(&response)? });
    Ok(())
}

pub fn render_stats(response: &PluginDbQueryResponse) -> anyhow::Result<String> {
    if response.rows.is_empty() {
        return Ok("Aucun effectif positif disponible sur cette periode (zero, absence de donnees ou collecte en echec).\n".into());
    }
    let mut rows = vec![vec!["PERIODE_UTC", "MOUNT", "PAYS", "REGION", "VILLE", "MOYENNE", "PIC", "RELEVES", "ECHECS", "ETAT_GEOIP"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>()];
    for row in &response.rows {
        anyhow::ensure!(row.values.len() == 10, "Unexpected listener statistics response");
        rows.push(row.values.iter().map(|v| match &v.kind {
            Some(Kind::Text(s)) if s.is_empty() => "(inconnue)".into(),
            Some(Kind::Text(s)) => safe_text(s),
            Some(Kind::Integer(i)) => i.to_string(),
            Some(Kind::Real(f)) if *f > 0.0 && *f < 0.01 => "<0.01".into(),
            Some(Kind::Real(f)) => format!("{f:.2}"),
            _ => "?".into(),
        }).collect());
    }
    let widths: Vec<usize> = (0..10).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap()).collect();
    let mut out = String::new();
    for row in rows {
        out.push_str(&row.iter().enumerate().map(|(i, s)| format!("{s:<width$}", width=widths[i])).collect::<Vec<_>>().join("  "));
        out.push('\n');
    }
    out.push_str("Moyenne des releves reussis, zeros inclus dans le calcul; pic d'auditeurs simultanes observes.\n");
    out.push_str("Ce ne sont pas des visiteurs uniques. Collectes echouees exclues; periodes partielles possibles.\n");
    out.push_str("Semaines du lundi au dimanche UTC. Mounts separes; lignes a zero masquees.\n");
    out.push_str(stationd::geoip::DATA_ATTRIBUTION);
    Ok(out)
}

fn safe_text(value: &str) -> String {
    // Mounts and geographic names are external strings: no terminal escapes.
    value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub fn render(response: &PluginDbQueryResponse) -> anyhow::Result<String> {
    if response.rows.is_empty() {
        return Ok("Aucun effectif positif disponible (ou aucun releve) pour cette selection.\n".into());
    }
    let mut rows = vec![vec!["MOUNT", "RELEVE_EPOCH", "PAYS", "REGION", "AUDITEURS", "ETAT"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>()];
    for row in &response.rows {
        anyhow::ensure!(row.values.len() == 6, "Unexpected listener regions response");
        let mut cells: Vec<String> = row.values.iter().map(|v| match &v.kind {
            None | Some(Kind::Null(_)) => "?".into(),
            Some(Kind::Integer(i)) => i.to_string(),
            Some(Kind::Text(s)) if s.is_empty() => "(inconnue)".into(),
            Some(Kind::Text(s)) => safe_text(s),
            _ => "?".into(),
        }).collect();
        cells[5] = match cells[5].as_str() {
            "found" => "localise".into(),
            "not_found" => "non_localise".into(),
            "unavailable" => "geoip_indisponible".into(),
            _ => cells[5].clone(),
        };
        rows.push(cells);
    }
    let widths: Vec<usize> = (0..6).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap()).collect();
    let mut out = String::from("Dernier releve conserve de chaque mount (pas un historique cumule).\n");
    for row in rows {
        let line = row.iter().enumerate().map(|(i, s)| format!("{s:<width$}", width=widths[i])).collect::<Vec<_>>().join("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str("Les horodatages peuvent differer ou etre anciens si la collecte est arretee.\n");
    out.push_str(stationd::geoip::DATA_ATTRIBUTION);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use stationd::proto::plugin::{PluginDbRow, PluginDbValue};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: ListenersCommand,
    }

    #[test]
    fn accepts_regions_and_optional_mount() {
        let cli = Cli::try_parse_from(["listeners", "regions", "--mount", "/radio"]).unwrap();
        let ListenersCommand::Regions { mount, plugin } = cli.command else { panic!("expected regions") };
        assert_eq!(mount.as_deref(), Some("/radio"));
        assert_eq!(plugin, "listener-stats");
    }

    #[test]
    fn escapes_the_mount_literal() {
        assert!(regions_query(Some("/a' OR 1=1 --")).contains("WHERE mount = '/a'' OR 1=1 --'"));
    }

    #[test]
    fn accepts_history_options_and_rejects_bad_windows() {
        let cli = Cli::try_parse_from(["listeners", "stats", "--by", "week", "--since", "4w"]).unwrap();
        let ListenersCommand::Stats { by: Period::Week, since, .. } = cli.command else { panic!("expected weekly stats") };
        assert_eq!(since, 28 * 86400);
        for bad in ["0h", "-1d", "all", "999999999999999999999w", "366d", "1m"] {
            assert!(parse_window(bad).is_err());
        }
    }

    #[test]
    fn renders_unknown_without_turning_it_into_zero() {
        let response = PluginDbQueryResponse {
            columns: vec![], rows: vec![PluginDbRow {values: vec![
                Kind::Text("/a\u{1b}[31m".into()), Kind::Integer(100),
                Kind::Text("".into()), Kind::Text("".into()), Kind::Null(true),
                Kind::Text("collecte_inconnue".into()),
            ].into_iter().map(|kind| PluginDbValue {kind: Some(kind)}).collect()}],
        };
        let out = render(&response).unwrap();
        assert!(out.contains("collecte_inconnue"));
        assert!(out.contains('?'));
        assert!(!out.contains('\u{1b}'));
        assert!(out.contains("https://db-ip.com"));
    }

    #[test]
    fn small_positive_means_are_not_displayed_as_zero() {
        let mut values = vec![Kind::Text("x".into()); 10];
        values[5] = Kind::Real(0.0001);
        let response = PluginDbQueryResponse {
            columns: vec![], rows: vec![PluginDbRow {values: values.into_iter()
                .map(|kind| PluginDbValue {kind: Some(kind)}).collect()}],
        };
        let out = render_stats(&response).unwrap();
        assert!(out.contains("<0.01"));
        assert!(!out.contains("0.00"));
    }
}
