use dashmap::DashMap;
use std::sync::Arc;

use crate::audio::player::Player;
use crate::config::{load_config, EffectiveConfig};

#[derive(Clone)]
pub struct AppState {
    pub players: Arc<DashMap<String, Arc<Player>>>,
    pub cfg: Arc<EffectiveConfig>,
}

impl AppState {
    pub fn new(cfg: EffectiveConfig) -> Self {
        Self { players: Arc::new(DashMap::new()), cfg: Arc::new(cfg) }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(load_config())
    }
}
