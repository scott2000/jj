// Copyright 2024 The Jujutsu Authors
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

use std::path::Path;

use crate::common::TestEnvironment;

#[test]
fn test_reset_author_default_config() {
    let test_env = TestEnvironment::default();
    test_env
        .add_config(r#"revsets.reset-author-on-edit = 'mine() & empty() & description(exact:"")'"#);
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    test_env.jj_cmd_ok(&repo_path, &["new", "root()"]);
    std::fs::write(repo_path.join("file1"), "a").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["describe", "-m", "1"]);

    test_env.jj_cmd_ok(&repo_path, &["new", "root()", "-m", "2"]);
    std::fs::write(repo_path.join("file2"), "b").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["st"]); // Snapshot before user gets changed

    test_env.jj_cmd_ok(
        &repo_path,
        &[
            "new",
            "--config-toml",
            "user.email='committer.email@example.com'",
            "root()",
            "-m",
            "3",
        ],
    );
    std::fs::write(repo_path.join("file3"), "c").unwrap();

    insta::assert_snapshot!(get_log_output(&test_env, &repo_path), @r###"
    @  cc20a19b417d committer.email@example.com 2001-02-03 04:05:12.000 +07:00 test.user@example.com 2001-02-03 04:05:13.000 +07:00 3
    │ ◉  83cbfda1cbc7 test.user@example.com 2001-02-03 04:05:10.000 +07:00 test.user@example.com 2001-02-03 04:05:11.000 +07:00 2
    ├─╯
    │ ◉  b1623baa27e4 test.user@example.com 2001-02-03 04:05:09.000 +07:00 test.user@example.com 2001-02-03 04:05:09.000 +07:00 1
    ├─╯
    ◉  000000000000 1970-01-01 00:00:00.000 +00:00 1970-01-01 00:00:00.000 +00:00
    "###);

    test_env.jj_cmd_ok(&repo_path, &["edit", "description(1)"]);
    std::fs::write(repo_path.join("file4"), "d").unwrap();

    insta::assert_snapshot!(get_log_output(&test_env, &repo_path), @r###"
    @  de11fa19c3a7 test.user@example.com 2001-02-03 04:05:09.000 +07:00 test.user@example.com 2001-02-03 04:05:15.000 +07:00 1
    │ ◉  cc20a19b417d committer.email@example.com 2001-02-03 04:05:12.000 +07:00 test.user@example.com 2001-02-03 04:05:13.000 +07:00 3
    ├─╯
    │ ◉  83cbfda1cbc7 test.user@example.com 2001-02-03 04:05:10.000 +07:00 test.user@example.com 2001-02-03 04:05:11.000 +07:00 2
    ├─╯
    ◉  000000000000 1970-01-01 00:00:00.000 +00:00 1970-01-01 00:00:00.000 +00:00
    "###);
}

#[test]
fn test_reset_author_timestamp() {
    let test_env = TestEnvironment::default();
    test_env.add_config(r#"revsets.reset-author-on-edit = "description(1)""#);
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    test_env.jj_cmd_ok(&repo_path, &["new", "root()", "-m", "1"]);
    std::fs::write(repo_path.join("file1"), "a").unwrap();

    test_env.jj_cmd_ok(&repo_path, &["new", "root()", "-m", "2"]);
    std::fs::write(repo_path.join("file2"), "b").unwrap();

    insta::assert_snapshot!(get_log_output(&test_env, &repo_path), @r###"
    @  0c939f685bff test.user@example.com 2001-02-03 04:05:09.000 +07:00 test.user@example.com 2001-02-03 04:05:10.000 +07:00 2
    │ ◉  b1623baa27e4 test.user@example.com 2001-02-03 04:05:09.000 +07:00 test.user@example.com 2001-02-03 04:05:09.000 +07:00 1
    ├─╯
    ◉  000000000000 1970-01-01 00:00:00.000 +00:00 1970-01-01 00:00:00.000 +00:00
    "###);
}

#[test]
fn test_reset_author_information() {
    let test_env = TestEnvironment::default();
    test_env.add_config(r#"revsets.reset-author-on-edit = "description(1)""#);
    test_env.jj_cmd_ok(test_env.env_root(), &["git", "init", "repo"]);
    let repo_path = test_env.env_root().join("repo");

    test_env.jj_cmd_ok(
        &repo_path,
        &[
            "new",
            "--config-toml",
            "user.email='committer.email@example.com'",
            "root()",
            "-m",
            "1",
        ],
    );
    std::fs::write(repo_path.join("file1"), "a").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["st"]); // Snapshot using a different user

    test_env.jj_cmd_ok(
        &repo_path,
        &[
            "new",
            "--config-toml",
            "user.email='committer.email@example.com'",
            "root()",
            "-m",
            "2",
        ],
    );
    std::fs::write(repo_path.join("file2"), "b").unwrap();

    insta::assert_snapshot!(get_log_output(&test_env, &repo_path), @r###"
    @  53407752d5a2 committer.email@example.com 2001-02-03 04:05:10.000 +07:00 test.user@example.com 2001-02-03 04:05:11.000 +07:00 2
    │ ◉  b1623baa27e4 test.user@example.com 2001-02-03 04:05:09.000 +07:00 test.user@example.com 2001-02-03 04:05:09.000 +07:00 1
    ├─╯
    ◉  000000000000 1970-01-01 00:00:00.000 +00:00 1970-01-01 00:00:00.000 +00:00
    "###);
}

fn get_log_output(test_env: &TestEnvironment, cwd: &Path) -> String {
    let template = r#"
    separate(
        " ",
        commit_id.short(),
        author.email(),
        author.timestamp(),
        committer.email(),
        committer.timestamp(),
        description,
    )
    "#;
    test_env.jj_cmd_success(cwd, &["log", "-T", template])
}
