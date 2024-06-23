// Copyright 2022 The Jujutsu Authors
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

use crate::common::{get_stdout_string, TestEnvironment};

#[test]
fn test_obslog_with_or_without_diff() {
    let test_env = TestEnvironment::default();
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    std::fs::write(repo_path.join("file1"), "foo\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["new", "-m", "my description"]);
    std::fs::write(repo_path.join("file1"), "foo\nbar\n").unwrap();
    std::fs::write(repo_path.join("file2"), "foo\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["rebase", "-r", "@", "-d", "root()"]);
    std::fs::write(repo_path.join("file1"), "resolved\n").unwrap();

    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog"]);
    insta::assert_snapshot!(stdout, @r###"
    @  rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    │  my description
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    │  my description
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 068224a7
    │  my description
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:08 2b023b5f
       (empty) my description
    "###);

    // Color
    let stdout = test_env.jj_cmd_success(&repo_path, &["--color=always", "obslog"]);
    insta::assert_snapshot!(stdout, @r###"
    @  [1m[38;5;13mr[38;5;8mlvkpnrz[39m [38;5;3mtest.user@example.com[39m [38;5;14m2001-02-03 08:05:10[39m [38;5;12m6[38;5;8m6b42ad3[39m[0m
    │  [1mmy description[0m
    ◉  [1m[39mr[0m[38;5;8mlvkpnrz[39m hidden [38;5;3mtest.user@example.com[39m [38;5;6m2001-02-03 08:05:09[39m [1m[38;5;4mc[0m[38;5;8mf73917d[39m [38;5;1mconflict[39m
    │  my description
    ◉  [1m[39mr[0m[38;5;8mlvkpnrz[39m hidden [38;5;3mtest.user@example.com[39m [38;5;6m2001-02-03 08:05:09[39m [1m[38;5;4m06[0m[38;5;8m8224a7[39m
    │  my description
    ◉  [1m[39mr[0m[38;5;8mlvkpnrz[39m hidden [38;5;3mtest.user@example.com[39m [38;5;6m2001-02-03 08:05:08[39m [1m[38;5;4m2b[0m[38;5;8m023b5f[39m
       [38;5;2m(empty)[39m my description
    "###);

    // There should be no diff caused by the rebase because it was a pure rebase
    // (even even though it resulted in a conflict).
    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "-p"]);
    insta::assert_snapshot!(stdout, @r###"
    @  rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    │  my description
    │  Resolved conflict in file1:
    │     1    1: <<<<<<< Conflict 1 of 1resolved
    │     2     : %%%%%%% Changes from base to side #1
    │     3     : -foo
    │     4     : +++++++ Contents of side #2
    │     5     : foo
    │     6     : bar
    │     7     : >>>>>>> Conflict 1 of 1 ends
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    │  my description
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 068224a7
    │  my description
    │  Modified regular file file1:
    │     1    1: foo
    │          2: bar
    │  Added regular file file2:
    │          1: foo
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:08 2b023b5f
       (empty) my description
    "###);

    // Test `--limit`
    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "--limit=2"]);
    insta::assert_snapshot!(stdout, @r###"
    @  rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    │  my description
    ◉  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    │  my description
    "###);

    // Test `--no-graph`
    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "--no-graph"]);
    insta::assert_snapshot!(stdout, @r###"
    rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    my description
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    my description
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 068224a7
    my description
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:08 2b023b5f
    (empty) my description
    "###);

    // Test `--git` format, and that it implies `-p`
    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "--no-graph", "--git"]);
    insta::assert_snapshot!(stdout, @r###"
    rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    my description
    diff --git a/file1 b/file1
    index 0000000000...2ab19ae607 100644
    --- a/file1
    +++ b/file1
    @@ -1,7 +1,1 @@
    -<<<<<<< Conflict 1 of 1
    -%%%%%%% Changes from base to side #1
    --foo
    -+++++++ Contents of side #2
    -foo
    -bar
    ->>>>>>> Conflict 1 of 1 ends
    +resolved
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    my description
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 068224a7
    my description
    diff --git a/file1 b/file1
    index 257cc5642c...3bd1f0e297 100644
    --- a/file1
    +++ b/file1
    @@ -1,1 +1,2 @@
     foo
    +bar
    diff --git a/file2 b/file2
    new file mode 100644
    index 0000000000..257cc5642c
    --- /dev/null
    +++ b/file2
    @@ -1,0 +1,1 @@
    +foo
    rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:08 2b023b5f
    (empty) my description
    "###);
}

#[test]
fn test_obslog_with_custom_symbols() {
    let test_env = TestEnvironment::default();
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    std::fs::write(repo_path.join("file1"), "foo\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["new", "-m", "my description"]);
    std::fs::write(repo_path.join("file1"), "foo\nbar\n").unwrap();
    std::fs::write(repo_path.join("file2"), "foo\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["rebase", "-r", "@", "-d", "root()"]);
    std::fs::write(repo_path.join("file1"), "resolved\n").unwrap();

    let toml = concat!("templates.log_node = 'if(current_working_copy, \"$\", \"┝\")'\n",);

    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "--config-toml", toml]);

    insta::assert_snapshot!(stdout, @r###"
    $  rlvkpnrz test.user@example.com 2001-02-03 08:05:10 66b42ad3
    │  my description
    ┝  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 cf73917d conflict
    │  my description
    ┝  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:09 068224a7
    │  my description
    ┝  rlvkpnrz hidden test.user@example.com 2001-02-03 08:05:08 2b023b5f
       (empty) my description
    "###);
}

#[test]
fn test_obslog_word_wrap() {
    let test_env = TestEnvironment::default();
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");
    let render = |args: &[&str], columns: u32, word_wrap: bool| {
        let mut args = args.to_vec();
        if word_wrap {
            args.push("--config-toml=ui.log-word-wrap=true");
        }
        let assert = test_env
            .jj_cmd(&repo_path, &args)
            .env("COLUMNS", columns.to_string())
            .assert()
            .success()
            .stderr("");
        get_stdout_string(&assert)
    };

    test_env.jj_cmd_ok(&repo_path, &["describe", "-m", "first"]);

    // ui.log-word-wrap option applies to both graph/no-graph outputs
    insta::assert_snapshot!(render(&["obslog"], 40, false), @r###"
    @  qpvuntsm test.user@example.com 2001-02-03 08:05:08 fa15625b
    │  (empty) first
    ◉  qpvuntsm hidden test.user@example.com 2001-02-03 08:05:07 230dd059
       (empty) (no description set)
    "###);
    insta::assert_snapshot!(render(&["obslog"], 40, true), @r###"
    @  qpvuntsm test.user@example.com
    │  2001-02-03 08:05:08 fa15625b
    │  (empty) first
    ◉  qpvuntsm hidden test.user@example.com
       2001-02-03 08:05:07 230dd059
       (empty) (no description set)
    "###);
    insta::assert_snapshot!(render(&["obslog", "--no-graph"], 40, false), @r###"
    qpvuntsm test.user@example.com 2001-02-03 08:05:08 fa15625b
    (empty) first
    qpvuntsm hidden test.user@example.com 2001-02-03 08:05:07 230dd059
    (empty) (no description set)
    "###);
    insta::assert_snapshot!(render(&["obslog", "--no-graph"], 40, true), @r###"
    qpvuntsm test.user@example.com
    2001-02-03 08:05:08 fa15625b
    (empty) first
    qpvuntsm hidden test.user@example.com
    2001-02-03 08:05:07 230dd059
    (empty) (no description set)
    "###);
}

#[test]
fn test_obslog_squash() {
    let mut test_env = TestEnvironment::default();
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    test_env.jj_cmd_ok(&repo_path, &["describe", "-m", "first"]);
    std::fs::write(repo_path.join("file1"), "foo\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["new", "-m", "second"]);
    std::fs::write(repo_path.join("file1"), "foo\nbar\n").unwrap();

    let edit_script = test_env.set_up_fake_editor();
    std::fs::write(edit_script, "write\nsquashed").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["squash"]);

    let stdout = test_env.jj_cmd_success(&repo_path, &["obslog", "-p", "-r", "@-"]);
    insta::assert_snapshot!(stdout, @r###"
    ◉    qpvuntsm test.user@example.com 2001-02-03 08:05:10 68647e34
    ├─╮  squashed
    │ │  Modified regular file file1:
    │ │     1    1: foo
    │ │          2: bar
    ◉ │  qpvuntsm hidden test.user@example.com 2001-02-03 08:05:09 766420db
    │ │  first
    │ │  Added regular file file1:
    │ │          1: foo
    ◉ │  qpvuntsm hidden test.user@example.com 2001-02-03 08:05:08 fa15625b
    │ │  (empty) first
    ◉ │  qpvuntsm hidden test.user@example.com 2001-02-03 08:05:07 230dd059
      │  (empty) (no description set)
      ◉  kkmpptxz hidden test.user@example.com 2001-02-03 08:05:10 46acd22a
      │  second
      │  Modified regular file file1:
      │     1    1: foo
      │          2: bar
      ◉  kkmpptxz hidden test.user@example.com 2001-02-03 08:05:09 cba41deb
         (empty) second
    "###);
}

#[test]
fn test_obslog_with_no_template() {
    let test_env = TestEnvironment::default();
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    let stderr = test_env.jj_cmd_cli_error(&repo_path, &["obslog", "-T"]);
    insta::assert_snapshot!(stderr, @r###"
    error: a value is required for '--template <TEMPLATE>' but none was supplied

    For more information, try '--help'.
    Hint: The following template aliases are defined:
    - builtin_log_comfortable
    - builtin_log_compact
    - builtin_log_detailed
    - builtin_log_node
    - builtin_log_node_ascii
    - builtin_log_oneline
    - builtin_op_log_comfortable
    - builtin_op_log_compact
    - builtin_op_log_node
    - builtin_op_log_node_ascii
    - commit_summary_separator
    - description_placeholder
    - email_placeholder
    - name_placeholder
    "###);
}
