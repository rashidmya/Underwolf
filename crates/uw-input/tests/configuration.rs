use uw_input::configuration::PreparedConfig;
use uw_pipe_proto::routing::*;

#[test]
fn a_table_without_an_exit_chord_cannot_be_prepared() {
    let config = RoutingConfig {
        epoch: 1,
        revision: 1,
        request_id: 1,
        width: 800,
        height: 600,
        widgets: vec![],
        hotkeys: vec![],
    };
    assert!(PreparedConfig::compile(config).is_err());
}
