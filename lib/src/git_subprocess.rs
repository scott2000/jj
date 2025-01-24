// Copyright 2025 The Jujutsu Authors
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
//
use std::num::NonZeroU32;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;

use bstr::ByteSlice;
use thiserror::Error;

use crate::git::RefSpec;
use crate::git::RefToPush;

/// Error originating by a Git subprocess
#[derive(Error, Debug)]
pub enum GitSubprocessError {
    #[error("Could not find repository at '{0}'")]
    NoSuchRepository(String),
    #[error("Could not execute the git process, found in the OS path '{path}'")]
    SpawnInPath {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("Could not execute git process at specified path '{path}'")]
    Spawn {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("Failed to wait for the git process")]
    Wait(std::io::Error),
    #[error("Git process failed: {0}")]
    External(String),
}

/// Context for creating Git subprocesses
pub(crate) struct GitSubprocessContext<'a> {
    git_dir: PathBuf,
    git_executable_path: &'a Path,
}

impl<'a> GitSubprocessContext<'a> {
    pub(crate) fn new(git_dir: impl Into<PathBuf>, git_executable_path: &'a Path) -> Self {
        GitSubprocessContext {
            git_dir: git_dir.into(),
            git_executable_path,
        }
    }

    pub(crate) fn from_git2(git_repo: &git2::Repository, git_executable_path: &'a Path) -> Self {
        Self::new(git_repo.path(), git_executable_path)
    }

    /// Create the Git command
    fn create_command(&self) -> Command {
        let mut git_cmd = Command::new(self.git_executable_path);
        // TODO: here we are passing the full path to the git_dir, which can lead to UNC
        // bugs in Windows. The ideal way to do this is to pass the workspace
        // root to Command::current_dir and then pass a relative path to the git
        // dir
        git_cmd
            .arg("--bare")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .stdin(Stdio::null())
            .stderr(Stdio::piped());

        git_cmd
    }

    /// Spawn the git command
    fn spawn_cmd(&self, mut git_cmd: Command) -> Result<Output, GitSubprocessError> {
        tracing::debug!(cmd = ?git_cmd, "spawning a git subprocess");
        let child_git = git_cmd.spawn().map_err(|error| {
            if self.git_executable_path.is_absolute() {
                GitSubprocessError::Spawn {
                    path: self.git_executable_path.to_path_buf(),
                    error,
                }
            } else {
                GitSubprocessError::SpawnInPath {
                    path: self.git_executable_path.to_path_buf(),
                    error,
                }
            }
        })?;

        child_git
            .wait_with_output()
            .map_err(GitSubprocessError::Wait)
    }

    /// Perform a git fetch
    ///
    /// This returns a fully qualified ref that wasn't fetched successfully
    /// Note that git only returns one failed ref at a time
    pub(crate) fn spawn_fetch(
        &self,
        remote_name: &str,
        depth: Option<NonZeroU32>,
        refspecs: &[RefSpec],
    ) -> Result<Option<String>, GitSubprocessError> {
        if refspecs.is_empty() {
            return Ok(None);
        }
        let mut command = self.create_command();
        command.stdout(Stdio::piped());
        command
            .arg("fetch")
            // attempt to prune stale refs
            .arg("--prune");
        if let Some(d) = depth {
            command.arg(format!("--depth={d}"));
        }
        command.arg("--").arg(remote_name);
        command.args(refspecs.iter().map(|x| x.to_git_format()));

        let output = self.spawn_cmd(command)?;

        parse_git_fetch_output(output)
    }

    /// Prune particular branches
    pub(crate) fn spawn_branch_prune(
        &self,
        branches_to_prune: &[String],
    ) -> Result<(), GitSubprocessError> {
        if branches_to_prune.is_empty() {
            return Ok(());
        }
        let mut command = self.create_command();
        command.stdout(Stdio::null());
        command.args(["branch", "--remotes", "--delete", "--"]);
        command.args(branches_to_prune);

        let output = self.spawn_cmd(command)?;

        // we name the type to make sure that it is not meant to be used
        let () = parse_git_branch_prune_output(output)?;

        Ok(())
    }

    /// How we retrieve the remote's default branch:
    ///
    /// `git remote show <remote_name>`
    ///
    /// dumps a lot of information about the remote, with a line such as:
    /// `  HEAD branch: <default_branch>`
    pub(crate) fn spawn_remote_show(
        &self,
        remote_name: &str,
    ) -> Result<Option<String>, GitSubprocessError> {
        let mut command = self.create_command();
        command.stdout(Stdio::piped());
        command.args(["remote", "show", "--", remote_name]);
        let output = self.spawn_cmd(command)?;

        let output = parse_git_remote_show_output(output)?;

        // find the HEAD branch line in the output
        parse_git_remote_show_default_branch(&output.stdout)
    }

    /// Push references to git
    ///
    /// All pushes are forced, using --force-with-lease to perform a test&set
    /// operation on the remote repository
    ///
    /// Return tuple with
    ///     1. refs that failed to push
    ///     2. refs that succeeded to push
    pub(crate) fn spawn_push(
        &self,
        remote_name: &str,
        references: &[RefToPush],
    ) -> Result<(Vec<String>, Vec<String>), GitSubprocessError> {
        let mut command = self.create_command();
        command.stdout(Stdio::piped());
        command.args(["push", "--porcelain"]);
        command.args(
            references
                .iter()
                .map(|reference| format!("--force-with-lease={}", reference.to_git_lease())),
        );
        command.args(["--", remote_name]);
        // with --force-with-lease we cannot have the forced refspec,
        // as it ignores the lease
        command.args(
            references
                .iter()
                .map(|r| r.refspec.to_git_format_not_forced()),
        );

        let output = self.spawn_cmd(command)?;

        parse_git_push_output(output)
    }
}

/// Generate a GitSubprocessError::ExternalGitError if the stderr output was not
/// recognizable
fn external_git_error(stderr: &[u8]) -> GitSubprocessError {
    GitSubprocessError::External(format!(
        "External git program failed:\n{}",
        stderr.to_str_lossy()
    ))
}

/// Parse no such remote errors output from git
///
/// Returns the remote that wasn't found
///
/// To say this, git prints out a lot of things, but the first line is of the
/// form:
/// `fatal: '<remote>' does not appear to be a git repository`
/// or
/// `fatal: '<remote>': Could not resolve host: invalid-remote
fn parse_no_such_remote(stderr: &[u8]) -> Option<String> {
    let first_line = stderr.lines().next()?;
    let suffix = first_line
        .strip_prefix(b"fatal: '")
        .or_else(|| first_line.strip_prefix(b"fatal: unable to access '"))?;

    suffix
        .strip_suffix(b"' does not appear to be a git repository")
        .or_else(|| suffix.strip_suffix(b"': Could not resolve host: invalid-remote"))
        .map(|remote| remote.to_str_lossy().into_owned())
}

/// Parse error from refspec not present on the remote
///
/// This returns
///     Some(local_ref) that wasn't found by the remote
///     None if this wasn't the error
///
/// On git fetch even though --prune is specified, if a particular
/// refspec is asked for but not present in the remote, git will error out.
///
/// Git only reports one of these errors at a time, so we only look at the first
/// line
///
/// The first line is of the form:
/// `fatal: couldn't find remote ref refs/heads/<ref>`
fn parse_no_remote_ref(stderr: &[u8]) -> Option<String> {
    let first_line = stderr.lines().next()?;
    first_line
        .strip_prefix(b"fatal: couldn't find remote ref ")
        .map(|refname| refname.to_str_lossy().into_owned())
}

/// Parse remote tracking branch not found
///
/// This returns true if the error was detected
///
/// if a branch is asked for but is not present, jj will detect it post-hoc
/// so, we want to ignore these particular errors with git
///
/// The first line is of the form:
/// `error: remote-tracking branch '<branch>' not found`
fn parse_no_remote_tracking_branch(stderr: &[u8]) -> Option<String> {
    let first_line = stderr.lines().next()?;

    let suffix = first_line.strip_prefix(b"error: remote-tracking branch '")?;

    suffix
        .strip_suffix(b"' not found.")
        .or_else(|| suffix.strip_suffix(b"' not found"))
        .map(|branch| branch.to_str_lossy().into_owned())
}

// return the fully qualified ref that failed to fetch
//
// note that git fetch only returns one error at a time
fn parse_git_fetch_output(output: Output) -> Result<Option<String>, GitSubprocessError> {
    if output.status.success() {
        return Ok(None);
    }

    // There are some git errors we want to parse out
    if let Some(remote) = parse_no_such_remote(&output.stderr) {
        return Err(GitSubprocessError::NoSuchRepository(remote));
    }

    if let Some(refspec) = parse_no_remote_ref(&output.stderr) {
        return Ok(Some(refspec));
    }

    if parse_no_remote_tracking_branch(&output.stderr).is_some() {
        return Ok(None);
    }

    Err(external_git_error(&output.stderr))
}

fn parse_git_branch_prune_output(output: Output) -> Result<(), GitSubprocessError> {
    if output.status.success() {
        return Ok(());
    }

    // There are some git errors we want to parse out
    if parse_no_remote_tracking_branch(&output.stderr).is_some() {
        return Ok(());
    }

    Err(external_git_error(&output.stderr))
}

fn parse_git_remote_show_output(output: Output) -> Result<Output, GitSubprocessError> {
    if output.status.success() {
        return Ok(output);
    }

    // There are some git errors we want to parse out
    if let Some(remote) = parse_no_such_remote(&output.stderr) {
        return Err(GitSubprocessError::NoSuchRepository(remote));
    }

    Err(external_git_error(&output.stderr))
}

fn parse_git_remote_show_default_branch(
    stdout: &[u8],
) -> Result<Option<String>, GitSubprocessError> {
    stdout
        .lines()
        .map(|x| x.trim())
        .find(|x| x.starts_with_str("HEAD branch:"))
        .and_then(|x| x.split_str(" ").last().map(|y| y.trim()))
        .filter(|branch_name| branch_name != b"(unknown)")
        .map(|branch_name| branch_name.to_str())
        .transpose()
        .map_err(|e| GitSubprocessError::External(format!("git remote output is not utf-8: {e:?}")))
        .map(|b| b.map(|x| x.to_string()))
}

// git-push porcelain has the following format (per line)
// `<flag>\t<from>:<to>\t<summary>\t(<reason>)`
//
// <flag> is one of:
//     ' ' for a successfully pushed fast-forward;
//      + for a successful forced update
//      - for a successfully deleted ref
//      * for a successfully pushed new ref
//      !  for a ref that was rejected or failed to push; and
//      =  for a ref that was up to date and did not need pushing.
//
// <from>:<to> is the refspec
//
// <summary> is extra info (commit ranges or reason for rejected)
// at times the summary is omitted
//
// <reason> is a human-readable explanation
fn parse_ref_pushes(stdout: &[u8]) -> Result<(Vec<String>, Vec<String>), GitSubprocessError> {
    if !stdout.starts_with(b"To ") {
        return Err(GitSubprocessError::External(format!(
            "Git push output unfamiliar:\n{}",
            stdout.to_str_lossy()
        )));
    }

    let mut pushed_refs = Vec::new();
    let mut rejected_refs = Vec::new();
    for (idx, line) in stdout
        .lines()
        .skip(1)
        .take_while(|line| line != b"Done")
        .enumerate()
    {
        // format:
        // <flag>\t<ref>\t<summary>\t<comment>
        // sometimes the summary is omitted
        let create_error = || {
            GitSubprocessError::External(format!(
                "Line #{idx} of git-push has unknown format: {}",
                line.to_str_lossy()
            ))
        };
        let mut it = line.split_str("\t").fuse();
        let flag = it.next().ok_or_else(create_error)?;
        let reference = it.next().ok_or_else(create_error)?;
        // we capture the remaining elements to ensure the line is well formed
        let _summary_or_comment = it.next().ok_or_else(create_error)?;
        let _comment_opt = it.next();
        if it.next().is_some() {
            return Err(create_error());
        }

        let full_refspec = reference
            .to_str()
            .map_err(|e| {
                format!(
                    "Line #{} of git-push has non-utf8 refspec {}: {}",
                    idx,
                    reference.to_str_lossy(),
                    e
                )
            })
            .map_err(GitSubprocessError::External)?;

        let reference = full_refspec
            .split_once(":")
            .map(|(_refname, reference)| reference.to_string())
            .ok_or_else(|| {
                GitSubprocessError::External(format!(
                    "Line #{idx} of git-push has full refspec without named ref: {full_refspec}"
                ))
            })?;

        match flag {
            // ' ' for a successfully pushed fast-forward;
            //  + for a successful forced update
            //  - for a successfully deleted ref
            //  * for a successfully pushed new ref
            //  =  for a ref that was up to date and did not need pushing.
            b"+" | b"-" | b"*" | b"=" | b" " => {
                pushed_refs.push(reference);
            }
            // ! for a ref that was rejected or failed to push; and
            b"!" => {
                rejected_refs.push(reference);
            }
            unknown => {
                return Err(GitSubprocessError::External(format!(
                    "Line #{} of git-push starts with an unknown flag '{}': '{}'",
                    idx,
                    unknown.to_str_lossy(),
                    line.to_str_lossy()
                )));
            }
        }
    }

    Ok((rejected_refs, pushed_refs))
}

// on Ok, return a tuple with
//  1. list of failed references from test and set
//  2. list of successful references pushed
fn parse_git_push_output(output: Output) -> Result<(Vec<String>, Vec<String>), GitSubprocessError> {
    if output.status.success() {
        // TODO: figure out how to report `remote: ` logging nicely
        // In sum, git apparently supports the remote repo sending a message to the
        // local, which we should probably forward nicely
        //
        // If we support this, we can be a bit more strict, and say that messages other
        // than these constitute an error
        let ref_pushes = parse_ref_pushes(&output.stdout)?;
        return Ok(ref_pushes);
    }

    if let Some(remote) = parse_no_such_remote(&output.stderr) {
        return Err(GitSubprocessError::NoSuchRepository(remote));
    }

    if output
        .stderr
        .starts_with_str("error: failed to push some refs to ")
    {
        parse_ref_pushes(&output.stdout)
    } else {
        Err(external_git_error(&output.stderr))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const SAMPLE_NO_SUCH_REPOSITORY_ERROR: &[u8] =
        br###"fatal: unable to access 'origin': Could not resolve host: invalid-remote
fatal: Could not read from remote repository.

Please make sure you have the correct access rights
and the repository exists. "###;
    const SAMPLE_NO_SUCH_REMOTE_ERROR: &[u8] =
        br###"fatal: 'origin' does not appear to be a git repository
fatal: Could not read from remote repository.

Please make sure you have the correct access rights
and the repository exists. "###;
    const SAMPLE_NO_REMOTE_REF_ERROR: &[u8] = b"fatal: couldn't find remote ref refs/heads/noexist";
    const SAMPLE_NO_REMOTE_TRACKING_BRANCH_ERROR: &[u8] =
        b"error: remote-tracking branch 'bookmark' not found";
    const SAMPLE_PUSH_REFS_PORCELAIN_OUTPUT: &[u8] = b"To origin
*\tdeadbeef:refs/heads/bookmark1\tdeadbeef\t[new branch]
+\tdeadbeef:refs/heads/bookmark2\tdeadbeef\t[new branch]
-\tdeadbeef:refs/heads/bookmark3\tdeadbeef\t[new branch]
 \tdeadbeef:refs/heads/bookmark4\tdeadbeef\t[new branch]
=\tdeadbeef:refs/heads/bookmark5\tdeadbeef\t[new branch]
!\tdeadbeef:refs/heads/bookmark6\tdeadbeef\t[new branch]
Done";
    const SAMPLE_OK_STDERR: &[u8] = b"";

    #[test]
    fn test_parse_no_such_remote() {
        assert_eq!(
            parse_no_such_remote(SAMPLE_NO_SUCH_REPOSITORY_ERROR),
            Some("origin".to_string())
        );
        assert_eq!(
            parse_no_such_remote(SAMPLE_NO_SUCH_REMOTE_ERROR),
            Some("origin".to_string())
        );
        assert_eq!(parse_no_such_remote(SAMPLE_NO_REMOTE_REF_ERROR), None);
        assert_eq!(
            parse_no_such_remote(SAMPLE_NO_REMOTE_TRACKING_BRANCH_ERROR),
            None
        );
        assert_eq!(
            parse_no_such_remote(SAMPLE_PUSH_REFS_PORCELAIN_OUTPUT),
            None
        );
        assert_eq!(parse_no_such_remote(SAMPLE_OK_STDERR), None);
    }

    #[test]
    fn test_parse_no_remote_ref() {
        assert_eq!(parse_no_remote_ref(SAMPLE_NO_SUCH_REPOSITORY_ERROR), None);
        assert_eq!(parse_no_remote_ref(SAMPLE_NO_SUCH_REMOTE_ERROR), None);
        assert_eq!(
            parse_no_remote_ref(SAMPLE_NO_REMOTE_REF_ERROR),
            Some("refs/heads/noexist".to_string())
        );
        assert_eq!(
            parse_no_remote_ref(SAMPLE_NO_REMOTE_TRACKING_BRANCH_ERROR),
            None
        );
        assert_eq!(parse_no_remote_ref(SAMPLE_PUSH_REFS_PORCELAIN_OUTPUT), None);
        assert_eq!(parse_no_remote_ref(SAMPLE_OK_STDERR), None);
    }

    #[test]
    fn test_parse_no_remote_tracking_branch() {
        assert_eq!(
            parse_no_remote_tracking_branch(SAMPLE_NO_SUCH_REPOSITORY_ERROR),
            None
        );
        assert_eq!(
            parse_no_remote_tracking_branch(SAMPLE_NO_SUCH_REMOTE_ERROR),
            None
        );
        assert_eq!(
            parse_no_remote_tracking_branch(SAMPLE_NO_REMOTE_REF_ERROR),
            None
        );
        assert_eq!(
            parse_no_remote_tracking_branch(SAMPLE_NO_REMOTE_TRACKING_BRANCH_ERROR),
            Some("bookmark".to_string())
        );
        assert_eq!(
            parse_no_remote_tracking_branch(SAMPLE_PUSH_REFS_PORCELAIN_OUTPUT),
            None
        );
        assert_eq!(parse_no_remote_tracking_branch(SAMPLE_OK_STDERR), None);
    }

    #[test]
    fn test_parse_ref_pushes() {
        assert!(parse_ref_pushes(SAMPLE_NO_SUCH_REPOSITORY_ERROR).is_err());
        assert!(parse_ref_pushes(SAMPLE_NO_SUCH_REMOTE_ERROR).is_err());
        assert!(parse_ref_pushes(SAMPLE_NO_REMOTE_REF_ERROR).is_err());
        assert!(parse_ref_pushes(SAMPLE_NO_REMOTE_TRACKING_BRANCH_ERROR).is_err());
        let (failed, success) = parse_ref_pushes(SAMPLE_PUSH_REFS_PORCELAIN_OUTPUT).unwrap();
        assert_eq!(failed, vec!["refs/heads/bookmark6".to_string()]);
        assert_eq!(
            success,
            vec![
                "refs/heads/bookmark1".to_string(),
                "refs/heads/bookmark2".to_string(),
                "refs/heads/bookmark3".to_string(),
                "refs/heads/bookmark4".to_string(),
                "refs/heads/bookmark5".to_string(),
            ]
        );
        assert!(parse_ref_pushes(SAMPLE_OK_STDERR).is_err());
    }
}