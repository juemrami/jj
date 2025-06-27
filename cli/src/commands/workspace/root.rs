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

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::ui::Ui;

/// Show the current workspace root directory
#[derive(clap::Args, Clone, Debug)]
pub struct WorkspaceRootArgs {}

#[instrument(skip_all)]
pub fn cmd_workspace_root(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &WorkspaceRootArgs,
) -> Result<(), CommandError> {
    let loader = command.workspace_loader()?;
    let path_bytes = file_util::path_to_bytes(loader.workspace_root()).map_err(user_error)?;
    ui.stdout().write_all(path_bytes)?;
    writeln!(ui.stdout())?;
    Ok(())
}
