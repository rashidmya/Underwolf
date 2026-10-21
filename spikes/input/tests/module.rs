use uw_spike_input::module::{ModuleAction, ModuleCommands};
use uw_spike_ipc::Msg;

#[test]
fn module_installs_input_hooks_only_when_told() {
    let mut m = ModuleCommands::new();
    assert_eq!(m.on_msg(&Msg::ModuleConfig { input_hooks: false }), []);
    assert_eq!(m.on_msg(&Msg::ModuleConfig { input_hooks: true }), [ModuleAction::InstallInputHooks]);
    assert_eq!(m.on_msg(&Msg::ModuleConfig { input_hooks: true }), []);
    // Hooks cannot be taken back once installed; a later `false` changes nothing.
    assert_eq!(m.on_msg(&Msg::ModuleConfig { input_hooks: false }), []);
    assert_eq!(m.on_msg(&Msg::ModuleConfig { input_hooks: true }), []);
}

#[test]
fn module_forwards_load_plugin() {
    let mut m = ModuleCommands::new();
    let msg = Msg::LoadPlugin { path: r"Z:\x\uw_spike_dlcollector.dll".into(), channel: 7, args: "discover=10".into() };
    assert_eq!(
        m.on_msg(&msg),
        [ModuleAction::LoadPlugin {
            path: r"Z:\x\uw_spike_dlcollector.dll".into(),
            channel: 7,
            args: "discover=10".into()
        }]
    );
}

#[test]
fn module_ignores_other_messages() {
    let mut m = ModuleCommands::new();
    assert_eq!(m.on_msg(&Msg::EditMode { on: true }), []);
    assert_eq!(m.on_msg(&Msg::Hello { protocol: 1, pid: 1 }), []);
    assert_eq!(m.on_msg(&Msg::PluginLoaded { path: "p".into(), ok: true, error: None }), []);
}
