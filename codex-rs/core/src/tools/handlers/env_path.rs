use std::path::Path;
use std::path::PathBuf;

use codex_utils_absolute_path::AbsolutePathBuf;

use crate::function_tool::FunctionCallError;
use crate::session::turn_context::TurnContext;
use crate::session::turn_context::TurnEnvironment;

const OAI_ENV_SCHEME: &str = "oai_env://";
const OAI_ENV_SCHEME_PREFIX: &str = "oai_env:";

#[derive(Debug)]
pub(super) struct ResolvedEnvironmentPath<'a> {
    pub(super) environment: &'a TurnEnvironment,
    pub(super) path: AbsolutePathBuf,
}

pub(super) fn resolve_tool_environment<'a>(
    turn: &'a TurnContext,
    environment_id: Option<&str>,
    tool_name: &str,
) -> Result<&'a TurnEnvironment, FunctionCallError> {
    match environment_id {
        Some("") => Err(FunctionCallError::RespondToModel(
            "environment_id must be non-empty".to_string(),
        )),
        Some(environment_id) => turn
            .selected_environment(Some(environment_id))
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "unknown turn environment id `{environment_id}`"
                ))
            }),
        None => turn.primary_environment().ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("{tool_name} is unavailable in this session"))
        }),
    }
}

pub(super) fn resolve_environment_path<'a>(
    turn: &'a TurnContext,
    explicit_environment_id: Option<&str>,
    raw_path: &str,
    field_name: &str,
    tool_name: &str,
) -> Result<ResolvedEnvironmentPath<'a>, FunctionCallError> {
    let qualified_path = parse_oai_env_uri(raw_path, field_name)?;
    let environment_id = selected_environment_id(
        explicit_environment_id,
        qualified_path.as_ref().map(|path| path.environment_id),
    )?;
    let environment = resolve_tool_environment(turn, environment_id, tool_name)?;
    let path = match qualified_path {
        Some(path) => path.path,
        None => environment.cwd.join(Path::new(raw_path)),
    };

    Ok(ResolvedEnvironmentPath { environment, path })
}

pub(super) fn format_oai_env_uri(environment_id: &str, path: &AbsolutePathBuf) -> String {
    let path = path.display().to_string();
    if path.starts_with('/') {
        format!("oai_env://{environment_id}{path}")
    } else {
        format!("oai_env://{environment_id}/{path}")
    }
}

#[derive(Debug)]
pub(super) struct OaiEnvPath<'a> {
    pub(super) environment_id: &'a str,
    pub(super) path: AbsolutePathBuf,
}

pub(super) fn parse_oai_env_uri<'a>(
    raw_path: &'a str,
    field_name: &str,
) -> Result<Option<OaiEnvPath<'a>>, FunctionCallError> {
    if !raw_path.starts_with(OAI_ENV_SCHEME_PREFIX) {
        return Ok(None);
    }

    let Some(remainder) = raw_path.strip_prefix(OAI_ENV_SCHEME) else {
        return Err(malformed_oai_env_uri(field_name, raw_path));
    };
    let Some((environment_id, uri_path)) = remainder.split_once('/') else {
        return Err(malformed_oai_env_uri(field_name, raw_path));
    };
    if environment_id.is_empty() {
        return Err(malformed_oai_env_uri(field_name, raw_path));
    }

    let path =
        absolute_uri_path(uri_path).ok_or_else(|| malformed_oai_env_uri(field_name, raw_path))?;
    let path = AbsolutePathBuf::try_from(path)
        .map_err(|err| FunctionCallError::RespondToModel(format!("invalid {field_name}: {err}")))?;
    Ok(Some(OaiEnvPath {
        environment_id,
        path,
    }))
}

fn selected_environment_id<'a>(
    explicit_environment_id: Option<&'a str>,
    qualified_environment_id: Option<&'a str>,
) -> Result<Option<&'a str>, FunctionCallError> {
    if matches!(explicit_environment_id, Some("")) {
        return Err(FunctionCallError::RespondToModel(
            "environment_id must be non-empty".to_string(),
        ));
    }

    if let (Some(explicit), Some(qualified)) = (explicit_environment_id, qualified_environment_id)
        && explicit != qualified
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "environment_id `{explicit}` does not match path environment `{qualified}`"
        )));
    }

    Ok(explicit_environment_id.or(qualified_environment_id))
}

fn absolute_uri_path(uri_path: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(uri_path);
    if direct.is_absolute() {
        return Some(direct);
    }

    let with_root = PathBuf::from(format!("/{uri_path}"));
    with_root.is_absolute().then_some(with_root)
}

fn malformed_oai_env_uri(field_name: &str, raw_path: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!(
        "malformed {field_name}: expected `oai_env://<environment_id>/<absolute-path>`, got `{raw_path}`"
    ))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::session::tests::make_session_and_context;

    #[test]
    fn parse_oai_env_uri_ignores_plain_paths() {
        assert!(parse_oai_env_uri("src/lib.rs", "path").unwrap().is_none());
    }

    #[test]
    fn parse_oai_env_uri_accepts_formatted_absolute_path() {
        let path = AbsolutePathBuf::current_dir()
            .expect("cwd")
            .join("src/lib.rs");
        let uri = format_oai_env_uri("remote", &path);

        let parsed = parse_oai_env_uri(&uri, "path")
            .expect("parse")
            .expect("qualified path");

        assert_eq!(parsed.environment_id, "remote");
        assert_eq!(parsed.path, path);
    }

    #[test]
    fn parse_oai_env_uri_rejects_malformed_scheme() {
        let err =
            parse_oai_env_uri("oai_env:/remote/tmp/file.txt", "path").expect_err("malformed path");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "malformed path: expected `oai_env://<environment_id>/<absolute-path>`, got `oai_env:/remote/tmp/file.txt`"
                    .to_string(),
            )
        );
    }

    #[test]
    fn selected_environment_id_rejects_explicit_path_mismatch() {
        let err =
            selected_environment_id(Some("local"), Some("remote")).expect_err("mismatched envs");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "environment_id `local` does not match path environment `remote`".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn resolve_tool_environment_defaults_to_primary_environment() {
        let (_session, turn) = make_session_and_context().await;

        let environment =
            resolve_tool_environment(&turn, /*environment_id*/ None, "example_tool")
                .expect("primary env");

        assert_eq!(
            environment.environment_id,
            codex_exec_server::LOCAL_ENVIRONMENT_ID
        );
    }

    #[tokio::test]
    async fn resolve_tool_environment_rejects_unknown_environment_id() {
        let (_session, turn) = make_session_and_context().await;

        let err = resolve_tool_environment(&turn, Some("missing"), "example_tool")
            .expect_err("unknown env");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel("unknown turn environment id `missing`".to_string(),)
        );
    }

    #[tokio::test]
    async fn resolve_environment_path_resolves_relative_paths_under_selected_cwd() {
        let (_session, turn) = make_session_and_context().await;
        let primary_environment = turn.primary_environment().expect("primary env");

        let resolved = resolve_environment_path(
            &turn,
            /*explicit_environment_id*/ None,
            "nested/file.txt",
            "path",
            "example_tool",
        )
        .expect("resolved path");

        assert_eq!(
            resolved.environment.environment_id,
            primary_environment.environment_id
        );
        assert_eq!(
            resolved.path,
            primary_environment.cwd.join("nested/file.txt")
        );
    }

    #[tokio::test]
    async fn resolve_environment_path_rejects_explicit_qualified_mismatch() {
        let (_session, turn) = make_session_and_context().await;
        let primary_environment = turn.primary_environment().expect("primary env");
        let uri = format_oai_env_uri(
            &primary_environment.environment_id,
            &primary_environment.cwd.join("file.txt"),
        );

        let err = resolve_environment_path(&turn, Some("missing"), &uri, "path", "example_tool")
            .expect_err("mismatch");

        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!(
                "environment_id `missing` does not match path environment `{}`",
                primary_environment.environment_id
            ))
        );
    }
}
