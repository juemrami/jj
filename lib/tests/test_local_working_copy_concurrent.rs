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

use std::cmp::max;
use std::thread;

use assert_matches::assert_matches;
use jj_lib::repo::Repo as _;
use jj_lib::working_copy::CheckoutError;
use jj_lib::working_copy::CheckoutOptions;
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::workspace::Workspace;
use jj_lib::workspace::default_working_copy_factories;
use testutils::TestWorkspace;
use testutils::commit_with_tree;
use testutils::create_tree;
use testutils::repo_path;
use testutils::repo_path_buf;
use testutils::write_working_copy_file;

#[test]
fn test_concurrent_checkout() {
    // Test that we error out if a concurrent checkout is detected (i.e. if the
    // working-copy commit changed on disk after we read it).
    let settings = testutils::user_settings();
    let mut test_workspace1 = TestWorkspace::init_with_settings(&settings);
    let repo = test_workspace1.repo.clone();
    let workspace1_root = test_workspace1.workspace.workspace_root().to_owned();

    let tree_id1 = testutils::create_random_tree(&repo);
    let tree_id2 = testutils::create_random_tree(&repo);
    let tree_id3 = testutils::create_random_tree(&repo);
    let tree1 = repo.store().get_root_tree(&tree_id1).unwrap();
    let tree2 = repo.store().get_root_tree(&tree_id2).unwrap();
    let tree3 = repo.store().get_root_tree(&tree_id3).unwrap();
    let commit1 = commit_with_tree(repo.store(), tree1.id());
    let commit2 = commit_with_tree(repo.store(), tree2.id());
    let commit3 = commit_with_tree(repo.store(), tree3.id());

    // Check out tree1
    let ws1 = &mut test_workspace1.workspace;
    // The operation ID is not correct, but that doesn't matter for this test
    ws1.check_out(
        repo.op_id().clone(),
        None,
        &commit1,
        &CheckoutOptions::empty_for_test(),
    )
    .unwrap();

    // Check out tree2 from another process (simulated by another workspace
    // instance)
    {
        let mut ws2 = Workspace::load(
            &settings,
            &workspace1_root,
            &test_workspace1.env.default_store_factories(),
            &default_working_copy_factories(),
        )
        .unwrap();
        // Reload commit from the store associated with the workspace
        let repo = ws2.repo_loader().load_at(repo.operation()).unwrap();
        let commit2 = repo.store().get_commit(commit2.id()).unwrap();
        ws2.check_out(
            repo.op_id().clone(),
            Some(&tree_id1),
            &commit2,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    }

    // Checking out another tree (via the first workspace instance) should now fail.
    assert_matches!(
        ws1.check_out(
            repo.op_id().clone(),
            Some(&tree_id1),
            &commit3,
            &CheckoutOptions::empty_for_test()
        ),
        Err(CheckoutError::ConcurrentCheckout)
    );

    // Check that the tree2 is still checked out on disk.
    let ws3 = Workspace::load(
        &settings,
        &workspace1_root,
        &test_workspace1.env.default_store_factories(),
        &default_working_copy_factories(),
    )
    .unwrap();
    assert_eq!(*ws3.working_copy().tree_id().unwrap(), tree_id2);
}

#[test]
fn test_checkout_parallel() {
    // Test that concurrent checkouts by different processes (simulated by using
    // different repo instances) is safe.
    let settings = testutils::user_settings();
    let mut test_workspace = TestWorkspace::init_with_settings(&settings);
    let repo = &test_workspace.repo;
    let workspace_root = test_workspace.workspace.workspace_root().to_owned();

    let num_threads = max(num_cpus::get(), 4);
    let mut tree_ids = vec![];
    for i in 0..num_threads {
        let path = repo_path_buf(format!("file{i}"));
        let tree = create_tree(repo, &[(&path, "contents")]);
        tree_ids.push(tree.id());
    }

    // Create another tree just so we can test the update stats reliably from the
    // first update
    let tree = create_tree(repo, &[(repo_path("other file"), "contents")]);
    let commit = commit_with_tree(repo.store(), tree.id());
    test_workspace
        .workspace
        .check_out(
            repo.op_id().clone(),
            None,
            &commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();

    thread::scope(|s| {
        for tree_id in &tree_ids {
            let test_env = &test_workspace.env;
            let op_id = repo.op_id().clone();
            let tree_ids = tree_ids.clone();
            let commit = commit_with_tree(repo.store(), tree_id.clone());
            let settings = settings.clone();
            let workspace_root = workspace_root.clone();
            s.spawn(move || {
                let mut workspace = Workspace::load(
                    &settings,
                    &workspace_root,
                    &test_env.default_store_factories(),
                    &default_working_copy_factories(),
                )
                .unwrap();
                // Reload commit from the store associated with the workspace
                let repo = workspace.repo_loader().load_at(repo.operation()).unwrap();
                let commit = repo.store().get_commit(commit.id()).unwrap();
                // The operation ID is not correct, but that doesn't matter for this test
                let stats = workspace
                    .check_out(op_id, None, &commit, &CheckoutOptions::empty_for_test())
                    .unwrap();
                assert_eq!(stats.updated_files, 0);
                assert_eq!(stats.added_files, 1);
                assert_eq!(stats.removed_files, 1);
                // Check that the working copy contains one of the trees. We may see a
                // different tree than the one we just checked out, but since
                // write_tree() should take the same lock as check_out(), write_tree(,
                // &CheckoutOptions::empty_for_test()) should never produce a
                // different tree.
                let mut locked_ws = workspace.start_working_copy_mutation().unwrap();
                let (new_tree_id, _stats) = locked_ws
                    .locked_wc()
                    .snapshot(&SnapshotOptions::empty_for_test())
                    .unwrap();
                assert!(tree_ids.contains(&new_tree_id));
            });
        }
    });
}

#[test]
fn test_racy_checkout() {
    let mut test_workspace = TestWorkspace::init();
    let repo = &test_workspace.repo;
    let op_id = repo.op_id().clone();
    let workspace_root = test_workspace.workspace.workspace_root().to_owned();

    let path = repo_path("file");
    let tree = create_tree(repo, &[(path, "1")]);
    let commit = commit_with_tree(repo.store(), tree.id());

    let mut num_matches = 0;
    for _ in 0..100 {
        let ws = &mut test_workspace.workspace;
        ws.check_out(
            op_id.clone(),
            None,
            &commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(path.to_fs_path_unchecked(&workspace_root)).unwrap(),
            b"1".to_vec()
        );
        // A file written right after checkout (hopefully, from the test's perspective,
        // within the file system timestamp granularity) is detected as changed.
        write_working_copy_file(&workspace_root, path, "x");
        let modified_tree = test_workspace.snapshot().unwrap();
        if modified_tree.id() == tree.id() {
            num_matches += 1;
        }
        // Reset the state for the next round
        write_working_copy_file(&workspace_root, path, "1");
    }
    assert_eq!(num_matches, 0);
}
