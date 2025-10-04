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

#![allow(missing_docs)]

use std::io;
use std::io::Write;
use std::iter::zip;
use std::pin::Pin;

use bstr::BString;
use bstr::ByteSlice as _;
use futures::Stream;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use futures::try_join;
use itertools::Itertools as _;
use pollster::FutureExt as _;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt as _;

use crate::backend::BackendError;
use crate::backend::BackendResult;
use crate::backend::CommitId;
use crate::backend::CopyId;
use crate::backend::FileId;
use crate::backend::SymlinkId;
use crate::backend::TreeId;
use crate::backend::TreeValue;
use crate::copies::CopiesTreeDiffEntry;
use crate::copies::CopiesTreeDiffEntryPath;
use crate::diff::Diff;
use crate::diff::DiffHunk;
use crate::diff::DiffHunkKind;
use crate::files;
use crate::files::MergeResult;
use crate::merge::Merge;
use crate::merge::MergedTreeValue;
use crate::repo_path::RepoPath;
use crate::store::Store;

/// Minimum length of conflict markers.
pub const MIN_CONFLICT_MARKER_LEN: usize = 7;

/// If a file already contains lines which look like conflict markers of length
/// N, then the conflict markers we add will be of length (N + increment). This
/// number is chosen to make the conflict markers noticeably longer than the
/// existing markers.
const CONFLICT_MARKER_LEN_INCREMENT: usize = 4;

/// Comment for missing terminating newline in a term of a conflict.
const NO_EOL_COMMENT: &str = " (no terminating newline)";

/// Comment for missing terminating newline in the "add" side of a diff.
const ADD_NO_EOL_COMMENT: &str = " (removes terminating newline)";

/// Comment for missing terminating newline in the "remove" side of a diff.
const REMOVE_NO_EOL_COMMENT: &str = " (adds terminating newline)";

fn write_diff_hunks(hunks: &[DiffHunk], file: &mut dyn Write) -> io::Result<()> {
    for hunk in hunks {
        match hunk.kind {
            DiffHunkKind::Matching => {
                debug_assert!(hunk.contents.iter().all_equal());
                for line in hunk.contents[0].lines_with_terminator() {
                    file.write_all(b" ")?;
                    write_and_ensure_newline(file, line)?;
                }
            }
            DiffHunkKind::Different => {
                for line in hunk.contents[0].lines_with_terminator() {
                    file.write_all(b"-")?;
                    write_and_ensure_newline(file, line)?;
                }
                for line in hunk.contents[1].lines_with_terminator() {
                    file.write_all(b"+")?;
                    write_and_ensure_newline(file, line)?;
                }
            }
        }
    }
    Ok(())
}

async fn get_file_contents(
    store: &Store,
    path: &RepoPath,
    term: &Option<FileId>,
) -> BackendResult<BString> {
    match term {
        Some(id) => {
            let mut reader = store.read_file(path, id).await?;
            let mut content = vec![];
            reader
                .read_to_end(&mut content)
                .await
                .map_err(|err| BackendError::ReadFile {
                    path: path.to_owned(),
                    id: id.clone(),
                    source: err.into(),
                })?;
            Ok(BString::new(content))
        }
        // If the conflict had removed the file on one side, we pretend that the file
        // was empty there.
        None => Ok(BString::new(vec![])),
    }
}

pub async fn extract_as_single_hunk(
    merge: &Merge<Option<FileId>>,
    store: &Store,
    path: &RepoPath,
) -> BackendResult<Merge<BString>> {
    merge
        .as_ref()
        .try_map_async(|term| get_file_contents(store, path, term))
        .await
}

/// A type similar to `MergedTreeValue` but with associated data to include in
/// e.g. the working copy or in a diff.
pub enum MaterializedTreeValue {
    Absent,
    AccessDenied(Box<dyn std::error::Error + Send + Sync>),
    File(MaterializedFileValue),
    Symlink { id: SymlinkId, target: String },
    FileConflict(MaterializedFileConflictValue),
    OtherConflict { id: MergedTreeValue },
    GitSubmodule(CommitId),
    Tree(TreeId),
}

impl MaterializedTreeValue {
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    pub fn is_present(&self) -> bool {
        !self.is_absent()
    }
}

/// [`TreeValue::File`] with file content `reader`.
pub struct MaterializedFileValue {
    pub id: FileId,
    pub executable: bool,
    pub copy_id: CopyId,
    pub reader: Pin<Box<dyn AsyncRead + Send>>,
}

impl MaterializedFileValue {
    /// Reads file content until EOF. The provided `path` is used only for error
    /// reporting purpose.
    pub async fn read_all(&mut self, path: &RepoPath) -> BackendResult<Vec<u8>> {
        let mut buf = Vec::new();
        self.reader
            .read_to_end(&mut buf)
            .await
            .map_err(|err| BackendError::ReadFile {
                path: path.to_owned(),
                id: self.id.clone(),
                source: err.into(),
            })?;
        Ok(buf)
    }
}

/// Conflicted [`TreeValue::File`]s with file contents.
pub struct MaterializedFileConflictValue {
    /// File ids which preserve the shape of the tree conflict, to be used with
    /// [`Merge::update_from_simplified()`].
    pub unsimplified_ids: Merge<Option<FileId>>,
    /// Simplified file ids, in which redundant id pairs are dropped.
    pub ids: Merge<Option<FileId>>,
    /// File contents corresponding to the simplified `ids`.
    // TODO: or Vec<(FileId, Box<dyn Read>)> so that caller can stop reading
    // when null bytes found?
    pub contents: Merge<BString>,
    /// Merged executable bit. `None` if there are changes in both executable
    /// bit and file absence.
    pub executable: Option<bool>,
    /// Merged copy id. `None` if no single value could be determined.
    pub copy_id: Option<CopyId>,
}

/// Reads the data associated with a `MergedTreeValue` so it can be written to
/// e.g. the working copy or diff.
pub async fn materialize_tree_value(
    store: &Store,
    path: &RepoPath,
    value: MergedTreeValue,
) -> BackendResult<MaterializedTreeValue> {
    match materialize_tree_value_no_access_denied(store, path, value).await {
        Err(BackendError::ReadAccessDenied { source, .. }) => {
            Ok(MaterializedTreeValue::AccessDenied(source))
        }
        result => result,
    }
}

async fn materialize_tree_value_no_access_denied(
    store: &Store,
    path: &RepoPath,
    value: MergedTreeValue,
) -> BackendResult<MaterializedTreeValue> {
    match value.into_resolved() {
        Ok(None) => Ok(MaterializedTreeValue::Absent),
        Ok(Some(TreeValue::File {
            id,
            executable,
            copy_id,
        })) => {
            let reader = store.read_file(path, &id).await?;
            Ok(MaterializedTreeValue::File(MaterializedFileValue {
                id,
                executable,
                copy_id,
                reader,
            }))
        }
        Ok(Some(TreeValue::Symlink(id))) => {
            let target = store.read_symlink(path, &id).await?;
            Ok(MaterializedTreeValue::Symlink { id, target })
        }
        Ok(Some(TreeValue::GitSubmodule(id))) => Ok(MaterializedTreeValue::GitSubmodule(id)),
        Ok(Some(TreeValue::Tree(id))) => Ok(MaterializedTreeValue::Tree(id)),
        Ok(Some(TreeValue::Conflict(_))) => {
            panic!("cannot materialize legacy conflict object at path {path:?}");
        }
        Err(conflict) => match try_materialize_file_conflict_value(store, path, &conflict).await? {
            Some(file) => Ok(MaterializedTreeValue::FileConflict(file)),
            None => Ok(MaterializedTreeValue::OtherConflict { id: conflict }),
        },
    }
}

/// Suppose `conflict` contains only files or absent entries, reads the file
/// contents.
pub async fn try_materialize_file_conflict_value(
    store: &Store,
    path: &RepoPath,
    conflict: &MergedTreeValue,
) -> BackendResult<Option<MaterializedFileConflictValue>> {
    let (Some(unsimplified_ids), Some(executable_bits)) =
        (conflict.to_file_merge(), conflict.to_executable_merge())
    else {
        return Ok(None);
    };
    let ids = unsimplified_ids.simplify();
    let contents = extract_as_single_hunk(&ids, store, path).await?;
    let executable = resolve_file_executable(&executable_bits);
    Ok(Some(MaterializedFileConflictValue {
        unsimplified_ids,
        ids,
        contents,
        executable,
        copy_id: Some(CopyId::placeholder()),
    }))
}

/// Resolves conflicts in file executable bit, returns the original state if the
/// file is deleted and executable bit is unchanged.
pub fn resolve_file_executable(merge: &Merge<Option<bool>>) -> Option<bool> {
    let resolved = merge.resolve_trivial().copied()?;
    if resolved.is_some() {
        resolved
    } else {
        // If the merge is resolved to None (absent), there should be the same
        // number of Some(true) and Some(false). Pick the old state if
        // unambiguous, so the new file inherits the original executable bit.
        merge.removes().flatten().copied().all_equal_value().ok()
    }
}

/// Describes what style should be used when materializing conflicts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictMarkerStyle {
    /// Style which shows a snapshot and a series of diffs to apply.
    #[default]
    Diff,
    /// Style which shows a snapshot for each base and side.
    Snapshot,
    /// Style which replicates Git's "diff3" style to support external tools.
    Git,
}

/// Characters which can be repeated to form a conflict marker line when
/// materializing and parsing conflicts.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ConflictMarkerLineChar {
    ConflictStart = b'<',
    ConflictEnd = b'>',
    Add = b'+',
    Remove = b'-',
    Diff = b'%',
    GitAncestor = b'|',
    GitSeparator = b'=',
}

impl ConflictMarkerLineChar {
    /// Get the ASCII byte used for this conflict marker.
    fn to_byte(self) -> u8 {
        self as u8
    }

    /// Parse a byte to see if it corresponds with any kind of conflict marker.
    fn parse_byte(byte: u8) -> Option<Self> {
        match byte {
            b'<' => Some(Self::ConflictStart),
            b'>' => Some(Self::ConflictEnd),
            b'+' => Some(Self::Add),
            b'-' => Some(Self::Remove),
            b'%' => Some(Self::Diff),
            b'|' => Some(Self::GitAncestor),
            b'=' => Some(Self::GitSeparator),
            _ => None,
        }
    }
}

/// Represents a conflict marker line parsed from the file. Conflict marker
/// lines consist of a single ASCII character repeated for a certain length.
struct ConflictMarkerLine {
    kind: ConflictMarkerLineChar,
    len: usize,
}

/// Write a conflict marker to an output file.
fn write_conflict_marker(
    output: &mut dyn Write,
    kind: ConflictMarkerLineChar,
    len: usize,
    suffix_text: &str,
) -> io::Result<()> {
    let conflict_marker = BString::new(vec![kind.to_byte(); len]);

    if suffix_text.is_empty() {
        writeln!(output, "{conflict_marker}")
    } else {
        writeln!(output, "{conflict_marker} {suffix_text}")
    }
}

/// Parse a conflict marker from a line of a file. The conflict marker may have
/// any length (even less than MIN_CONFLICT_MARKER_LEN).
fn parse_conflict_marker_any_len(line: &[u8]) -> Option<ConflictMarkerLine> {
    let first_byte = *line.first()?;
    let kind = ConflictMarkerLineChar::parse_byte(first_byte)?;
    let len = line.iter().take_while(|&&b| b == first_byte).count();

    if let Some(next_byte) = line.get(len) {
        // If there is a character after the marker, it must be ASCII whitespace
        if !next_byte.is_ascii_whitespace() {
            return None;
        }
    }

    Some(ConflictMarkerLine { kind, len })
}

/// Parse a conflict marker, expecting it to be at least a certain length. Any
/// shorter conflict markers are ignored.
fn parse_conflict_marker(line: &[u8], expected_len: usize) -> Option<ConflictMarkerLineChar> {
    parse_conflict_marker_any_len(line)
        .filter(|marker| marker.len >= expected_len)
        .map(|marker| marker.kind)
}

/// Given a Merge of files, choose the conflict marker length to use when
/// materializing conflicts.
pub fn choose_materialized_conflict_marker_len<T: AsRef<[u8]>>(single_hunk: &Merge<T>) -> usize {
    let max_existing_marker_len = single_hunk
        .iter()
        .flat_map(|file| file.as_ref().lines_with_terminator())
        .filter_map(parse_conflict_marker_any_len)
        .map(|marker| marker.len)
        .max()
        .unwrap_or_default();

    max_existing_marker_len
        .saturating_add(CONFLICT_MARKER_LEN_INCREMENT)
        .max(MIN_CONFLICT_MARKER_LEN)
}

pub fn materialize_merge_result<T: AsRef<[u8]>>(
    single_hunk: &Merge<T>,
    conflict_marker_style: ConflictMarkerStyle,
    output: &mut dyn Write,
) -> io::Result<()> {
    let merge_result = files::merge_hunks(single_hunk);
    match &merge_result {
        MergeResult::Resolved(content) => output.write_all(content),
        MergeResult::Conflict(hunks) => {
            let conflict_marker_len = choose_materialized_conflict_marker_len(single_hunk);
            materialize_conflict_hunks(hunks, conflict_marker_style, conflict_marker_len, output)
        }
    }
}

pub fn materialize_merge_result_with_marker_len<T: AsRef<[u8]>>(
    single_hunk: &Merge<T>,
    conflict_marker_style: ConflictMarkerStyle,
    conflict_marker_len: usize,
    output: &mut dyn Write,
) -> io::Result<()> {
    let merge_result = files::merge_hunks(single_hunk);
    match &merge_result {
        MergeResult::Resolved(content) => output.write_all(content),
        MergeResult::Conflict(hunks) => {
            materialize_conflict_hunks(hunks, conflict_marker_style, conflict_marker_len, output)
        }
    }
}

pub fn materialize_merge_result_to_bytes<T: AsRef<[u8]>>(
    single_hunk: &Merge<T>,
    conflict_marker_style: ConflictMarkerStyle,
) -> BString {
    let merge_result = files::merge_hunks(single_hunk);
    match merge_result {
        MergeResult::Resolved(content) => content,
        MergeResult::Conflict(hunks) => {
            let conflict_marker_len = choose_materialized_conflict_marker_len(single_hunk);
            let mut output = Vec::new();
            materialize_conflict_hunks(
                &hunks,
                conflict_marker_style,
                conflict_marker_len,
                &mut output,
            )
            .expect("writing to an in-memory buffer should never fail");
            output.into()
        }
    }
}

pub fn materialize_merge_result_to_bytes_with_marker_len<T: AsRef<[u8]>>(
    single_hunk: &Merge<T>,
    conflict_marker_style: ConflictMarkerStyle,
    conflict_marker_len: usize,
) -> BString {
    let merge_result = files::merge_hunks(single_hunk);
    match merge_result {
        MergeResult::Resolved(content) => content,
        MergeResult::Conflict(hunks) => {
            let mut output = Vec::new();
            materialize_conflict_hunks(
                &hunks,
                conflict_marker_style,
                conflict_marker_len,
                &mut output,
            )
            .expect("writing to an in-memory buffer should never fail");
            output.into()
        }
    }
}

fn materialize_conflict_hunks(
    hunks: &[Merge<BString>],
    conflict_marker_style: ConflictMarkerStyle,
    conflict_marker_len: usize,
    output: &mut dyn Write,
) -> io::Result<()> {
    let num_conflicts = hunks
        .iter()
        .filter(|hunk| hunk.as_resolved().is_none())
        .count();
    let mut conflict_index = 0;
    for hunk in hunks {
        if let Some(content) = hunk.as_resolved() {
            output.write_all(content)?;
        } else {
            conflict_index += 1;
            let conflict_info = format!("Conflict {conflict_index} of {num_conflicts}");

            match (conflict_marker_style, hunk.as_slice()) {
                // 2-sided conflicts can use Git-style conflict markers
                (ConflictMarkerStyle::Git, [left, base, right]) => {
                    materialize_git_style_conflict(
                        left,
                        base,
                        right,
                        &conflict_info,
                        conflict_marker_len,
                        output,
                    )?;
                }
                _ => {
                    materialize_jj_style_conflict(
                        hunk,
                        &conflict_info,
                        conflict_marker_style,
                        conflict_marker_len,
                        output,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn materialize_git_style_conflict(
    left: &[u8],
    base: &[u8],
    right: &[u8],
    conflict_info: &str,
    conflict_marker_len: usize,
    output: &mut dyn Write,
) -> io::Result<()> {
    write_conflict_marker(
        output,
        ConflictMarkerLineChar::ConflictStart,
        conflict_marker_len,
        &format!("Side #1 ({conflict_info})"),
    )?;
    write_and_ensure_newline(output, left)?;

    write_conflict_marker(
        output,
        ConflictMarkerLineChar::GitAncestor,
        conflict_marker_len,
        "Base",
    )?;
    write_and_ensure_newline(output, base)?;

    // VS Code doesn't seem to support any trailing text on the separator line
    write_conflict_marker(
        output,
        ConflictMarkerLineChar::GitSeparator,
        conflict_marker_len,
        "",
    )?;

    write_and_ensure_newline(output, right)?;
    write_conflict_marker(
        output,
        ConflictMarkerLineChar::ConflictEnd,
        conflict_marker_len,
        &format!("Side #2 ({conflict_info} ends)"),
    )?;

    Ok(())
}

fn materialize_jj_style_conflict(
    hunk: &Merge<BString>,
    conflict_info: &str,
    conflict_marker_style: ConflictMarkerStyle,
    conflict_marker_len: usize,
    output: &mut dyn Write,
) -> io::Result<()> {
    // Write a positive snapshot (side) of a conflict
    let write_side = |add_index: usize, data: &[u8], output: &mut dyn Write| {
        write_conflict_marker(
            output,
            ConflictMarkerLineChar::Add,
            conflict_marker_len,
            &format!(
                "Contents of side #{}{}",
                add_index + 1,
                maybe_no_eol_comment(data)
            ),
        )?;
        write_and_ensure_newline(output, data)
    };

    // Write a negative snapshot (base) of a conflict
    let write_base = |base_str: &str, data: &[u8], output: &mut dyn Write| {
        write_conflict_marker(
            output,
            ConflictMarkerLineChar::Remove,
            conflict_marker_len,
            &format!("Contents of {base_str}{}", maybe_no_eol_comment(data)),
        )?;
        write_and_ensure_newline(output, data)
    };

    // Write a diff from a negative term to a positive term
    let write_diff =
        |base_str: &str, add_index: usize, diff: &[DiffHunk], output: &mut dyn Write| {
            let no_eol_remove = diff
                .last()
                .is_some_and(|diff_hunk| has_no_eol(diff_hunk.contents[0]));
            let no_eol_add = diff
                .last()
                .is_some_and(|diff_hunk| has_no_eol(diff_hunk.contents[1]));
            let no_eol_comment = match (no_eol_remove, no_eol_add) {
                (true, true) => NO_EOL_COMMENT,
                (true, _) => REMOVE_NO_EOL_COMMENT,
                (_, true) => ADD_NO_EOL_COMMENT,
                _ => "",
            };
            write_conflict_marker(
                output,
                ConflictMarkerLineChar::Diff,
                conflict_marker_len,
                &format!(
                    "Changes from {base_str} to side #{}{no_eol_comment}",
                    add_index + 1
                ),
            )?;
            write_diff_hunks(diff, output)
        };

    write_conflict_marker(
        output,
        ConflictMarkerLineChar::ConflictStart,
        conflict_marker_len,
        conflict_info,
    )?;
    let mut add_index = 0;
    for (base_index, left) in hunk.removes().enumerate() {
        // The vast majority of conflicts one actually tries to resolve manually have 1
        // base.
        let base_str = if hunk.removes().len() == 1 {
            "base".to_string()
        } else {
            format!("base #{}", base_index + 1)
        };

        let Some(right1) = hunk.get_add(add_index) else {
            // If we have no more positive terms, emit the remaining negative terms as
            // snapshots.
            write_base(&base_str, left, output)?;
            continue;
        };

        // For any style other than "diff", always emit sides and bases separately
        if conflict_marker_style != ConflictMarkerStyle::Diff {
            write_side(add_index, right1, output)?;
            write_base(&base_str, left, output)?;
            add_index += 1;
            continue;
        }

        let diff1 = Diff::by_line([&left, &right1]).hunks().collect_vec();
        // Check if the diff against the next positive term is better. Since we want to
        // preserve the order of the terms, we don't match against any later positive
        // terms.
        if let Some(right2) = hunk.get_add(add_index + 1) {
            let diff2 = Diff::by_line([&left, &right2]).hunks().collect_vec();
            if diff_size(&diff2) < diff_size(&diff1) {
                // If the next positive term is a better match, emit the current positive term
                // as a snapshot and the next positive term as a diff.
                write_side(add_index, right1, output)?;
                write_diff(&base_str, add_index + 1, &diff2, output)?;
                add_index += 2;
                continue;
            }
        }

        write_diff(&base_str, add_index, &diff1, output)?;
        add_index += 1;
    }

    // Emit the remaining positive terms as snapshots.
    for (add_index, slice) in hunk.adds().enumerate().skip(add_index) {
        write_side(add_index, slice, output)?;
    }
    write_conflict_marker(
        output,
        ConflictMarkerLineChar::ConflictEnd,
        conflict_marker_len,
        &format!("{conflict_info} ends"),
    )?;
    Ok(())
}

fn maybe_no_eol_comment(slice: &[u8]) -> &'static str {
    if has_no_eol(slice) {
        NO_EOL_COMMENT
    } else {
        ""
    }
}

// Write a chunk of data, ensuring that it doesn't end with a line which is
// missing its terminating newline.
fn write_and_ensure_newline(output: &mut dyn Write, data: &[u8]) -> io::Result<()> {
    output.write_all(data)?;
    if has_no_eol(data) {
        writeln!(output)?;
    }
    Ok(())
}

// Check whether a slice is missing its terminating newline character.
fn has_no_eol(slice: &[u8]) -> bool {
    slice.last().is_some_and(|&last| last != b'\n')
}

fn diff_size(hunks: &[DiffHunk]) -> usize {
    hunks
        .iter()
        .map(|hunk| match hunk.kind {
            DiffHunkKind::Matching => 0,
            DiffHunkKind::Different => hunk.contents.iter().map(|content| content.len()).sum(),
        })
        .sum()
}

pub struct MaterializedTreeDiffEntry {
    pub path: CopiesTreeDiffEntryPath,
    pub values: BackendResult<(MaterializedTreeValue, MaterializedTreeValue)>,
}

pub fn materialized_diff_stream<'a>(
    store: &'a Store,
    tree_diff: BoxStream<'a, CopiesTreeDiffEntry>,
) -> impl Stream<Item = MaterializedTreeDiffEntry> + use<'a> {
    tree_diff
        .map(async |CopiesTreeDiffEntry { path, values }| match values {
            Err(err) => MaterializedTreeDiffEntry {
                path,
                values: Err(err),
            },
            Ok((before, after)) => {
                let before_future = materialize_tree_value(store, path.source(), before);
                let after_future = materialize_tree_value(store, path.target(), after);
                let values = try_join!(before_future, after_future);
                MaterializedTreeDiffEntry { path, values }
            }
        })
        .buffered((store.concurrency() / 2).max(1))
}

/// Parses conflict markers from a slice.
///
/// Returns `None` if there were no valid conflict markers. The caller
/// has to provide the expected number of merge sides (adds). Conflict
/// markers that are otherwise valid will be considered invalid if
/// they don't have the expected arity.
///
/// All conflict markers in the file must be at least as long as the expected
/// length. Any shorter conflict markers will be ignored.
// TODO: "parse" is not usually the opposite of "materialize", so maybe we
// should rename them to "serialize" and "deserialize"?
pub fn parse_conflict(
    input: &[u8],
    num_sides: usize,
    expected_marker_len: usize,
) -> Option<Vec<Merge<BString>>> {
    if input.is_empty() {
        return None;
    }
    let mut hunks = vec![];
    let mut pos = 0;
    let mut resolved_start = 0;
    let mut conflict_start = None;
    let mut conflict_start_len = 0;
    for line in input.lines_with_terminator() {
        match parse_conflict_marker(line, expected_marker_len) {
            Some(ConflictMarkerLineChar::ConflictStart) => {
                conflict_start = Some(pos);
                conflict_start_len = line.len();
            }
            Some(ConflictMarkerLineChar::ConflictEnd) => {
                if let Some(conflict_start_index) = conflict_start.take() {
                    let conflict_body = &input[conflict_start_index + conflict_start_len..pos];
                    let hunk = parse_conflict_hunk(conflict_body, expected_marker_len);
                    if hunk.num_sides() == num_sides {
                        let resolved_slice = &input[resolved_start..conflict_start_index];
                        if !resolved_slice.is_empty() {
                            hunks.push(Merge::resolved(BString::from(resolved_slice)));
                        }
                        hunks.push(hunk);
                        resolved_start = pos + line.len();
                    }
                }
            }
            _ => {}
        }
        pos += line.len();
    }

    if hunks.is_empty() {
        None
    } else {
        if resolved_start < input.len() {
            hunks.push(Merge::resolved(BString::from(&input[resolved_start..])));
        }
        Some(hunks)
    }
}

/// This method handles parsing both JJ-style and Git-style conflict markers,
/// meaning that switching conflict marker styles won't prevent existing files
/// with other conflict marker styles from being parsed successfully. The
/// conflict marker style to use for parsing is determined based on the first
/// line of the hunk.
fn parse_conflict_hunk(input: &[u8], expected_marker_len: usize) -> Merge<BString> {
    // If the hunk starts with a conflict marker, find its first character
    let initial_conflict_marker = input
        .lines_with_terminator()
        .next()
        .and_then(|line| parse_conflict_marker(line, expected_marker_len));

    match initial_conflict_marker {
        // JJ-style conflicts must start with one of these 3 conflict marker lines
        Some(
            ConflictMarkerLineChar::Diff
            | ConflictMarkerLineChar::Remove
            | ConflictMarkerLineChar::Add,
        ) => parse_jj_style_conflict_hunk(input, expected_marker_len),
        // Git-style conflicts either must not start with a conflict marker line, or must start with
        // the "|||||||" conflict marker line (if the first side was empty)
        None | Some(ConflictMarkerLineChar::GitAncestor) => {
            parse_git_style_conflict_hunk(input, expected_marker_len)
        }
        // No other conflict markers are allowed at the start of a hunk
        Some(_) => Merge::resolved(BString::new(vec![])),
    }
}

fn parse_jj_style_conflict_hunk(input: &[u8], expected_marker_len: usize) -> Merge<BString> {
    enum State {
        Diff,
        Remove,
        Add,
        Unknown,
    }
    let mut state = State::Unknown;
    let mut removes = vec![];
    let mut adds = vec![];
    for line in input.lines_with_terminator() {
        match parse_conflict_marker(line, expected_marker_len) {
            Some(ConflictMarkerLineChar::Diff) => {
                state = State::Diff;
                removes.push(BString::new(vec![]));
                adds.push(BString::new(vec![]));
                continue;
            }
            Some(ConflictMarkerLineChar::Remove) => {
                state = State::Remove;
                removes.push(BString::new(vec![]));
                continue;
            }
            Some(ConflictMarkerLineChar::Add) => {
                state = State::Add;
                adds.push(BString::new(vec![]));
                continue;
            }
            _ => {}
        }
        match state {
            State::Diff => {
                if let Some(rest) = line.strip_prefix(b"-") {
                    removes.last_mut().unwrap().extend_from_slice(rest);
                } else if let Some(rest) = line.strip_prefix(b"+") {
                    adds.last_mut().unwrap().extend_from_slice(rest);
                } else if let Some(rest) = line.strip_prefix(b" ") {
                    removes.last_mut().unwrap().extend_from_slice(rest);
                    adds.last_mut().unwrap().extend_from_slice(rest);
                } else if line == b"\n" || line == b"\r\n" {
                    // Some editors strip trailing whitespace, so " \n" might become "\n". It would
                    // be unfortunate if this prevented the conflict from being parsed, so we add
                    // the empty line to the "remove" and "add" as if there was a space in front
                    removes.last_mut().unwrap().extend_from_slice(line);
                    adds.last_mut().unwrap().extend_from_slice(line);
                } else {
                    // Doesn't look like a valid conflict
                    return Merge::resolved(BString::new(vec![]));
                }
            }
            State::Remove => {
                removes.last_mut().unwrap().extend_from_slice(line);
            }
            State::Add => {
                adds.last_mut().unwrap().extend_from_slice(line);
            }
            State::Unknown => {
                // Doesn't look like a valid conflict
                return Merge::resolved(BString::new(vec![]));
            }
        }
    }

    if adds.len() == removes.len() + 1 {
        Merge::from_removes_adds(removes, adds)
    } else {
        // Doesn't look like a valid conflict
        Merge::resolved(BString::new(vec![]))
    }
}

fn parse_git_style_conflict_hunk(input: &[u8], expected_marker_len: usize) -> Merge<BString> {
    #[derive(PartialEq, Eq)]
    enum State {
        Left,
        Base,
        Right,
    }
    let mut state = State::Left;
    let mut left = BString::new(vec![]);
    let mut base = BString::new(vec![]);
    let mut right = BString::new(vec![]);
    for line in input.lines_with_terminator() {
        match parse_conflict_marker(line, expected_marker_len) {
            Some(ConflictMarkerLineChar::GitAncestor) => {
                if state == State::Left {
                    state = State::Base;
                    continue;
                } else {
                    // Base must come after left
                    return Merge::resolved(BString::new(vec![]));
                }
            }
            Some(ConflictMarkerLineChar::GitSeparator) => {
                if state == State::Base {
                    state = State::Right;
                    continue;
                } else {
                    // Right must come after base
                    return Merge::resolved(BString::new(vec![]));
                }
            }
            _ => {}
        }
        match state {
            State::Left => left.extend_from_slice(line),
            State::Base => base.extend_from_slice(line),
            State::Right => right.extend_from_slice(line),
        }
    }

    if state == State::Right {
        Merge::from_vec(vec![left, base, right])
    } else {
        // Doesn't look like a valid conflict
        Merge::resolved(BString::new(vec![]))
    }
}

/// Parses conflict markers in `content` and returns an updated version of
/// `file_ids` with the new contents. If no (valid) conflict markers remain, a
/// single resolves `FileId` will be returned.
pub async fn update_from_content(
    file_ids: &Merge<Option<FileId>>,
    store: &Store,
    path: &RepoPath,
    content: &[u8],
    conflict_marker_style: ConflictMarkerStyle,
    conflict_marker_len: usize,
) -> BackendResult<Merge<Option<FileId>>> {
    let simplified_file_ids = file_ids.simplify();

    // First check if the new content is unchanged compared to the old content. If
    // it is, we don't need parse the content or write any new objects to the
    // store. This is also a way of making sure that unchanged tree/file
    // conflicts (for example) are not converted to regular files in the working
    // copy.
    let mut old_content = Vec::with_capacity(content.len());
    let merge_hunk = extract_as_single_hunk(&simplified_file_ids, store, path).await?;
    materialize_merge_result_with_marker_len(
        &merge_hunk,
        conflict_marker_style,
        conflict_marker_len,
        &mut old_content,
    )
    .unwrap();
    if content == old_content {
        return Ok(file_ids.clone());
    }

    // Parse conflicts from the new content using the arity of the simplified
    // conflicts.
    let Some(mut hunks) = parse_conflict(
        content,
        simplified_file_ids.num_sides(),
        conflict_marker_len,
    ) else {
        // Either there are no markers or they don't have the expected arity
        let file_id = store.write_file(path, &mut &content[..]).await?;
        return Ok(Merge::normal(file_id));
    };

    // If there is a conflict at the end of the file and a term ends with a newline,
    // check whether the original term ended with a newline. If it didn't, then
    // remove the newline since it was added automatically when materializing.
    if let Some(last_hunk) = hunks.last_mut().filter(|hunk| !hunk.is_resolved()) {
        for (original_content, term) in merge_hunk.iter().zip_eq(last_hunk.iter_mut()) {
            if term.last() == Some(&b'\n') && has_no_eol(original_content) {
                term.pop();
            }
        }
    }

    let mut contents = simplified_file_ids.as_ref().map(|_| vec![]);
    for hunk in hunks {
        if let Some(slice) = hunk.as_resolved() {
            for content in contents.iter_mut() {
                content.extend_from_slice(slice);
            }
        } else {
            for (content, slice) in zip(contents.iter_mut(), hunk.into_iter()) {
                content.extend(Vec::from(slice));
            }
        }
    }

    // If the user edited the empty placeholder for an absent side, we consider the
    // conflict resolved.
    if zip(contents.iter(), simplified_file_ids.iter())
        .any(|(content, file_id)| file_id.is_none() && !content.is_empty())
    {
        let file_id = store.write_file(path, &mut &content[..]).await?;
        return Ok(Merge::normal(file_id));
    }

    // Now write the new files contents we found by parsing the file with conflict
    // markers.
    // TODO: Write these concurrently
    let new_file_ids: Vec<Option<FileId>> = zip(contents.iter(), simplified_file_ids.iter())
        .map(|(content, file_id)| -> BackendResult<Option<FileId>> {
            match file_id {
                Some(_) => {
                    let file_id = store.write_file(path, &mut content.as_slice()).block_on()?;
                    Ok(Some(file_id))
                }
                None => {
                    // The missing side of a conflict is still represented by
                    // the empty string we materialized it as
                    Ok(None)
                }
            }
        })
        .try_collect()?;

    // If the conflict was simplified, expand the conflict to the original
    // number of sides.
    let new_file_ids = if new_file_ids.len() != file_ids.iter().len() {
        file_ids
            .clone()
            .update_from_simplified(Merge::from_vec(new_file_ids))
    } else {
        Merge::from_vec(new_file_ids)
    };
    Ok(new_file_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_file_executable() {
        fn resolve<const N: usize>(values: [Option<bool>; N]) -> Option<bool> {
            resolve_file_executable(&Merge::from_vec(values.to_vec()))
        }

        // already resolved
        assert_eq!(resolve([None]), None);
        assert_eq!(resolve([Some(false)]), Some(false));
        assert_eq!(resolve([Some(true)]), Some(true));

        // trivially resolved
        assert_eq!(resolve([Some(true), Some(true), Some(true)]), Some(true));
        assert_eq!(resolve([Some(true), Some(false), Some(false)]), Some(true));
        assert_eq!(resolve([Some(false), Some(true), Some(false)]), Some(false));
        assert_eq!(resolve([None, None, Some(true)]), Some(true));

        // unresolvable
        assert_eq!(resolve([Some(false), Some(true), None]), None);

        // trivially resolved to absent, so pick the original state
        assert_eq!(resolve([Some(true), Some(true), None]), Some(true));
        assert_eq!(resolve([None, Some(false), Some(false)]), Some(false));
        assert_eq!(
            resolve([None, None, Some(true), Some(true), None]),
            Some(true)
        );

        // trivially resolved to absent, and the original state is ambiguous
        assert_eq!(
            resolve([Some(true), Some(true), None, Some(false), Some(false)]),
            None
        );
        assert_eq!(
            resolve([
                None,
                Some(true),
                Some(true),
                Some(false),
                Some(false),
                Some(false),
                Some(false),
            ]),
            None
        );
    }
}
