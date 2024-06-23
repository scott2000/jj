// Copyright 2023 The Jujutsu Authors
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

use insta::assert_snapshot;

use crate::common::TestEnvironment;

#[test]
fn test_util_config_schema() {
    let test_env = TestEnvironment::default();
    let stdout = test_env.jj_cmd_success(test_env.env_root(), &["util", "config-schema"]);
    // Validate partial snapshot, redacting any lines nested 2+ indent levels.
    insta::with_settings!({filters => vec![(r"(?m)(^        .*$\r?\n)+", "        [...]\n")]}, {
        assert_snapshot!(stdout, @r###"
        {
            "$schema": "http://json-schema.org/draft-07/schema",
            "title": "Jujutsu config",
            "type": "object",
            "description": "User configuration for Jujutsu VCS. See https://github.com/martinvonz/jj/blob/main/docs/config.md for details",
            "properties": {
                [...]
        	"fix": {
                [...]
            }
        }
        "###)
    });
}

#[test]
fn test_gc_args() {
    let test_env = TestEnvironment::default();
    // Use the local backend because GitBackend::gc() depends on the git CLI.
    test_env.jj_cmd_ok(
        test_env.env_root(),
        &["init", "repo", "--config-toml=ui.allow-init-native=true"],
    );
    let repo_path = test_env.env_root().join("repo");

    let (_stdout, stderr) = test_env.jj_cmd_ok(&repo_path, &["util", "gc"]);
    insta::assert_snapshot!(stderr, @"");

    let stderr = test_env.jj_cmd_failure(&repo_path, &["util", "gc", "--at-op=@-"]);
    insta::assert_snapshot!(stderr, @r###"
    Error: Cannot garbage collect from a non-head operation
    "###);

    let stderr = test_env.jj_cmd_failure(&repo_path, &["util", "gc", "--expire=foobar"]);
    insta::assert_snapshot!(stderr, @r###"
    Error: --expire only accepts 'now'
    "###);
}

#[test]
fn test_gc_operation_log() {
    let test_env = TestEnvironment::default();
    // Use the local backend because GitBackend::gc() depends on the git CLI.
    test_env.jj_cmd_ok(
        test_env.env_root(),
        &["init", "repo", "--config-toml=ui.allow-init-native=true"],
    );
    let repo_path = test_env.env_root().join("repo");

    // Create an operation.
    std::fs::write(repo_path.join("file"), "a change\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["commit", "-m", "a change"]);
    let op_to_remove = test_env.current_operation_id(&repo_path);

    // Make another operation the head.
    std::fs::write(repo_path.join("file"), "another change\n").unwrap();
    test_env.jj_cmd_ok(&repo_path, &["commit", "-m", "another change"]);

    // This works before the operation is removed.
    test_env.jj_cmd_ok(&repo_path, &["debug", "operation", &op_to_remove]);

    // Remove some operations.
    test_env.jj_cmd_ok(&repo_path, &["operation", "abandon", "..@-"]);
    test_env.jj_cmd_ok(&repo_path, &["util", "gc", "--expire=now"]);

    // Now this doesn't work.
    let stderr = test_env.jj_cmd_failure(&repo_path, &["debug", "operation", &op_to_remove]);
    insta::assert_snapshot!(stderr, @r###"
    Error: No operation ID matching "1708ccd0d25f313f1559dfd1c4d16f0424de23a58e946830b0f27eb1252ce9295fe018e03fa4356e6aa39520cd8b5d44b7688024428988fe4d015291a4706172"
    "###);
}

#[test]
fn test_shell_completions() {
    #[track_caller]
    fn test(shell: &str) {
        let test_env = TestEnvironment::default();
        // Use the local backend because GitBackend::gc() depends on the git CLI.
        let (out, err) = test_env.jj_cmd_ok(test_env.env_root(), &["util", "completion", shell]);
        // Ensures only stdout contains text
        assert!(!out.is_empty());
        assert!(err.is_empty());
    }

    test("bash");
    test("fish");
    test("nushell");
    test("zsh");
}
