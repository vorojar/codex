use super::*;
use async_trait::async_trait;
use codex_exec_server::CopyOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::FileMetadata;
use codex_exec_server::FileSystemResult;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::ReadDirectoryEntry;
use codex_exec_server::RemoveOptions;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::ReadDenyMatcher;
use core_test_support::PathExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::ffi::OsString;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use tempfile::tempdir;
use tokio::sync::Mutex as AsyncMutex;

use crate::session::tests::make_session_and_context;
use crate::session::turn_context::TurnEnvironment;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::turn_diff_tracker::TurnDiffTracker;

async fn list_dir_slice(
    path: &Path,
    offset: usize,
    limit: usize,
    depth: usize,
) -> Result<Vec<String>, FunctionCallError> {
    let path = AbsolutePathBuf::from_absolute_path(path)
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
    list_dir_slice_with_policy(
        LOCAL_FS.as_ref(),
        /*sandbox*/ None,
        &path,
        offset,
        limit,
        depth,
        /*read_deny_matcher*/ None,
    )
    .await
}

async fn invoke_list_dir_with_turn(
    arguments: serde_json::Value,
    session: crate::session::session::Session,
    turn: crate::session::turn_context::TurnContext,
) -> Result<String, FunctionCallError> {
    let invocation = ToolInvocation {
        session: session.into(),
        turn: turn.into(),
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tracker: Arc::new(AsyncMutex::new(TurnDiffTracker::new())),
        call_id: "call-list-dir".to_string(),
        tool_name: codex_tools::ToolName::plain("list_dir"),
        source: ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    };

    ListDirHandler
        .handle(invocation)
        .await
        .map(FunctionToolOutput::into_text)
}

fn add_secondary_environment(
    turn: &mut crate::session::turn_context::TurnContext,
    environment_id: &str,
    cwd: AbsolutePathBuf,
) {
    let primary_environment = turn.primary_environment().expect("primary env");
    turn.environments.push(TurnEnvironment {
        environment_id: environment_id.to_string(),
        environment: primary_environment.environment.clone(),
        cwd,
    });
}

#[derive(Default)]
struct RecordingFileSystem {
    read_directories: Mutex<Vec<AbsolutePathBuf>>,
    inspected_paths: Mutex<Vec<AbsolutePathBuf>>,
    directory_entries: Mutex<Vec<(AbsolutePathBuf, Vec<ReadDirectoryEntry>)>>,
}

impl RecordingFileSystem {
    fn add_directory(&self, path: &AbsolutePathBuf, entries: &[TestEntry]) {
        self.directory_entries
            .lock()
            .expect("lock directory entries")
            .push((
                path.clone(),
                entries
                    .iter()
                    .map(|entry| ReadDirectoryEntry {
                        file_name: OsString::from(entry.name),
                        metadata: FileMetadata::from(entry.kind),
                    })
                    .collect(),
            ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TestEntryKind {
    Directory,
    File,
    Symlink,
}

impl From<TestEntryKind> for FileMetadata {
    fn from(kind: TestEntryKind) -> Self {
        Self {
            is_directory: kind == TestEntryKind::Directory,
            is_file: kind == TestEntryKind::File || kind == TestEntryKind::Symlink,
            is_symlink: kind == TestEntryKind::Symlink,
            created_at_ms: 0,
            modified_at_ms: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct TestEntry {
    name: &'static str,
    kind: TestEntryKind,
}

impl TestEntry {
    fn directory(name: &'static str) -> Self {
        Self {
            name,
            kind: TestEntryKind::Directory,
        }
    }

    fn file(name: &'static str) -> Self {
        Self {
            name,
            kind: TestEntryKind::File,
        }
    }

    fn symlink(name: &'static str) -> Self {
        Self {
            name,
            kind: TestEntryKind::Symlink,
        }
    }
}

#[async_trait]
impl ExecutorFileSystem for RecordingFileSystem {
    async fn read_file(
        &self,
        _path: &AbsolutePathBuf,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<u8>> {
        Err(io::Error::other("read_file is not implemented"))
    }

    async fn write_file(
        &self,
        _path: &AbsolutePathBuf,
        _contents: Vec<u8>,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        Err(io::Error::other("write_file is not implemented"))
    }

    async fn create_directory(
        &self,
        _path: &AbsolutePathBuf,
        _create_directory_options: CreateDirectoryOptions,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        Err(io::Error::other("create_directory is not implemented"))
    }

    async fn get_metadata(
        &self,
        path: &AbsolutePathBuf,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<FileMetadata> {
        self.inspected_paths
            .lock()
            .expect("lock inspected paths")
            .push(path.clone());
        Err(io::Error::other("get_metadata is not implemented"))
    }

    async fn read_directory(
        &self,
        path: &AbsolutePathBuf,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<Vec<ReadDirectoryEntry>> {
        self.read_directories
            .lock()
            .expect("lock read directories")
            .push(path.clone());
        self.directory_entries
            .lock()
            .expect("lock directory entries")
            .iter()
            .find_map(|(directory_path, entries)| (directory_path == path).then(|| entries.clone()))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "directory not found"))
    }

    async fn remove(
        &self,
        _path: &AbsolutePathBuf,
        _remove_options: RemoveOptions,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        Err(io::Error::other("remove is not implemented"))
    }

    async fn copy(
        &self,
        _source_path: &AbsolutePathBuf,
        _destination_path: &AbsolutePathBuf,
        _copy_options: CopyOptions,
        _sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    ) -> FileSystemResult<()> {
        Err(io::Error::other("copy is not implemented"))
    }
}

#[tokio::test]
async fn lists_entries_from_provided_environment_filesystem() {
    let fs = RecordingFileSystem::default();
    let temp = tempdir().expect("create tempdir");
    let path = AbsolutePathBuf::from_absolute_path(temp.path()).expect("absolute path");
    fs.add_directory(&path, &[TestEntry::file("from-environment-fs.txt")]);

    let entries = list_dir_slice_with_policy(
        &fs, /*sandbox*/ None, &path, /*offset*/ 1, /*limit*/ 20, /*depth*/ 1,
        /*read_deny_matcher*/ None,
    )
    .await
    .expect("list directory through provided filesystem");

    assert_eq!(entries, vec!["from-environment-fs.txt".to_string()]);
    assert_eq!(
        *fs.read_directories.lock().expect("lock read directories"),
        vec![path.clone()]
    );
    assert_eq!(
        *fs.inspected_paths.lock().expect("lock inspected paths"),
        Vec::<AbsolutePathBuf>::new()
    );
}

#[tokio::test]
async fn provided_environment_filesystem_preserves_symlink_suffixes() {
    let fs = RecordingFileSystem::default();
    let temp = tempdir().expect("create tempdir");
    let path = AbsolutePathBuf::from_absolute_path(temp.path()).expect("absolute path");
    fs.add_directory(
        &path,
        &[
            TestEntry::file("entry.txt"),
            TestEntry::symlink("link"),
            TestEntry::directory("nested"),
        ],
    );
    fs.add_directory(&path.join("nested"), &[TestEntry::file("child.txt")]);

    let entries = list_dir_slice_with_policy(
        &fs, /*sandbox*/ None, &path, /*offset*/ 1, /*limit*/ 20, /*depth*/ 2,
        /*read_deny_matcher*/ None,
    )
    .await
    .expect("list directory through provided filesystem");

    assert_eq!(
        entries,
        vec![
            "entry.txt".to_string(),
            "link@".to_string(),
            "nested/".to_string(),
            "  child.txt".to_string(),
        ]
    );
}

#[tokio::test]
async fn provided_environment_filesystem_paginates_after_pruning_denied_entries() {
    let fs = RecordingFileSystem::default();
    let temp = tempdir().expect("create tempdir");
    let path = AbsolutePathBuf::from_absolute_path(temp.path()).expect("absolute path");
    let visible_dir = path.join("visible");
    let denied_dir = path.join("private");
    fs.add_directory(
        &path,
        &[
            TestEntry::directory("private"),
            TestEntry::file("secret.txt"),
            TestEntry::directory("visible"),
            TestEntry::file("z.txt"),
        ],
    );
    fs.add_directory(&visible_dir, &[TestEntry::file("ok.txt")]);
    fs.add_directory(&denied_dir, &[TestEntry::file("hidden.txt")]);

    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: denied_dir.clone(),
            },
            access: FileSystemAccessMode::None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: path.join("secret.txt"),
            },
            access: FileSystemAccessMode::None,
        },
    ]);
    let read_deny_matcher = ReadDenyMatcher::new(&policy, &path);

    let entries = list_dir_slice_with_policy(
        &fs,
        /*sandbox*/ None,
        &path,
        /*offset*/ 1,
        /*limit*/ 2,
        /*depth*/ 2,
        read_deny_matcher.as_ref(),
    )
    .await
    .expect("list directory through provided filesystem");

    assert_eq!(
        entries,
        vec![
            "visible/".to_string(),
            "  ok.txt".to_string(),
            "More than 2 entries found".to_string(),
        ]
    );
    assert_eq!(
        *fs.read_directories.lock().expect("lock read directories"),
        vec![path.clone(), visible_dir.clone()]
    );
    assert_eq!(
        *fs.inspected_paths.lock().expect("lock inspected paths"),
        Vec::<AbsolutePathBuf>::new()
    );
}

#[tokio::test]
async fn handler_resolves_relative_paths_under_primary_environment_cwd() {
    let temp = tempdir().expect("create tempdir");
    let nested = temp.path().join("nested");
    tokio::fs::create_dir(&nested)
        .await
        .expect("create nested dir");
    tokio::fs::write(nested.join("entry.txt"), b"content")
        .await
        .expect("write entry");
    let (session, mut turn) = make_session_and_context().await;
    turn.cwd = temp.path().abs();
    turn.environments[0].cwd = temp.path().abs();

    let output = invoke_list_dir_with_turn(
        json!({
            "dir_path": "nested",
            "depth": 1,
        }),
        session,
        turn,
    )
    .await
    .expect("list relative path");

    assert_eq!(
        output,
        format!(
            "Absolute path: {}\nentry.txt",
            temp.path().join("nested").display()
        )
    );
}

#[tokio::test]
async fn handler_routes_to_explicit_environment_and_uses_env_qualified_display() {
    let primary_temp = tempdir().expect("create primary tempdir");
    let secondary_temp = tempdir().expect("create secondary tempdir");
    let secondary_nested = secondary_temp.path().join("nested");
    tokio::fs::create_dir(&secondary_nested)
        .await
        .expect("create secondary nested dir");
    tokio::fs::write(secondary_nested.join("secondary.txt"), b"content")
        .await
        .expect("write secondary entry");
    let (session, mut turn) = make_session_and_context().await;
    turn.cwd = primary_temp.path().abs();
    turn.environments[0].cwd = primary_temp.path().abs();
    add_secondary_environment(&mut turn, "secondary", secondary_temp.path().abs());

    let output = invoke_list_dir_with_turn(
        json!({
            "dir_path": "nested",
            "environment_id": "secondary",
            "depth": 1,
        }),
        session,
        turn,
    )
    .await
    .expect("list secondary environment");

    assert_eq!(
        output,
        format!(
            "Environment path: oai_env://secondary{}\nsecondary.txt",
            secondary_temp.path().join("nested").display()
        )
    );
}

#[tokio::test]
async fn handler_rejects_mismatched_explicit_and_path_environment() {
    let primary_temp = tempdir().expect("create primary tempdir");
    let secondary_temp = tempdir().expect("create secondary tempdir");
    let (session, mut turn) = make_session_and_context().await;
    turn.cwd = primary_temp.path().abs();
    turn.environments[0].cwd = primary_temp.path().abs();
    add_secondary_environment(&mut turn, "secondary", secondary_temp.path().abs());

    let err = invoke_list_dir_with_turn(
        json!({
            "dir_path": format!(
                "oai_env://secondary{}",
                secondary_temp.path().join("nested").display(),
            ),
            "environment_id": codex_exec_server::LOCAL_ENVIRONMENT_ID,
        }),
        session,
        turn,
    )
    .await
    .expect_err("mismatched environment");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "environment_id `local` does not match path environment `secondary`".to_string(),
        )
    );
}

#[tokio::test]
async fn lists_directory_entries() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();

    let sub_dir = dir_path.join("nested");
    tokio::fs::create_dir(&sub_dir)
        .await
        .expect("create sub dir");

    let deeper_dir = sub_dir.join("deeper");
    tokio::fs::create_dir(&deeper_dir)
        .await
        .expect("create deeper dir");

    tokio::fs::write(dir_path.join("entry.txt"), b"content")
        .await
        .expect("write file");
    tokio::fs::write(sub_dir.join("child.txt"), b"child")
        .await
        .expect("write child");
    tokio::fs::write(deeper_dir.join("grandchild.txt"), b"grandchild")
        .await
        .expect("write grandchild");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link_path = dir_path.join("link");
        symlink(dir_path.join("entry.txt"), &link_path).expect("create symlink");
    }

    let entries = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 20, /*depth*/ 3,
    )
    .await
    .expect("list directory");

    #[cfg(unix)]
    let expected = vec![
        "entry.txt".to_string(),
        "link@".to_string(),
        "nested/".to_string(),
        "  child.txt".to_string(),
        "  deeper/".to_string(),
        "    grandchild.txt".to_string(),
    ];

    #[cfg(not(unix))]
    let expected = vec![
        "entry.txt".to_string(),
        "nested/".to_string(),
        "  child.txt".to_string(),
        "  deeper/".to_string(),
        "    grandchild.txt".to_string(),
    ];

    assert_eq!(entries, expected);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recurses_into_non_utf8_local_directory_names() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    let directory_name_bytes = b"bad-\xFF-dir".to_vec();
    let directory_name = OsString::from_vec(directory_name_bytes.clone());
    let non_utf8_dir = dir_path.join(&directory_name);
    tokio::fs::create_dir(&non_utf8_dir)
        .await
        .expect("create non-utf8 dir");
    tokio::fs::write(non_utf8_dir.join("child.txt"), b"child")
        .await
        .expect("write child");

    let entries = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 20, /*depth*/ 2,
    )
    .await
    .expect("list directory");

    let display_name = String::from_utf8_lossy(&directory_name_bytes);
    assert_eq!(
        entries,
        vec![format!("{display_name}/"), "  child.txt".to_string()]
    );
}

#[tokio::test]
async fn errors_when_offset_exceeds_entries() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    tokio::fs::create_dir(dir_path.join("nested"))
        .await
        .expect("create sub dir");

    let err = list_dir_slice(
        dir_path, /*offset*/ 10, /*limit*/ 1, /*depth*/ 2,
    )
    .await
    .expect_err("offset exceeds entries");
    assert_eq!(
        err,
        FunctionCallError::RespondToModel("offset exceeds directory entry count".to_string())
    );
}

#[tokio::test]
async fn respects_depth_parameter() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    let nested = dir_path.join("nested");
    let deeper = nested.join("deeper");
    tokio::fs::create_dir(&nested).await.expect("create nested");
    tokio::fs::create_dir(&deeper).await.expect("create deeper");
    tokio::fs::write(dir_path.join("root.txt"), b"root")
        .await
        .expect("write root");
    tokio::fs::write(nested.join("child.txt"), b"child")
        .await
        .expect("write nested");
    tokio::fs::write(deeper.join("grandchild.txt"), b"deep")
        .await
        .expect("write deeper");

    let entries_depth_one = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 10, /*depth*/ 1,
    )
    .await
    .expect("list depth 1");
    assert_eq!(
        entries_depth_one,
        vec!["nested/".to_string(), "root.txt".to_string(),]
    );

    let entries_depth_two = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 20, /*depth*/ 2,
    )
    .await
    .expect("list depth 2");
    assert_eq!(
        entries_depth_two,
        vec![
            "nested/".to_string(),
            "  child.txt".to_string(),
            "  deeper/".to_string(),
            "root.txt".to_string(),
        ]
    );

    let entries_depth_three = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 30, /*depth*/ 3,
    )
    .await
    .expect("list depth 3");
    assert_eq!(
        entries_depth_three,
        vec![
            "nested/".to_string(),
            "  child.txt".to_string(),
            "  deeper/".to_string(),
            "    grandchild.txt".to_string(),
            "root.txt".to_string(),
        ]
    );
}

#[tokio::test]
async fn paginates_in_sorted_order() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();

    let dir_a = dir_path.join("a");
    let dir_b = dir_path.join("b");
    tokio::fs::create_dir(&dir_a).await.expect("create a");
    tokio::fs::create_dir(&dir_b).await.expect("create b");

    tokio::fs::write(dir_a.join("a_child.txt"), b"a")
        .await
        .expect("write a child");
    tokio::fs::write(dir_b.join("b_child.txt"), b"b")
        .await
        .expect("write b child");

    let first_page = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 2, /*depth*/ 2,
    )
    .await
    .expect("list page one");
    assert_eq!(
        first_page,
        vec![
            "a/".to_string(),
            "  a_child.txt".to_string(),
            "More than 2 entries found".to_string()
        ]
    );

    let second_page = list_dir_slice(
        dir_path, /*offset*/ 3, /*limit*/ 2, /*depth*/ 2,
    )
    .await
    .expect("list page two");
    assert_eq!(
        second_page,
        vec!["b/".to_string(), "  b_child.txt".to_string()]
    );
}

#[tokio::test]
async fn handles_large_limit_without_overflow() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    tokio::fs::write(dir_path.join("alpha.txt"), b"alpha")
        .await
        .expect("write alpha");
    tokio::fs::write(dir_path.join("beta.txt"), b"beta")
        .await
        .expect("write beta");
    tokio::fs::write(dir_path.join("gamma.txt"), b"gamma")
        .await
        .expect("write gamma");

    let entries = list_dir_slice(dir_path, /*offset*/ 2, usize::MAX, /*depth*/ 1)
        .await
        .expect("list without overflow");
    assert_eq!(
        entries,
        vec!["beta.txt".to_string(), "gamma.txt".to_string(),]
    );
}

#[tokio::test]
async fn indicates_truncated_results() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();

    for idx in 0..40 {
        let file = dir_path.join(format!("file_{idx:02}.txt"));
        tokio::fs::write(file, b"content")
            .await
            .expect("write file");
    }

    let entries = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 25, /*depth*/ 1,
    )
    .await
    .expect("list directory");
    assert_eq!(entries.len(), 26);
    assert_eq!(
        entries.last(),
        Some(&"More than 25 entries found".to_string())
    );
}

#[tokio::test]
async fn truncation_respects_sorted_order() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let dir_path = temp.path();
    let nested = dir_path.join("nested");
    let deeper = nested.join("deeper");
    tokio::fs::create_dir(&nested).await?;
    tokio::fs::create_dir(&deeper).await?;
    tokio::fs::write(dir_path.join("root.txt"), b"root").await?;
    tokio::fs::write(nested.join("child.txt"), b"child").await?;
    tokio::fs::write(deeper.join("grandchild.txt"), b"deep").await?;

    let entries_depth_three = list_dir_slice(
        dir_path, /*offset*/ 1, /*limit*/ 3, /*depth*/ 3,
    )
    .await?;
    assert_eq!(
        entries_depth_three,
        vec![
            "nested/".to_string(),
            "  child.txt".to_string(),
            "  deeper/".to_string(),
            "More than 3 entries found".to_string()
        ]
    );

    Ok(())
}

#[tokio::test]
async fn hides_denied_entries_and_prunes_denied_subtrees() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    let visible_dir = dir_path.join("visible");
    let denied_dir = dir_path.join("private");
    tokio::fs::create_dir(&visible_dir)
        .await
        .expect("create visible dir");
    tokio::fs::create_dir(&denied_dir)
        .await
        .expect("create denied dir");
    tokio::fs::write(visible_dir.join("ok.txt"), b"ok")
        .await
        .expect("write visible file");
    tokio::fs::write(denied_dir.join("secret.txt"), b"secret")
        .await
        .expect("write denied file");
    tokio::fs::write(dir_path.join("top_secret.txt"), b"secret")
        .await
        .expect("write denied top-level file");

    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: denied_dir.try_into().expect("absolute denied dir"),
            },
            access: FileSystemAccessMode::None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: dir_path
                    .join("top_secret.txt")
                    .try_into()
                    .expect("absolute denied file"),
            },
            access: FileSystemAccessMode::None,
        },
    ]);

    let read_deny_matcher = ReadDenyMatcher::new(&policy, dir_path);
    let entries = list_dir_slice_with_policy(
        LOCAL_FS.as_ref(),
        /*sandbox*/ None,
        &AbsolutePathBuf::from_absolute_path(dir_path).expect("absolute dir"),
        /*offset*/ 1,
        /*limit*/ 20,
        /*depth*/ 3,
        read_deny_matcher.as_ref(),
    )
    .await
    .expect("list directory");

    assert_eq!(
        entries,
        vec!["visible/".to_string(), "  ok.txt".to_string(),]
    );
}
