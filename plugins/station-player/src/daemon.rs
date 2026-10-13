//! Native registry adapter. Implementation, assets and tests live in plugins/.
use super::{Host, Plugin};

pub(super) use ::station_player::validate_config;

pub(super) struct StationPlayer(::station_player::StationPlayer);

impl StationPlayer {
    pub(super) fn from_config(config: &toml::Table) -> Result<Self, String> {
        ::station_player::StationPlayer::from_config(config).map(Self)
    }
}

impl Plugin for StationPlayer {
    fn name(&self) -> &str {
        "station-player"
    }

    fn filters_pool(&self) -> bool {
        false
    }

    fn validate_config(&mut self, config: &toml::Table, _host: &Host) -> Result<(), String> {
        validate_config(config)
    }

    fn on_load(&mut self, _host: Host) -> Result<(), String> {
        self.0.start()
    }

    fn on_unload(&mut self) {
        self.0.stop();
    }
}
