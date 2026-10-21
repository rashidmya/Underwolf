//! Host commands to the in-game module (portable). The `cfg(windows)` reader thread feeds every
//! message from the host through [`ModuleCommands::on_msg`] and carries out the actions.

use uw_spike_ipc::Msg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleAction {
    InstallInputHooks,
    LoadPlugin { path: String, channel: u32, args: String },
}

/// The module starts with no input hooks; they are installed once, on the first
/// `ModuleConfig { input_hooks: true }`, and never removed.
#[derive(Debug, Default)]
pub struct ModuleCommands {
    hooks_installed: bool,
}

impl ModuleCommands {
    pub fn new() -> ModuleCommands {
        ModuleCommands::default()
    }

    pub fn on_msg(&mut self, m: &Msg) -> Vec<ModuleAction> {
        match m {
            Msg::ModuleConfig { input_hooks: true } if !self.hooks_installed => {
                self.hooks_installed = true;
                vec![ModuleAction::InstallInputHooks]
            }
            Msg::LoadPlugin { path, channel, args } => {
                vec![ModuleAction::LoadPlugin { path: path.clone(), channel: *channel, args: args.clone() }]
            }
            _ => Vec::new(),
        }
    }
}
