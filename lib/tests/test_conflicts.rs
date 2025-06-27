// Copyright 2021 The Jujutsu Authors
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

use indoc::indoc;
use itertools::Itertools as _;
use jj_lib::backend::FileId;
use jj_lib::conflicts::ConflictMarkerStyle;
use jj_lib::conflicts::MIN_CONFLICT_MARKER_LEN;
use jj_lib::conflicts::choose_materialized_conflict_marker_len;
use jj_lib::conflicts::extract_as_single_hunk;
use jj_lib::conflicts::materialize_merge_result_to_bytes;
use jj_lib::conflicts::parse_conflict;
use jj_lib::conflicts::update_from_content;
use jj_lib::merge::Merge;
use jj_lib::repo::Repo as _;
use jj_lib::repo_path::RepoPath;
use jj_lib::store::Store;
use pollster::FutureExt as _;
use testutils::TestRepo;
use testutils::read_file;
use testutils::repo_path;

#[test]
fn test_materialize_conflict_basic() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 3
            line 4
            line 5
        "},
    );
    let left_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            left 3.1
            left 3.2
            left 3.3
            line 4
            line 5
        "},
    );
    let right_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            right 3.1
            line 4
            line 5
        "},
    );

    // The left side should come first. The diff should be use the smaller (right)
    // side, and the left side should be a snapshot.
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(left_id.clone()), Some(right_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    line 2
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    left 3.1
    left 3.2
    left 3.3
    %%%%%%% Changes from base to side #2
    -line 3
    +right 3.1
    >>>>>>> Conflict 1 of 1 ends
    line 4
    line 5
    "
    );
    // Swap the positive terms in the conflict. The diff should still use the right
    // side, but now the right side should come first.
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(right_id.clone()), Some(left_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    line 2
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base to side #1
    -line 3
    +right 3.1
    +++++++ Contents of side #2
    left 3.1
    left 3.2
    left 3.3
    >>>>>>> Conflict 1 of 1 ends
    line 4
    line 5
    "
    );
    // Test materializing "snapshot" conflict markers
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(left_id.clone()), Some(right_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Snapshot),
        @r"
    line 1
    line 2
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    left 3.1
    left 3.2
    left 3.3
    ------- Contents of base
    line 3
    +++++++ Contents of side #2
    right 3.1
    >>>>>>> Conflict 1 of 1 ends
    line 4
    line 5
    "
    );
    // Test materializing "git" conflict markers
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(left_id.clone()), Some(right_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Git),
        @r"
    line 1
    line 2
    <<<<<<< Side #1 (Conflict 1 of 1)
    left 3.1
    left 3.2
    left 3.3
    ||||||| Base
    line 3
    =======
    right 3.1
    >>>>>>> Side #2 (Conflict 1 of 1 ends)
    line 4
    line 5
    "
    );
}

#[test]
fn test_materialize_conflict_three_sides() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_1_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 base
            line 3 base
            line 4 base
            line 5
        "},
    );
    let base_2_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 base
            line 5
        "},
    );
    let a_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 a.1
            line 3 a.2
            line 4 base
            line 5
        "},
    );
    let b_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 b.1
            line 3 base
            line 4 b.2
            line 5
        "},
    );
    let c_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 base
            line 3 c.2
            line 5
        "},
    );

    let conflict = Merge::from_removes_adds(
        vec![Some(base_1_id.clone()), Some(base_2_id.clone())],
        vec![Some(a_id.clone()), Some(b_id.clone()), Some(c_id.clone())],
    );
    // Test materializing "diff" conflict markers
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base #1 to side #1
    -line 2 base
    -line 3 base
    +line 2 a.1
    +line 3 a.2
     line 4 base
    +++++++ Contents of side #2
    line 2 b.1
    line 3 base
    line 4 b.2
    %%%%%%% Changes from base #2 to side #3
     line 2 base
    +line 3 c.2
    >>>>>>> Conflict 1 of 1 ends
    line 5
    "
    );
    // Test materializing "snapshot" conflict markers
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Snapshot),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    line 2 a.1
    line 3 a.2
    line 4 base
    ------- Contents of base #1
    line 2 base
    line 3 base
    line 4 base
    +++++++ Contents of side #2
    line 2 b.1
    line 3 base
    line 4 b.2
    ------- Contents of base #2
    line 2 base
    +++++++ Contents of side #3
    line 2 base
    line 3 c.2
    >>>>>>> Conflict 1 of 1 ends
    line 5
    "
    );
    // Test materializing "git" conflict markers (falls back to "snapshot" since
    // "git" conflict markers don't support more than 2 sides)
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Git),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    line 2 a.1
    line 3 a.2
    line 4 base
    ------- Contents of base #1
    line 2 base
    line 3 base
    line 4 base
    +++++++ Contents of side #2
    line 2 b.1
    line 3 base
    line 4 b.2
    ------- Contents of base #2
    line 2 base
    +++++++ Contents of side #3
    line 2 base
    line 3 c.2
    >>>>>>> Conflict 1 of 1 ends
    line 5
    "
    );
}

#[test]
fn test_materialize_conflict_multi_rebase_conflicts() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    // Create changes (a, b, c) on top of the base, and linearize them.
    let path = repo_path("file");
    let base_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 base
            line 3
        "},
    );
    let a_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 a.1
            line 2 a.2
            line 2 a.3
            line 3
        "},
    );
    let b_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 b.1
            line 2 b.2
            line 3
        "},
    );
    let c_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 c.1
            line 3
        "},
    );

    // The order of (a, b, c) should be preserved. For all cases, the "a" side
    // should be a snapshot.
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone()), Some(base_id.clone())],
        vec![Some(a_id.clone()), Some(b_id.clone()), Some(c_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    line 2 a.1
    line 2 a.2
    line 2 a.3
    %%%%%%% Changes from base #1 to side #2
    -line 2 base
    +line 2 b.1
    +line 2 b.2
    %%%%%%% Changes from base #2 to side #3
    -line 2 base
    +line 2 c.1
    >>>>>>> Conflict 1 of 1 ends
    line 3
    "
    );
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone()), Some(base_id.clone())],
        vec![Some(c_id.clone()), Some(b_id.clone()), Some(a_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base #1 to side #1
    -line 2 base
    +line 2 c.1
    %%%%%%% Changes from base #2 to side #2
    -line 2 base
    +line 2 b.1
    +line 2 b.2
    +++++++ Contents of side #3
    line 2 a.1
    line 2 a.2
    line 2 a.3
    >>>>>>> Conflict 1 of 1 ends
    line 3
    "
    );
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone()), Some(base_id.clone())],
        vec![Some(c_id.clone()), Some(a_id.clone()), Some(b_id.clone())],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base #1 to side #1
    -line 2 base
    +line 2 c.1
    +++++++ Contents of side #2
    line 2 a.1
    line 2 a.2
    line 2 a.3
    %%%%%%% Changes from base #2 to side #3
    -line 2 base
    +line 2 b.1
    +line 2 b.2
    >>>>>>> Conflict 1 of 1 ends
    line 3
    "
    );
}

//  TODO: With options
#[test]
fn test_materialize_parse_roundtrip() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 3
            line 4
            line 5
        "},
    );
    let left_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1 left
            line 2 left
            line 3
            line 4
            line 5 left
        "},
    );
    let right_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1 right
            line 2
            line 3
            line 4 right
            line 5 right
        "},
    );

    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(left_id.clone()), Some(right_id.clone())],
    );
    let materialized =
        materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(
        materialized,
        @r"
    <<<<<<< Conflict 1 of 2
    +++++++ Contents of side #1
    line 1 left
    line 2 left
    %%%%%%% Changes from base to side #2
    -line 1
    +line 1 right
     line 2
    >>>>>>> Conflict 1 of 2 ends
    line 3
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
     line 4
    -line 5
    +line 5 left
    +++++++ Contents of side #2
    line 4 right
    line 5 right
    >>>>>>> Conflict 2 of 2 ends
    "
    );

    // The first add should always be from the left side
    insta::assert_debug_snapshot!(
        parse_conflict(materialized.as_bytes(), conflict.num_sides(), MIN_CONFLICT_MARKER_LEN),
        @r#"
    Some(
        [
            Conflicted(
                [
                    "line 1 left\nline 2 left\n",
                    "line 1\nline 2\n",
                    "line 1 right\nline 2\n",
                ],
            ),
            Resolved(
                "line 3\n",
            ),
            Conflicted(
                [
                    "line 4\nline 5 left\n",
                    "line 4\nline 5\n",
                    "line 4 right\nline 5 right\n",
                ],
            ),
        ],
    )
    "#);
}

#[test]
fn test_materialize_parse_roundtrip_different_markers() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 base
            line 3 base
            line 4 base
            line 5
        "},
    );
    let a_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 a.1
            line 3 a.2
            line 4 base
            line 5
        "},
    );
    let b_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 b.1
            line 3 base
            line 4 b.2
            line 5
        "},
    );

    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(a_id.clone()), Some(b_id.clone())],
    );

    let all_styles = [
        ConflictMarkerStyle::Diff,
        ConflictMarkerStyle::Snapshot,
        ConflictMarkerStyle::Git,
    ];

    // For every pair of conflict marker styles, materialize the conflict using the
    // first style and parse it using the second. It should return the same result
    // regardless of the conflict markers used for materialization and parsing.
    for materialize_style in all_styles {
        let materialized = materialize_conflict_string(store, path, &conflict, materialize_style);
        for parse_style in all_styles {
            let parsed = update_from_content(
                &conflict,
                store,
                path,
                materialized.as_bytes(),
                parse_style,
                MIN_CONFLICT_MARKER_LEN,
            )
            .block_on()
            .unwrap();

            assert_eq!(
                parsed, conflict,
                "parse {materialize_style:?} conflict markers with {parse_style:?}"
            );
        }
    }
}

#[test]
fn test_materialize_conflict_no_newlines_at_eof() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(store, path, "base");
    let left_empty_id = testutils::write_file(store, path, "");
    let right_id = testutils::write_file(store, path, "right");

    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(left_empty_id.clone()), Some(right_id.clone())],
    );
    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(materialized,
        @r"
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base to side #1 (adds terminating newline)
    -base
    +++++++ Contents of side #2 (no terminating newline)
    right
    >>>>>>> Conflict 1 of 1 ends
    "
    );
    // The conflict markers are parsed with the trailing newline, but it is removed
    // by `update_from_content`
    insta::assert_debug_snapshot!(
        parse_conflict(
            materialized.as_bytes(),
            conflict.num_sides(),
            MIN_CONFLICT_MARKER_LEN
        ),
        @r#"
    Some(
        [
            Conflicted(
                [
                    "",
                    "base\n",
                    "right\n",
                ],
            ),
        ],
    )
    "#);
}

#[test]
fn test_materialize_conflict_modify_delete() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 3
            line 4
            line 5
        "},
    );
    let modified_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            modified
            line 4
            line 5
        "},
    );
    let deleted_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 4
            line 5
        "},
    );

    // left modifies a line, right deletes the same line.
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(modified_id.clone()), Some(deleted_id.clone())],
    );
    insta::assert_snapshot!(&materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff), @r"
    line 1
    line 2
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    modified
    %%%%%%% Changes from base to side #2
    -line 3
    >>>>>>> Conflict 1 of 1 ends
    line 4
    line 5
    "
    );

    // right modifies a line, left deletes the same line.
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(deleted_id.clone()), Some(modified_id.clone())],
    );
    insta::assert_snapshot!(&materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff), @r"
    line 1
    line 2
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base to side #1
    -line 3
    +++++++ Contents of side #2
    modified
    >>>>>>> Conflict 1 of 1 ends
    line 4
    line 5
    "
    );

    // modify/delete conflict at the file level
    let conflict = Merge::from_removes_adds(
        vec![Some(base_id.clone())],
        vec![Some(modified_id.clone()), None],
    );
    insta::assert_snapshot!(&materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff), @r"
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base to side #1
     line 1
     line 2
    -line 3
    +modified
     line 4
     line 5
    +++++++ Contents of side #2
    >>>>>>> Conflict 1 of 1 ends
    "
    );
}

#[test]
fn test_materialize_conflict_two_forward_diffs() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    // Create conflict A-B+B-C+D-E+C. This is designed to tempt the algorithm to
    // produce a negative snapshot at the end like this:
    // <<<<
    // ====
    // A
    // %%%%
    //  B
    // ++++
    // D
    // %%%%
    //  C
    // ----
    // E
    // >>>>
    // TODO: Maybe we should never have negative snapshots
    let path = repo_path("file");
    let a_id = testutils::write_file(store, path, "A\n");
    let b_id = testutils::write_file(store, path, "B\n");
    let c_id = testutils::write_file(store, path, "C\n");
    let d_id = testutils::write_file(store, path, "D\n");
    let e_id = testutils::write_file(store, path, "E\n");

    let conflict = Merge::from_removes_adds(
        vec![Some(b_id.clone()), Some(c_id.clone()), Some(e_id.clone())],
        vec![
            Some(a_id.clone()),
            Some(b_id.clone()),
            Some(d_id.clone()),
            Some(c_id.clone()),
        ],
    );
    insta::assert_snapshot!(
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff),
        @r"
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    A
    %%%%%%% Changes from base #1 to side #2
     B
    +++++++ Contents of side #3
    D
    %%%%%%% Changes from base #2 to side #4
     C
    ------- Contents of base #3
    E
    >>>>>>> Conflict 1 of 1 ends
    "
    );
}

#[test]
fn test_parse_conflict_resolved() {
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
line 2
line 3
line 4
line 5
"},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_simple() {
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<<
            %%%%%%%
             line 2
            -line 3
            +left
             line 4
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\nleft\nline 4\n",
                    "line 2\nline 3\nline 4\n",
                    "right\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Text
            %%%%%%% Different text 
             line 2
            -line 3
            +left
             line 4
            +++++++ Yet <><>< more text
            right
            >>>>>>> More and more text
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\nleft\nline 4\n",
                    "line 2\nline 3\nline 4\n",
                    "right\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test "snapshot" style
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Random text
            +++++++ Random text
            line 3.1
            line 3.2
            ------- Random text
            line 3
            line 4
            +++++++ Random text
            line 3
            line 4.1
            >>>>>>> Random text
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 3.1\nline 3.2\n",
                    "line 3\nline 4\n",
                    "line 3\nline 4.1\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test "snapshot" style with reordered sections
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Random text
            ------- Random text
            line 3
            line 4
            +++++++ Random text
            line 3.1
            line 3.2
            +++++++ Random text
            line 3
            line 4.1
            >>>>>>> Random text
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 3.1\nline 3.2\n",
                    "line 3\nline 4\n",
                    "line 3\nline 4.1\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test "git" style
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Side #1
            line 3.1
            line 3.2
            ||||||| Base
            line 3
            line 4
            ======= Side #2
            line 3
            line 4.1
            >>>>>>> End
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 3.1\nline 3.2\n",
                    "line 3\nline 4\n",
                    "line 3\nline 4.1\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test "git" style with empty side 1
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Side #1
            ||||||| Base
            line 3
            line 4
            ======= Side #2
            line 3.1
            line 4.1
            >>>>>>> End
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "",
                    "line 3\nline 4\n",
                    "line 3.1\nline 4.1\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // The conflict markers are longer than the originally materialized markers, but
    // we allow them to parse anyway
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<<<<<<
            %%%%%%%%%%%
             line 2
            -line 3
            +left
             line 4
            +++++++++++
            right
            >>>>>>>>>>>
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\nleft\nline 4\n",
                    "line 2\nline 3\nline 4\n",
                    "right\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
}

#[test]
fn test_parse_conflict_multi_way() {
    insta::assert_debug_snapshot!(
        parse_conflict(
            indoc! {b"
                line 1
                <<<<<<<
                %%%%%%%
                 line 2
                -line 3
                +left
                 line 4
                +++++++
                right
                %%%%%%%
                 line 2
                +forward
                 line 3
                 line 4
                >>>>>>>
                line 5
                "},
            3,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\nleft\nline 4\n",
                    "line 2\nline 3\nline 4\n",
                    "right\n",
                    "line 2\nline 3\nline 4\n",
                    "line 2\nforward\nline 3\nline 4\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Random text
            %%%%%%% Random text
             line 2
            -line 3
            +left
             line 4
            +++++++ Random text
            right
            %%%%%%% Random text
             line 2
            +forward
             line 3
             line 4
            >>>>>>> Random text
            line 5
            "},
            3,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\nleft\nline 4\n",
                    "line 2\nline 3\nline 4\n",
                    "right\n",
                    "line 2\nline 3\nline 4\n",
                    "line 2\nforward\nline 3\nline 4\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test "snapshot" style
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Random text
            +++++++ Random text
            line 3.1
            line 3.2
            +++++++ Random text
            line 3
            line 4.1
            ------- Random text
            line 3
            line 4
            ------- Random text
            line 3
            +++++++ Random text
            line 3
            line 4
            >>>>>>> Random text
            line 5
            "},
            3,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 3.1\nline 3.2\n",
                    "line 3\nline 4\n",
                    "line 3\nline 4.1\n",
                    "line 3\n",
                    "line 3\nline 4\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
}

#[test]
fn test_parse_conflict_crlf_markers() {
    // Conflict markers should be recognized even with CRLF
    insta::assert_debug_snapshot!(
        parse_conflict(
            indoc! {b"
            line 1\r
            <<<<<<<\r
            +++++++\r
            left\r
            -------\r
            base\r
            +++++++\r
            right\r
            >>>>>>>\r
            line 5\r
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\r\n",
            ),
            Conflicted(
                [
                    "left\r\n",
                    "base\r\n",
                    "right\r\n",
                ],
            ),
            Resolved(
                "line 5\r\n",
            ),
        ],
    )
    "#
    );
}

#[test]
fn test_parse_conflict_diff_stripped_whitespace() {
    // Should be able to parse conflict even if diff contains empty line (without
    // even a leading space, which is sometimes stripped by text editors)
    insta::assert_debug_snapshot!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            %%%%%%%
             line 2

            -line 3
            +left
            \r
             line 4
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "line 2\n\nleft\n\r\nline 4\n",
                    "line 2\n\nline 3\n\r\nline 4\n",
                    "right\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
}

#[test]
fn test_parse_conflict_wrong_arity() {
    // Valid conflict marker but it has fewer sides than the caller expected
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            %%%%%%%
             line 2
            -line 3
            +left
             line 4
            +++++++
            right
            >>>>>>>
            line 5
            "},
            3,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_malformed_missing_removes() {
    // Right number of adds but missing removes
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            +++++++
            left
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_malformed_marker() {
    // The conflict marker is missing `%%%%%%%`
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
             line 2
            -line 3
            +left
             line 4
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_malformed_diff() {
    // The diff part is invalid (missing space before "line 4")
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            %%%%%%%
             line 2
            -line 3
            +left
            line 4
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_snapshot_missing_header() {
    // The "+++++++" header is missing
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            left
            -------
            base
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_wrong_git_style() {
    // The "|||||||" section is missing
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            left
            =======
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_git_reordered_headers() {
    // The "=======" header must come after the "|||||||" header
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            left
            =======
            right
            |||||||
            base
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_git_too_many_sides() {
    // Git-style conflicts only allow 2 sides
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            a
            |||||||
            b
            =======
            c
            |||||||
            d
            =======
            e
            >>>>>>>
            line 5
            "},
            3,
            7
        ),
        None
    );
}

#[test]
fn test_parse_conflict_mixed_header_styles() {
    // "|||||||" can't be used in place of "-------"
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            +++++++
            left
            |||||||
            base
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
    // "+++++++" can't be used in place of "======="
    assert_eq!(
        parse_conflict(
            indoc! {b"
            line 1
            <<<<<<<
            left
            |||||||
            base
            +++++++
            right
            >>>>>>>
            line 5
            "},
            2,
            7
        ),
        None
    );
    // Test Git-style markers are ignored inside of JJ-style conflict
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Conflict 1 of 1
            +++++++ Contents of side #1
            ======= ignored
            ------- Contents of base
            ||||||| ignored
            +++++++ Contents of side #2
            >>>>>>> Conflict 1 of 1 ends
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "======= ignored\n",
                    "||||||| ignored\n",
                    "",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
    // Test JJ-style markers are ignored inside of Git-style conflict
    insta::assert_debug_snapshot!(
        parse_conflict(indoc! {b"
            line 1
            <<<<<<< Side #1 (Conflict 1 of 1)
            ||||||| Base
            ------- ignored
            %%%%%%% ignored
            =======
            +++++++ ignored
            >>>>>>> Side #2 (Conflict 1 of 1 ends)
            line 5
            "},
            2,
            7
        ),
        @r#"
    Some(
        [
            Resolved(
                "line 1\n",
            ),
            Conflicted(
                [
                    "",
                    "------- ignored\n%%%%%%% ignored\n",
                    "+++++++ ignored\n",
                ],
            ),
            Resolved(
                "line 5\n",
            ),
        ],
    )
    "#
    );
}

#[test]
fn test_update_conflict_from_content() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("dir/file");
    let base_file_id = testutils::write_file(store, path, "line 1\nline 2\nline 3\n");
    let left_file_id = testutils::write_file(store, path, "left 1\nline 2\nleft 3\n");
    let right_file_id = testutils::write_file(store, path, "right 1\nline 2\nright 3\n");
    let conflict = Merge::from_removes_adds(
        vec![Some(base_file_id.clone())],
        vec![Some(left_file_id.clone()), Some(right_file_id.clone())],
    );

    // If the content is unchanged compared to the materialized value, we get the
    // old conflict id back.
    let materialized =
        materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    let parse = |content| {
        update_from_content(
            &conflict,
            store,
            path,
            content,
            ConflictMarkerStyle::Diff,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap()
    };
    assert_eq!(parse(materialized.as_bytes()), conflict);

    // If the conflict is resolved, we get None back to indicate that.
    let expected_file_id = testutils::write_file(store, path, "resolved 1\nline 2\nresolved 3\n");
    assert_eq!(
        parse(b"resolved 1\nline 2\nresolved 3\n"),
        Merge::normal(expected_file_id)
    );

    // If the conflict is partially resolved, we get a new conflict back.
    let new_conflict = parse(
        b"resolved 1\nline 2\n<<<<<<<\n%%%%%%%\n-line 3\n+left 3\n+++++++\nright 3\n>>>>>>>\n",
    );
    assert_ne!(new_conflict, conflict);
    // Calculate expected new FileIds
    let new_base_file_id = testutils::write_file(store, path, "resolved 1\nline 2\nline 3\n");
    let new_left_file_id = testutils::write_file(store, path, "resolved 1\nline 2\nleft 3\n");
    let new_right_file_id = testutils::write_file(store, path, "resolved 1\nline 2\nright 3\n");
    assert_eq!(
        new_conflict,
        Merge::from_removes_adds(
            vec![Some(new_base_file_id.clone())],
            vec![
                Some(new_left_file_id.clone()),
                Some(new_right_file_id.clone())
            ]
        )
    );
}

#[test]
fn test_update_conflict_from_content_modify_delete() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("dir/file");
    let before_file_id = testutils::write_file(store, path, "line 1\nline 2 before\nline 3\n");
    let after_file_id = testutils::write_file(store, path, "line 1\nline 2 after\nline 3\n");
    let conflict =
        Merge::from_removes_adds(vec![Some(before_file_id)], vec![Some(after_file_id), None]);

    // If the content is unchanged compared to the materialized value, we get the
    // old conflict id back.
    let materialized =
        materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    let parse = |content| {
        update_from_content(
            &conflict,
            store,
            path,
            content,
            ConflictMarkerStyle::Diff,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap()
    };
    assert_eq!(parse(materialized.as_bytes()), conflict);

    // If the conflict is resolved, we get None back to indicate that.
    let expected_file_id = testutils::write_file(store, path, "resolved\n");
    assert_eq!(parse(b"resolved\n"), Merge::normal(expected_file_id));

    // If the conflict is modified, we get a new conflict back.
    let new_conflict = parse(
        b"<<<<<<<\n%%%%%%%\n line 1\n-line 2 before\n+line 2 modified after\n line 3\n+++++++\n>>>>>>>\n",
    );
    // Calculate expected new FileIds
    let new_base_file_id = testutils::write_file(store, path, "line 1\nline 2 before\nline 3\n");
    let new_left_file_id =
        testutils::write_file(store, path, "line 1\nline 2 modified after\nline 3\n");

    assert_eq!(
        new_conflict,
        Merge::from_removes_adds(
            vec![Some(new_base_file_id.clone())],
            vec![Some(new_left_file_id.clone()), None]
        )
    );
}

#[test]
fn test_update_conflict_from_content_simplified_conflict() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("dir/file");
    let base_file_id = testutils::write_file(store, path, "line 1\nline 2\nline 3\n");
    let left_file_id = testutils::write_file(store, path, "left 1\nline 2\nleft 3\n");
    let right_file_id = testutils::write_file(store, path, "right 1\nline 2\nright 3\n");
    // Conflict: left - base + base - base + right
    let conflict = Merge::from_removes_adds(
        vec![Some(base_file_id.clone()), Some(base_file_id.clone())],
        vec![
            Some(left_file_id.clone()),
            Some(base_file_id.clone()),
            Some(right_file_id.clone()),
        ],
    );
    let simplified_conflict = conflict.simplify();

    // If the content is unchanged compared to the materialized value, we get the
    // old conflict id back.
    let materialized =
        materialize_conflict_string(store, path, &simplified_conflict, ConflictMarkerStyle::Diff);
    let parse = |content| {
        update_from_content(
            &conflict,
            store,
            path,
            content,
            ConflictMarkerStyle::Diff,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap()
    };
    insta::assert_snapshot!(
        materialized,
        @r"
    <<<<<<< Conflict 1 of 2
    %%%%%%% Changes from base to side #1
    -line 1
    +left 1
    +++++++ Contents of side #2
    right 1
    >>>>>>> Conflict 1 of 2 ends
    line 2
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
    -line 3
    +left 3
    +++++++ Contents of side #2
    right 3
    >>>>>>> Conflict 2 of 2 ends
    "
    );
    assert_eq!(parse(materialized.as_bytes()), conflict);

    // If the conflict is resolved, we get a normal merge back to indicate that.
    let expected_file_id = testutils::write_file(store, path, "resolved 1\nline 2\nresolved 3\n");
    assert_eq!(
        parse(b"resolved 1\nline 2\nresolved 3\n"),
        Merge::normal(expected_file_id)
    );

    // If the conflict is partially resolved, we get a new conflict back.
    let new_conflict = parse(indoc! {b"
        resolved 1
        line 2
        <<<<<<< Conflict 2 of 2
        %%%%%%% Changes from base to side #1
        -edited line 3
        +edited left 3
        +++++++ Contents of side #2
        edited right 3
        >>>>>>> Conflict 2 of 2 ends
    "});
    assert_ne!(new_conflict, conflict);
    // Calculate expected new FileIds
    let new_base_file_id =
        testutils::write_file(store, path, "resolved 1\nline 2\nedited line 3\n");
    let new_left_file_id =
        testutils::write_file(store, path, "resolved 1\nline 2\nedited left 3\n");
    let new_right_file_id =
        testutils::write_file(store, path, "resolved 1\nline 2\nedited right 3\n");
    assert_eq!(
        new_conflict,
        Merge::from_removes_adds(
            vec![Some(base_file_id.clone()), Some(new_base_file_id.clone())],
            vec![
                Some(new_left_file_id.clone()),
                Some(base_file_id.clone()),
                Some(new_right_file_id.clone())
            ]
        )
    );
}

#[test]
fn test_update_conflict_from_content_with_long_markers() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    // Create conflicts which contain conflict markers of varying lengths
    let path = repo_path("dir/file");
    let base_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 3
        "},
    );
    let left_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            <<<< left 1
            line 2
            <<<<<<<<<<<< left 3
        "},
    );
    let right_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            >>>>>>> right 1
            line 2
            >>>>>>>>>>>> right 3
        "},
    );
    let conflict = Merge::from_removes_adds(
        vec![Some(base_file_id.clone())],
        vec![Some(left_file_id.clone()), Some(right_file_id.clone())],
    );

    // The conflict should be materialized using long conflict markers
    let materialized_marker_len = choose_materialized_conflict_marker_len(
        &extract_as_single_hunk(&conflict, store, path)
            .block_on()
            .unwrap(),
    );
    assert!(materialized_marker_len > MIN_CONFLICT_MARKER_LEN);
    let materialized =
        materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Snapshot);
    insta::assert_snapshot!(materialized, @r"
    <<<<<<<<<<<<<<<< Conflict 1 of 2
    ++++++++++++++++ Contents of side #1
    <<<< left 1
    ---------------- Contents of base
    line 1
    ++++++++++++++++ Contents of side #2
    >>>>>>> right 1
    >>>>>>>>>>>>>>>> Conflict 1 of 2 ends
    line 2
    <<<<<<<<<<<<<<<< Conflict 2 of 2
    ++++++++++++++++ Contents of side #1
    <<<<<<<<<<<< left 3
    ---------------- Contents of base
    line 3
    ++++++++++++++++ Contents of side #2
    >>>>>>>>>>>> right 3
    >>>>>>>>>>>>>>>> Conflict 2 of 2 ends
    "
    );

    // Parse the conflict markers using a different conflict marker style. This is
    // to avoid the two versions of the file being obviously identical, so that we
    // can test the actual parsing logic.
    let parse = |conflict, content| {
        update_from_content(
            conflict,
            store,
            path,
            content,
            ConflictMarkerStyle::Diff,
            materialized_marker_len,
        )
        .block_on()
        .unwrap()
    };
    assert_eq!(parse(&conflict, materialized.as_bytes()), conflict);

    // Test resolving the conflict, leaving some fake conflict markers which should
    // not be parsed since they are too short
    let resolved_file_contents = indoc! {"
        <<<<<<<<<<<< not a real conflict!
        ++++++++++++
        left
        ------------
        base
        ++++++++++++
        right
        >>>>>>>>>>>>
    "};
    let resolved_file_id = testutils::write_file(store, path, resolved_file_contents);
    assert_eq!(
        parse(&conflict, resolved_file_contents.as_bytes()),
        Merge::normal(resolved_file_id)
    );

    // Resolve one of the conflicts, decreasing the minimum conflict marker length
    let new_conflict_contents = indoc! {"
        <<<<<<<<<<<<<<<< Conflict 1 of 2
        ++++++++++++++++ Contents of side #1
        <<<< left 1
        ---------------- Contents of base
        line 1
        ++++++++++++++++ Contents of side #2
        >>>>>>> right 1
        >>>>>>>>>>>>>>>> Conflict 1 of 2 ends
        line 2
        line 3
    "};

    // Confirm that the new conflict parsed correctly
    let new_conflict = parse(&conflict, new_conflict_contents.as_bytes());
    assert_eq!(new_conflict.num_sides(), 2);
    let new_conflict_terms = new_conflict
        .iter()
        .map(|id| String::from_utf8(read_file(store, path, id.as_ref().unwrap())).unwrap())
        .collect_vec();
    let [new_left_side, new_base, new_right_side] = new_conflict_terms.as_slice() else {
        unreachable!()
    };
    insta::assert_snapshot!(new_left_side, @r"
    <<<< left 1
    line 2
    line 3
    ");
    insta::assert_snapshot!(new_base, @r"
    line 1
    line 2
    line 3
    ");
    insta::assert_snapshot!(new_right_side, @r"
    >>>>>>> right 1
    line 2
    line 3
    ");

    // The conflict markers should still parse in future snapshots even though
    // they're now longer than necessary
    assert_eq!(
        parse(&new_conflict, new_conflict_contents.as_bytes()),
        new_conflict
    );

    // If we add back the second conflict, it should still be parsed correctly
    // (the fake conflict markers shouldn't be interpreted as conflict markers
    // still, since they aren't the longest ones in the file).
    assert_eq!(parse(&new_conflict, materialized.as_bytes()), conflict);

    // If the new conflict is materialized again, it should have shorter
    // conflict markers now
    insta::assert_snapshot!(
        materialize_conflict_string(store, path, &new_conflict, ConflictMarkerStyle::Snapshot),
        @r"
    <<<<<<<<<<< Conflict 1 of 1
    +++++++++++ Contents of side #1
    <<<< left 1
    ----------- Contents of base
    line 1
    +++++++++++ Contents of side #2
    >>>>>>> right 1
    >>>>>>>>>>> Conflict 1 of 1 ends
    line 2
    line 3
    "
    );
}

#[test]
fn test_update_conflict_from_content_no_eol() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    let base_id = testutils::write_file(store, path, "line 1\nline 2\nline 3\nbase");
    let left_empty_id =
        testutils::write_file(store, path, "line 1\nline 2 left\nline 3\nbase\nleft\n");
    let right_id = testutils::write_file(store, path, "line 1\nline 2 right\nline 3\nright");

    let conflict = Merge::from_removes_adds(
        vec![Some(base_id)],
        vec![Some(left_empty_id), Some(right_id)],
    );

    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(materialized,
        @r"
    line 1
    <<<<<<< Conflict 1 of 2
    %%%%%%% Changes from base to side #1
    -line 2
    +line 2 left
    +++++++ Contents of side #2
    line 2 right
    >>>>>>> Conflict 1 of 2 ends
    line 3
    <<<<<<< Conflict 2 of 2
    +++++++ Contents of side #1
    base
    left
    %%%%%%% Changes from base to side #2 (no terminating newline)
    -base
    +right
    >>>>>>> Conflict 2 of 2 ends
    "
    );
    // Parse with "snapshot" markers to ensure the file is actually parsed
    assert_eq!(
        update_from_content(
            &conflict,
            store,
            path,
            materialized.as_bytes(),
            ConflictMarkerStyle::Snapshot,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap(),
        conflict
    );

    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Snapshot);
    insta::assert_snapshot!(materialized,
        @r"
    line 1
    <<<<<<< Conflict 1 of 2
    +++++++ Contents of side #1
    line 2 left
    ------- Contents of base
    line 2
    +++++++ Contents of side #2
    line 2 right
    >>>>>>> Conflict 1 of 2 ends
    line 3
    <<<<<<< Conflict 2 of 2
    +++++++ Contents of side #1
    base
    left
    ------- Contents of base (no terminating newline)
    base
    +++++++ Contents of side #2 (no terminating newline)
    right
    >>>>>>> Conflict 2 of 2 ends
    "
    );
    // Parse with "diff" markers to ensure the file is actually parsed
    assert_eq!(
        update_from_content(
            &conflict,
            store,
            path,
            materialized.as_bytes(),
            ConflictMarkerStyle::Diff,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap(),
        conflict
    );

    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Git);
    insta::assert_snapshot!(materialized,
        @r"
    line 1
    <<<<<<< Side #1 (Conflict 1 of 2)
    line 2 left
    ||||||| Base
    line 2
    =======
    line 2 right
    >>>>>>> Side #2 (Conflict 1 of 2 ends)
    line 3
    <<<<<<< Side #1 (Conflict 2 of 2)
    base
    left
    ||||||| Base
    base
    =======
    right
    >>>>>>> Side #2 (Conflict 2 of 2 ends)
    "
    );
    // Parse with "diff" markers to ensure the file is actually parsed
    assert_eq!(
        update_from_content(
            &conflict,
            store,
            path,
            materialized.as_bytes(),
            ConflictMarkerStyle::Diff,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap(),
        conflict
    );
}

#[test]
fn test_update_conflict_from_content_no_eol_in_diff_hunk() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    // Create a conflict with all 4 possible cases for diff "noeol" markers
    let side_1_id = testutils::write_file(store, path, "side\n");
    let base_1_id = testutils::write_file(store, path, "add newline\nline");
    let side_2_id = testutils::write_file(store, path, "add newline\nline\n");
    let base_2_id = testutils::write_file(store, path, "remove newline\nline\n");
    let side_3_id = testutils::write_file(store, path, "remove newline\nline");
    let base_3_id = testutils::write_file(store, path, "no newline\nline 1");
    let side_4_id = testutils::write_file(store, path, "no newline\nline 2");
    let base_4_id = testutils::write_file(store, path, "with newline\nline 1\n");
    let side_5_id = testutils::write_file(store, path, "with newline\nline 2\n");

    let conflict = Merge::from_removes_adds(
        vec![
            Some(base_1_id),
            Some(base_2_id),
            Some(base_3_id),
            Some(base_4_id),
        ],
        vec![
            Some(side_1_id),
            Some(side_2_id),
            Some(side_3_id),
            Some(side_4_id),
            Some(side_5_id),
        ],
    );

    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(materialized,
        @r"
    <<<<<<< Conflict 1 of 1
    +++++++ Contents of side #1
    side
    %%%%%%% Changes from base #1 to side #2 (adds terminating newline)
     add newline
    -line
    +line
    %%%%%%% Changes from base #2 to side #3 (removes terminating newline)
     remove newline
    -line
    +line
    %%%%%%% Changes from base #3 to side #4 (no terminating newline)
     no newline
    -line 1
    +line 2
    %%%%%%% Changes from base #4 to side #5
     with newline
    -line 1
    +line 2
    >>>>>>> Conflict 1 of 1 ends
    "
    );
    // Parse with "snapshot" markers to ensure the file is actually parsed
    assert_eq!(
        update_from_content(
            &conflict,
            store,
            path,
            materialized.as_bytes(),
            ConflictMarkerStyle::Snapshot,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap(),
        conflict
    );
}

#[test]
fn test_update_conflict_from_content_only_no_eol_change() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("file");
    // Create a conflict which would be resolved by the "A-B+A = A" rule if the
    // missing newline is wrongly ignored
    let left_id = testutils::write_file(store, path, "line 1\nline 2");
    let base_id = testutils::write_file(store, path, "line 1\n");
    let right_id = testutils::write_file(store, path, "line 1\nline 2\n");

    let conflict =
        Merge::from_removes_adds(vec![Some(base_id)], vec![Some(left_id), Some(right_id)]);

    let materialized =
        &materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(materialized,
        @r"
    line 1
    <<<<<<< Conflict 1 of 1
    %%%%%%% Changes from base to side #1 (removes terminating newline)
    +line 2
    +++++++ Contents of side #2
    line 2
    >>>>>>> Conflict 1 of 1 ends
    "
    );
    // Parse with "snapshot" markers to ensure the file is actually parsed
    assert_eq!(
        update_from_content(
            &conflict,
            store,
            path,
            materialized.as_bytes(),
            ConflictMarkerStyle::Snapshot,
            MIN_CONFLICT_MARKER_LEN,
        )
        .block_on()
        .unwrap(),
        conflict
    );
}

#[test]
fn test_update_from_content_malformed_conflict() {
    let test_repo = TestRepo::init();
    let store = test_repo.repo.store();

    let path = repo_path("dir/file");
    let base_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2
            line 3
            line 4
            line 5
        "},
    );
    let left_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 left
            line 3
            line 4 left
            line 5
        "},
    );
    let right_file_id = testutils::write_file(
        store,
        path,
        indoc! {"
            line 1
            line 2 right
            line 3
            line 4 right
            line 5
        "},
    );
    let conflict = Merge::from_removes_adds(
        vec![Some(base_file_id.clone())],
        vec![Some(left_file_id.clone()), Some(right_file_id.clone())],
    );

    // The conflict should be materialized with normal markers
    let materialized_marker_len = choose_materialized_conflict_marker_len(
        &extract_as_single_hunk(&conflict, store, path)
            .block_on()
            .unwrap(),
    );
    assert!(materialized_marker_len == MIN_CONFLICT_MARKER_LEN);

    let materialized =
        materialize_conflict_string(store, path, &conflict, ConflictMarkerStyle::Diff);
    insta::assert_snapshot!(materialized, @r"
    line 1
    <<<<<<< Conflict 1 of 2
    %%%%%%% Changes from base to side #1
    -line 2
    +line 2 left
    +++++++ Contents of side #2
    line 2 right
    >>>>>>> Conflict 1 of 2 ends
    line 3
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
    -line 4
    +line 4 left
    +++++++ Contents of side #2
    line 4 right
    >>>>>>> Conflict 2 of 2 ends
    line 5
    "
    );

    let parse = |conflict, content| {
        update_from_content(
            conflict,
            store,
            path,
            content,
            ConflictMarkerStyle::Diff,
            materialized_marker_len,
        )
        .block_on()
        .unwrap()
    };
    assert_eq!(parse(&conflict, materialized.as_bytes()), conflict);

    // Make a change to the second conflict that causes it to become invalid
    let new_conflict_contents = indoc! {"
        line 1
        <<<<<<< Conflict 1 of 2
        %%%%%%% Changes from base to side #1
        -line 2
        +line 2 left
        +++++++ Contents of side #2
        line 2 right
        >>>>>>> Conflict 1 of 2 ends
        line 3
        <<<<<<< Conflict 2 of 2
        %%%%%%% Changes from base to side #1
        -line 4
        +line 4 left
        line 4 right
        >>>>>>> Conflict 2 of 2 ends
        line 5
    "};
    // On the first snapshot, it will parse as a conflict containing conflict
    // markers as text
    let new_conflict = parse(&conflict, new_conflict_contents.as_bytes());
    assert_eq!(new_conflict.num_sides(), 2);
    let new_conflict_terms = new_conflict
        .iter()
        .map(|id| String::from_utf8(read_file(store, path, id.as_ref().unwrap())).unwrap())
        .collect_vec();
    let [new_left_side, new_base, new_right_side] = new_conflict_terms.as_slice() else {
        unreachable!()
    };
    insta::assert_snapshot!(new_left_side, @r"
    line 1
    line 2 left
    line 3
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
    -line 4
    +line 4 left
    line 4 right
    >>>>>>> Conflict 2 of 2 ends
    line 5
    ");
    insta::assert_snapshot!(new_base, @r"
    line 1
    line 2
    line 3
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
    -line 4
    +line 4 left
    line 4 right
    >>>>>>> Conflict 2 of 2 ends
    line 5
    ");
    insta::assert_snapshot!(new_right_side, @r"
    line 1
    line 2 right
    line 3
    <<<<<<< Conflict 2 of 2
    %%%%%%% Changes from base to side #1
    -line 4
    +line 4 left
    line 4 right
    >>>>>>> Conflict 2 of 2 ends
    line 5
    ");

    // Even though the file now contains markers of length 7, the materialized
    // markers of length 7 are still parsed
    let second_snapshot = parse(&new_conflict, new_conflict_contents.as_bytes());
    assert_eq!(second_snapshot, new_conflict);
}

fn materialize_conflict_string(
    store: &Store,
    path: &RepoPath,
    conflict: &Merge<Option<FileId>>,
    conflict_marker_style: ConflictMarkerStyle,
) -> String {
    let contents = extract_as_single_hunk(conflict, store, path)
        .block_on()
        .unwrap();
    String::from_utf8(materialize_merge_result_to_bytes(&contents, conflict_marker_style).into())
        .unwrap()
}
