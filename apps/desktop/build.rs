use tauri_build::{Attributes, DefaultPermissionRule, InlinedPlugin};

fn main() {
    // The sim-tools plugin has no build script of its own: its commands come
    // from the shared contract, so its permissions are declared here.
    let sim_tools = InlinedPlugin::new()
        .commands(infinity_sim_tools_ipc::ALL)
        .default_permission(DefaultPermissionRule::AllowAllCommands);
    let attributes = Attributes::new().plugin(infinity_sim_tools_ipc::PLUGIN, sim_tools);
    if let Err(e) = tauri_build::try_build(attributes) {
        #[allow(clippy::panic)]
        {
            panic!("tauri build failed: {e:#}");
        }
    }
}
