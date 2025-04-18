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

use crate::common::TestEnvironment;

#[test]
fn test_deprecated_flags() {
    let test_env = TestEnvironment::default();
    let output = test_env
        .run_jj_in(".", ["util", "completion", "--bash"])
        .success();
    insta::assert_snapshot!(output.stderr, @r"
    Warning: `jj util completion --bash` will be removed in a future version, and this will be a hard error
    Hint: Use `jj util completion bash` instead
    [EOF]
    ");
    assert!(output.stdout.raw().contains("COMPREPLY"), "{output}");
}
