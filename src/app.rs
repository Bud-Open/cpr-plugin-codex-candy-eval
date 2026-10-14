use crate::command;
use crate::management;
use crate::state::StateManager;
use gateway_plugin_sdk::client::{AuthorError, ComposedPlugin, PluginBuilder};
use std::sync::Arc;

pub fn plugin() -> Result<ComposedPlugin, AuthorError> {
    let state_manager = Arc::new(StateManager::new());
    let mgmt_state = Arc::clone(&state_manager);
    let cmd_state = Arc::clone(&state_manager);

    PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .management(management::registration(), move |call| {
            let state = Arc::clone(&mgmt_state);
            async move { management::handle(&state, call).await }
        })?
        .command_line(command::command_registration(), move |call| {
            let state = Arc::clone(&cmd_state);
            async move { command::command_line(&state, call).await }
        })?
        .build()
}
