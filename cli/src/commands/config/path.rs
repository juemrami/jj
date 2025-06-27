// Copyright 2020 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::io::Write as _;

use jj_lib::file_util;
use tracing::instrument;

use super::ConfigLevelArgs;
use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::ui::Ui;

/// Print the paths to the config files
///
/// A config file at that path may or may not exist.
///
/// See `jj config edit` if you'd like to immediately edit a file.
#[derive(clap::Args, Clone, Debug)]
pub struct ConfigPathArgs {
    #[command(flatten)]
    pub level: ConfigLevelArgs,
}

#[instrument(skip_all)]
pub fn cmd_config_path(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &ConfigPathArgs,
) -> Result<(), CommandError> {
    for config_path in args.level.config_paths(command.config_env())? {
        let path_bytes = file_util::path_to_bytes(config_path).map_err(user_error)?;
        ui.stdout().write_all(path_bytes)?;
        writeln!(ui.stdout())?;
    }
    Ok(())
}
