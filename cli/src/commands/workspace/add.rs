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

use std::fs;

use itertools::Itertools as _;
use jj_lib::commit::CommitIteratorExt as _;
use jj_lib::file_util;
use jj_lib::file_util::IoResultExt as _;
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::workspace::Workspace;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::cli_util::RevisionArg;
use crate::command_error::CommandError;
use crate::command_error::internal_error_with_message;
use crate::command_error::user_error;
use crate::ui::Ui;

/// How to handle sparse patterns when creating a new workspace.
#[derive(clap::ValueEnum, Clone, Debug, Eq, PartialEq)]
enum SparseInheritance {
    /// Copy all sparse patterns from the current workspace.
    Copy,
    /// Include all files in the new workspace.
    Full,
    /// Clear all files from the workspace (it will be empty).
    Empty,
}

/// Add a workspace
///
/// By default, the new workspace inherits the sparse patterns of the current
/// workspace. You can override this with the `--sparse-patterns` option.
#[derive(clap::Args, Clone, Debug)]
pub struct WorkspaceAddArgs {
    /// Where to create the new workspace
    destination: String,
    /// A name for the workspace
    ///
    /// To override the default, which is the basename of the destination
    /// directory.
    #[arg(long)]
    name: Option<WorkspaceNameBuf>,
    /// A list of parent revisions for the working-copy commit of the newly
    /// created workspace. You may specify nothing, or any number of parents.
    ///
    /// If no revisions are specified, the new workspace will be created, and
    /// its working-copy commit will exist on top of the parent(s) of the
    /// working-copy commit in the current workspace, i.e. they will share the
    /// same parent(s).
    ///
    /// If any revisions are specified, the new workspace will be created, and
    /// the new working-copy commit will be created with all these revisions as
    /// parents, i.e. the working-copy commit will exist as if you had run `jj
    /// new r1 r2 r3 ...`.
    #[arg(long, short, value_name = "REVSETS")]
    revision: Vec<RevisionArg>,
    /// How to handle sparse patterns when creating a new workspace.
    #[arg(long, value_enum, default_value_t = SparseInheritance::Copy)]
    sparse_patterns: SparseInheritance,
}

#[instrument(skip_all)]
pub fn cmd_workspace_add(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &WorkspaceAddArgs,
) -> Result<(), CommandError> {
    let old_workspace_command = command.workspace_helper(ui)?;
    let destination_path = command.cwd().join(&args.destination);
    if destination_path.exists() {
        return Err(user_error("Workspace already exists"));
    } else {
        fs::create_dir(&destination_path).context(&destination_path)?;
    }
    let workspace_name = if let Some(name) = &args.name {
        name.to_owned()
    } else {
        let file_name = destination_path.file_name().unwrap();
        file_name
            .to_str()
            .ok_or_else(|| user_error("Destination path is not valid UTF-8"))?
            .into()
    };
    let repo = old_workspace_command.repo();
    if repo.view().get_wc_commit_id(&workspace_name).is_some() {
        return Err(user_error(format!(
            "Workspace named '{name}' already exists",
            name = workspace_name.as_symbol()
        )));
    }

    let working_copy_factory = command.get_working_copy_factory()?;
    let repo_path = old_workspace_command.repo_path();
    // If we add per-workspace configuration, we'll need to reload settings for
    // the new workspace.
    let (new_workspace, repo) = Workspace::init_workspace_with_existing_repo(
        &destination_path,
        repo_path,
        repo,
        working_copy_factory,
        workspace_name.clone(),
    )?;
    writeln!(
        ui.status(),
        "Created workspace in \"{}\"",
        file_util::relative_path(command.cwd(), &destination_path).display()
    )?;
    // Show a warning if the user passed a path without a separator, since they
    // may have intended the argument to only be the name for the workspace.
    if !args.destination.contains(std::path::is_separator) {
        writeln!(
            ui.warning_default(),
            r#"Workspace created inside current directory. If this was unintentional, delete the "{}" directory and run `jj workspace forget {name}` to remove it."#,
            args.destination,
            name = workspace_name.as_symbol()
        )?;
    }

    let mut new_workspace_command = command.for_workable_repo(ui, new_workspace, repo)?;

    let sparsity = match args.sparse_patterns {
        SparseInheritance::Full => None,
        SparseInheritance::Empty => Some(vec![]),
        SparseInheritance::Copy => {
            let sparse_patterns = old_workspace_command
                .working_copy()
                .sparse_patterns()?
                .to_vec();
            Some(sparse_patterns)
        }
    };

    if let Some(sparse_patterns) = sparsity {
        let checkout_options = new_workspace_command.checkout_options();
        let (mut locked_ws, _wc_commit) = new_workspace_command.start_working_copy_mutation()?;
        locked_ws
            .locked_wc()
            .set_sparse_patterns(sparse_patterns, &checkout_options)
            .map_err(|err| internal_error_with_message("Failed to set sparse patterns", err))?;
        let operation_id = locked_ws.locked_wc().old_operation_id().clone();
        locked_ws.finish(operation_id)?;
    }

    let mut tx = new_workspace_command.start_transaction();

    // If no parent revisions are specified, create a working-copy commit based
    // on the parent of the current working-copy commit.
    let parents = if args.revision.is_empty() {
        // Check out parents of the current workspace's working-copy commit, or the
        // root if there is no working-copy commit in the current workspace.
        if let Some(old_wc_commit_id) = tx
            .base_repo()
            .view()
            .get_wc_commit_id(old_workspace_command.workspace_name())
        {
            tx.repo()
                .store()
                .get_commit(old_wc_commit_id)?
                .parents()
                .try_collect()?
        } else {
            vec![tx.repo().store().root_commit()]
        }
    } else {
        old_workspace_command
            .resolve_some_revsets_default_single(ui, &args.revision)?
            .iter()
            .map(|id| tx.repo().store().get_commit(id))
            .try_collect()?
    };

    let tree = merge_commit_trees(tx.repo(), &parents)?;
    let parent_ids = parents.iter().ids().cloned().collect_vec();
    let new_wc_commit = tx.repo_mut().new_commit(parent_ids, tree.id()).write()?;

    tx.edit(&new_wc_commit)?;
    tx.finish(
        ui,
        format!(
            "create initial working-copy commit in workspace {name}",
            name = workspace_name.as_symbol()
        ),
    )?;
    Ok(())
}
