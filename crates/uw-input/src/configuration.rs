//! Only validated, immutable snapshots may reach a callback's table replacement.
use uw_pipe_proto::{pipe::ProtoError, routing::RoutingConfig};
#[derive(Debug)]
pub struct ConfigError(pub ProtoError);
#[derive(Debug, Clone)]
pub struct PreparedConfig {
    config: RoutingConfig,
}
impl PreparedConfig {
    pub fn compile(config: RoutingConfig) -> Result<Self, ConfigError> {
        config.validate().map_err(ConfigError)?;
        Ok(Self { config })
    }
    pub fn config(&self) -> &RoutingConfig {
        &self.config
    }
}
