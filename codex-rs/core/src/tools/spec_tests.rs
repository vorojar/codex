use crate::config::test_config;
use crate::shell::Shell;
use crate::shell::ShellType;
use crate::test_support::construct_model_info_offline;
use crate::tools::ToolRouter;
use crate::tools::router::ToolRouterParams;
use codex_app_server_protocol::AppInfo;
use codex_features::Feature;
use codex_features::Features;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_models_manager::bundled_models_response;
use codex_models_manager::model_info::with_config_overrides;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::VIEW_IMAGE_TOOL_NAME;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_tools::AdditionalProperties;
use codex_tools::CommandToolOptions;
use codex_tools::ConfiguredToolSpec;
use codex_tools::DiscoverableTool;
use codex_tools::FreeformTool;
use codex_tools::JsonSchema;
use codex_tools::LoadableToolSpec;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ShellCommandBackendConfig;
use codex_tools::SpawnAgentToolOptions;
use codex_tools::TOOL_SEARCH_TOOL_NAME;
use codex_tools::TOOL_SUGGEST_TOOL_NAME;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::ToolsConfig;
use codex_tools::ToolsConfigParams;
use codex_tools::UnifiedExecShellMode;
use codex_tools::ViewImageToolOptions;
use codex_tools::WaitAgentTimeoutOptions;
use codex_tools::ZshForkConfig;
use codex_tools::create_apply_patch_freeform_tool;
use codex_tools::create_close_agent_tool_v1;
use codex_tools::create_close_agent_tool_v2;
use codex_tools::create_compact_parent_context_tool;
use codex_tools::create_exec_command_tool;
use codex_tools::create_list_agents_tool;
use codex_tools::create_list_agents_tool_v1;
use codex_tools::create_request_permissions_tool;
use codex_tools::create_request_user_input_tool;
use codex_tools::create_resume_agent_tool;
use codex_tools::create_send_input_tool_v1;
use codex_tools::create_send_message_tool;
use codex_tools::create_spawn_agent_tool_v1;
use codex_tools::create_spawn_agent_tool_v2;
use codex_tools::create_update_plan_tool;
use codex_tools::create_view_image_tool;
use codex_tools::create_wait_agent_tool_v1;
use codex_tools::create_wait_agent_tool_v2;
use codex_tools::create_watchdog_self_close_tool;
use codex_tools::create_write_stdin_tool;
use codex_tools::mcp_call_tool_result_output_schema;
use codex_tools::mcp_tool_to_deferred_responses_api_tool;
use codex_tools::request_permissions_tool_description;
use codex_tools::request_user_input_tool_description;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::assert_regex_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;
use crate::tools::tool_search_entry::build_tool_search_entries_for_config;

fn mcp_tool(name: &str, description: &str, input_schema: serde_json::Value) -> rmcp::model::Tool {
    rmcp::model::Tool {
        name: name.to_string().into(),
        title: None,
        description: Some(description.to_string().into()),
        input_schema: std::sync::Arc::new(rmcp::model::object(input_schema)),
        output_schema: None,
        annotations: None,
        execution: None,
        icons: None,
        meta: None,
    }
}

fn mcp_tool_info(tool: rmcp::model::Tool) -> ToolInfo {
    ToolInfo {
        server_name: "test_server".to_string(),
        callable_name: tool.name.to_string(),
        callable_namespace: "mcp__test_server__".to_string(),
        server_instructions: None,
        tool,
        connector_id: None,
        connector_name: None,
        plugin_display_names: Vec::new(),
        connector_description: None,
    }
}

fn mcp_tool_info_with_display_name(display_name: &str, tool: rmcp::model::Tool) -> ToolInfo {
    let (callable_namespace, callable_name) = display_name
        .rsplit_once('/')
        .map(|(namespace, callable_name)| (format!("{namespace}/"), callable_name.to_string()))
        .unwrap_or_else(|| ("".to_string(), display_name.to_string()));

    ToolInfo {
        server_name: "test_server".to_string(),
        callable_name,
        callable_namespace,
        server_instructions: None,
        tool,
        connector_id: None,
        connector_name: None,
        plugin_display_names: Vec::new(),
        connector_description: None,
    }
}

fn discoverable_connector(id: &str, name: &str, description: &str) -> DiscoverableTool {
    let slug = name.replace(' ', "-").to_lowercase();
    DiscoverableTool::Connector(Box::new(AppInfo {
        id: id.to_string(),
        name: name.to_string(),
        description: Some(description.to_string()),
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some(format!("https://chatgpt.com/apps/{slug}/{id}")),
        is_accessible: false,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }))
}

async fn search_capable_model_info() -> ModelInfo {
    let config = test_config().await;
    let mut model_info = construct_model_info_offline("gpt-5.4", &config);
    model_info.supports_search_tool = true;
    model_info
}

#[test]
fn deferred_responses_api_tool_serializes_with_defer_loading() {
    let tool = mcp_tool(
        "lookup_order",
        "Look up an order",
        serde_json::json!({
            "type": "object",
            "properties": {
                "order_id": {"type": "string"}
            },
            "required": ["order_id"],
            "additionalProperties": false,
        }),
    );

    let serialized = serde_json::to_value(ToolSpec::Function(
        mcp_tool_to_deferred_responses_api_tool(
            &ToolName::namespaced("mcp__codex_apps__", "lookup_order"),
            &tool,
        )
        .expect("convert deferred tool"),
    ))
    .expect("serialize deferred tool");

    assert_eq!(
        serialized,
        serde_json::json!({
            "type": "function",
            "name": "lookup_order",
            "description": "Look up an order",
            "strict": false,
            "defer_loading": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "order_id": {"type": "string"}
                },
                "required": ["order_id"],
                "additionalProperties": false,
            }
        })
    );
}

// Avoid order-based assertions; compare via set containment instead.
fn assert_contains_tool_names(tools: &[ConfiguredToolSpec], expected_subset: &[&str]) {
    use std::collections::HashSet;
    let mut names = HashSet::new();
    let mut duplicates = Vec::new();
    for name in tools.iter().map(ConfiguredToolSpec::name) {
        if !names.insert(name) {
            duplicates.push(name);
        }
    }
    assert!(
        duplicates.is_empty(),
        "duplicate tool entries detected: {duplicates:?}"
    );
    for expected in expected_subset {
        assert!(
            names.contains(expected),
            "expected tool {expected} to be present; had: {names:?}"
        );
    }
}

fn shell_tool_name(config: &ToolsConfig) -> Option<&'static str> {
    match config.shell_type {
        ConfigShellToolType::Default => Some("shell"),
        ConfigShellToolType::Local => Some("local_shell"),
        ConfigShellToolType::UnifiedExec => None,
        ConfigShellToolType::Disabled => None,
        ConfigShellToolType::ShellCommand => Some("shell_command"),
    }
}

fn find_tool<'a>(tools: &'a [ConfiguredToolSpec], expected_name: &str) -> &'a ConfiguredToolSpec {
    if let Some(tool) = tools.iter().find(|tool| tool.name() == expected_name) {
        return tool;
    }
    for tool in tools {
        let ToolSpec::Namespace(namespace) = &tool.spec else {
            continue;
        };
        if let Some(tool) = namespace.tools.iter().find_map(|tool| match tool {
            ResponsesApiNamespaceTool::Function(tool) if tool.name == expected_name => {
                Some(tool.clone())
            }
            _ => None,
        }) {
            return Box::leak(Box::new(ConfiguredToolSpec::new(
                ToolSpec::Function(tool),
                /*supports_parallel_tool_calls*/ false,
            )));
        }
    }
    panic!("expected tool {expected_name}")
}

fn find_namespaced_tool(
    tools: &[ConfiguredToolSpec],
    namespace_name: &str,
    expected_name: &str,
) -> ConfiguredToolSpec {
    let namespace = tools
        .iter()
        .find_map(|tool| match &tool.spec {
            ToolSpec::Namespace(namespace) if namespace.name == namespace_name => Some(namespace),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected namespace {namespace_name}"));

    let tool = namespace
        .tools
        .iter()
        .find_map(|tool| match tool {
            ResponsesApiNamespaceTool::Function(tool) if tool.name == expected_name => {
                Some(tool.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected tool {expected_name} in {namespace_name}"));

    ConfiguredToolSpec::new(
        ToolSpec::Function(tool),
        /*supports_parallel_tool_calls*/ false,
    )
}

fn assert_lacks_tool_name(tools: &[ConfiguredToolSpec], expected_absent: &str) {
    let names = tools
        .iter()
        .map(ConfiguredToolSpec::name)
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&expected_absent),
        "expected tool {expected_absent} to be absent; had: {names:?}"
    );
}

fn assert_contains_top_level_tool_name(tools: &[ConfiguredToolSpec], expected: &str) {
    let names = tools
        .iter()
        .map(ConfiguredToolSpec::name)
        .collect::<Vec<_>>();
    assert!(
        names.contains(&expected),
        "expected top-level tool {expected} to be present; had: {names:?}"
    );
}

fn assert_lacks_top_level_tool_name(tools: &[ConfiguredToolSpec], expected_absent: &str) {
    let names = tools
        .iter()
        .map(ConfiguredToolSpec::name)
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&expected_absent),
        "expected top-level tool {expected_absent} to be absent; had: {names:?}"
    );
}

fn request_user_input_tool_spec(default_mode_request_user_input: bool) -> ToolSpec {
    create_request_user_input_tool(request_user_input_tool_description(
        default_mode_request_user_input,
    ))
}

fn spawn_agent_tool_options(config: &ToolsConfig) -> SpawnAgentToolOptions<'_> {
    SpawnAgentToolOptions {
        available_models: &config.available_models,
        agent_type_description: if config.agent_type_description.is_empty() {
            crate::agent::role::spawn_tool_spec::build(&std::collections::BTreeMap::new())
        } else {
            config.agent_type_description.clone()
        },
        hide_agent_type_model_reasoning: config.hide_spawn_agent_metadata,
        include_usage_hint: config.spawn_agent_usage_hint,
        usage_hint_text: config.spawn_agent_usage_hint_text.clone(),
        max_concurrent_threads_per_session: config.max_concurrent_threads_per_session,
    }
}

fn wait_agent_timeout_options() -> WaitAgentTimeoutOptions {
    WaitAgentTimeoutOptions {
        default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
        min_timeout_ms: MIN_WAIT_TIMEOUT_MS,
        max_timeout_ms: MAX_WAIT_TIMEOUT_MS,
    }
}

fn create_watchdog_tools_namespace(tools: Vec<ToolSpec>) -> ToolSpec {
    let tools = tools
        .into_iter()
        .map(|tool| match tool {
            ToolSpec::Function(tool) => ResponsesApiNamespaceTool::Function(tool),
            ToolSpec::Namespace(_)
            | ToolSpec::Freeform(_)
            | ToolSpec::LocalShell {}
            | ToolSpec::ImageGeneration { .. }
            | ToolSpec::ToolSearch { .. }
            | ToolSpec::WebSearch { .. } => {
                panic!("watchdog namespace can only contain function tools")
            }
        })
        .collect();
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: "watchdog".to_string(),
        description:
            "Watchdog-only tools for parent-thread recovery and watchdog check-in lifecycle control."
                .to_string(),
        tools,
    })
}

fn strip_descriptions_schema(schema: &mut JsonSchema) {
    schema.description = None;
    if let Some(items) = schema.items.as_deref_mut() {
        strip_descriptions_schema(items);
    }
    if let Some(properties) = schema.properties.as_mut() {
        for value in properties.values_mut() {
            strip_descriptions_schema(value);
        }
    }
    if let Some(AdditionalProperties::Schema(schema)) = schema.additional_properties.as_mut() {
        strip_descriptions_schema(schema);
    }
    if let Some(any_of) = schema.any_of.as_mut() {
        for variant in any_of {
            strip_descriptions_schema(variant);
        }
    }
}

fn schema_object_parts<'a>(
    schema: &'a JsonSchema,
    expected_name: &str,
) -> (&'a BTreeMap<String, JsonSchema>, Option<&'a Vec<String>>) {
    let Some(properties) = schema.properties.as_ref() else {
        panic!("{expected_name} should use object params");
    };
    (properties, schema.required.as_ref())
}

fn strip_descriptions_tool(spec: &mut ToolSpec) {
    match spec {
        ToolSpec::ToolSearch { parameters, .. } => strip_descriptions_schema(parameters),
        ToolSpec::Function(ResponsesApiTool { parameters, .. }) => {
            strip_descriptions_schema(parameters);
        }
        ToolSpec::Namespace(_)
        | ToolSpec::Freeform(FreeformTool { .. })
        | ToolSpec::LocalShell {}
        | ToolSpec::ImageGeneration { .. }
        | ToolSpec::WebSearch { .. } => {}
    }
}

fn find_namespace_function_tool<'a>(
    tools: &'a [ConfiguredToolSpec],
    expected_namespace: &str,
    expected_name: &str,
) -> &'a ResponsesApiTool {
    let namespace_tool = find_tool(tools, expected_namespace);
    let ToolSpec::Namespace(namespace) = &namespace_tool.spec else {
        panic!("expected namespace tool {expected_namespace}");
    };
    namespace
        .tools
        .iter()
        .find_map(|tool| match tool {
            ResponsesApiNamespaceTool::Function(tool) if tool.name == expected_name => Some(tool),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected tool {expected_namespace}{expected_name} in namespace"))
}

async fn multi_agent_v2_tools_config() -> ToolsConfig {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::Collab);
    features.enable(Feature::MultiAgentV2);
    let available_models = Vec::new();
    ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    })
    .with_max_concurrent_threads_per_session(Some(4))
}

fn multi_agent_v2_spawn_agent_description(tools_config: &ToolsConfig) -> String {
    let (tools, _) = build_specs(
        tools_config,
        /*mcp_tools*/ None,
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();
    let spawn_agent = find_tool(&tools, "spawn_agent");
    let ToolSpec::Function(ResponsesApiTool { description, .. }) = &spawn_agent.spec else {
        panic!("spawn_agent should be a function tool");
    };
    description.clone()
}

async fn model_info_from_models_json(slug: &str) -> ModelInfo {
    let config = test_config().await;
    let response = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    let model = response
        .models
        .into_iter()
        .find(|candidate| candidate.slug == slug)
        .unwrap_or_else(|| panic!("model slug {slug} is missing from models.json"));
    with_config_overrides(model, &config.to_models_manager_config())
}

/// Builds the tool registry builder while collecting tool specs for later serialization.
fn build_specs(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, ToolInfo>>,
    deferred_mcp_tools: Option<HashMap<String, ToolInfo>>,
    dynamic_tools: &[DynamicToolSpec],
) -> ToolRegistryBuilder {
    build_specs_with_unavailable_tools(
        config,
        mcp_tools,
        deferred_mcp_tools,
        Vec::new(),
        dynamic_tools,
    )
}

fn build_specs_with_unavailable_tools(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, ToolInfo>>,
    deferred_mcp_tools: Option<HashMap<String, ToolInfo>>,
    unavailable_called_tools: Vec<ToolName>,
    dynamic_tools: &[DynamicToolSpec],
) -> ToolRegistryBuilder {
    build_specs_with_discoverable_tools(
        config,
        mcp_tools,
        deferred_mcp_tools,
        unavailable_called_tools,
        /*discoverable_tools*/ None,
        dynamic_tools,
    )
}

#[tokio::test]
async fn model_provided_unified_exec_is_blocked_for_windows_sandboxed_policies() {
    let mut model_info = model_info_from_models_json("gpt-5.4").await;
    model_info.shell_type = ConfigShellToolType::UnifiedExec;
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::workspace_write(),
        windows_sandbox_level: WindowsSandboxLevel::RestrictedToken,
    });

    let expected_shell_type = if cfg!(target_os = "windows") {
        ConfigShellToolType::ShellCommand
    } else {
        ConfigShellToolType::UnifiedExec
    };
    assert_eq!(config.shell_type, expected_shell_type);
}

#[tokio::test]
async fn test_full_toolset_specs_for_gpt5_codex_unified_exec_web_search() {
    let model_info = model_info_from_models_json("gpt-5-codex").await;
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();

    // Build actual map name -> spec
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    let mut actual: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
    let mut duplicate_names = Vec::new();
    for t in &tools {
        let name = t.name().to_string();
        if actual.insert(name.clone(), t.spec.clone()).is_some() {
            duplicate_names.push(name);
        }
    }
    assert!(
        duplicate_names.is_empty(),
        "duplicate tool entries detected: {duplicate_names:?}"
    );

    // Build expected from the same helpers used by the builder.
    let mut expected: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
    for spec in [
        create_exec_command_tool(CommandToolOptions {
            allow_login_shell: true,
            exec_permission_approvals_enabled: false,
        }),
        create_write_stdin_tool(),
        create_update_plan_tool(),
        request_user_input_tool_spec(/*default_mode_request_user_input*/ false),
        create_apply_patch_freeform_tool(),
        ToolSpec::WebSearch {
            external_web_access: Some(true),
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        },
        create_view_image_tool(ViewImageToolOptions {
            can_request_original_image_detail: config.can_request_original_image_detail,
        }),
    ] {
        expected.insert(spec.name().to_string(), spec);
    }
    let mut collab_specs = if config.multi_agent_v2 {
        vec![
            create_spawn_agent_tool_v2(spawn_agent_tool_options(&config)),
            create_send_message_tool(),
            create_wait_agent_tool_v2(wait_agent_timeout_options()),
            create_close_agent_tool_v2(),
            create_list_agents_tool(),
        ]
    } else {
        let mut collab_specs = vec![
            create_spawn_agent_tool_v1(spawn_agent_tool_options(&config)),
            create_send_input_tool_v1(),
            create_resume_agent_tool(),
            create_wait_agent_tool_v1(wait_agent_timeout_options()),
            create_close_agent_tool_v1(),
        ];
        if config.agent_watchdog {
            collab_specs.push(create_list_agents_tool_v1(config.agent_watchdog));
        }
        collab_specs
    };
    for spec in collab_specs.split_off(0) {
        expected.insert(spec.name().to_string(), spec);
    }
    if config.agent_watchdog {
        let spec = create_watchdog_tools_namespace(vec![
            create_compact_parent_context_tool(),
            create_watchdog_self_close_tool(),
        ]);
        expected.insert(spec.name().to_string(), spec);
    }

    if config.exec_permission_approvals_enabled {
        let spec = create_request_permissions_tool(request_permissions_tool_description());
        expected.insert(spec.name().to_string(), spec);
    }

    // Exact name set match — this is the only test allowed to fail when tools change.
    let actual_names: HashSet<_> = actual.keys().cloned().collect();
    let expected_names: HashSet<_> = expected.keys().cloned().collect();
    assert_eq!(actual_names, expected_names, "tool name set mismatch");

    // Compare specs ignoring human-readable descriptions.
    for name in expected.keys() {
        let mut a = actual.get(name).expect("present").clone();
        let mut e = expected.get(name).expect("present").clone();
        strip_descriptions_tool(&mut a);
        strip_descriptions_tool(&mut e);
        assert_eq!(a, e, "spec mismatch for {name}");
    }
}

#[tokio::test]
async fn test_build_specs_collab_tools_enabled() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::Collab);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    assert_contains_tool_names(
        &tools,
        &["spawn_agent", "send_input", "wait_agent", "close_agent"],
    );
    assert_lacks_tool_name(&tools, "spawn_agents_on_csv");
    assert_lacks_tool_name(&tools, "list_agents");
    assert_lacks_tool_name(&tools, "watchdog");
}

#[tokio::test]
async fn test_build_specs_watchdog_collab_tools_include_self_close_tool() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::Collab);
    features.enable(Feature::AgentWatchdog);
    features.normalize_dependencies();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();

    assert_contains_top_level_tool_name(&tools, "watchdog");
    assert_contains_top_level_tool_name(&tools, "spawn_agent");
    assert_contains_top_level_tool_name(&tools, "send_input");
    assert_contains_top_level_tool_name(&tools, "wait_agent");
    assert_contains_top_level_tool_name(&tools, "close_agent");
    assert_contains_top_level_tool_name(&tools, "list_agents");
    assert_contains_tool_names(&tools, &["list_agents", "close_agent"]);
    assert_lacks_top_level_tool_name(&tools, "watchdog_self_close");
    assert_lacks_top_level_tool_name(&tools, "compact_parent_context");

    let watchdog_self_close = find_namespaced_tool(&tools, "watchdog", "watchdog_self_close");
    let ToolSpec::Function(ResponsesApiTool {
        defer_loading: Some(deferred),
        ..
    }) = &watchdog_self_close.spec
    else {
        panic!("watchdog_self_close should be a function tool");
    };
    assert!(*deferred);
    let compact_parent_context = find_namespaced_tool(&tools, "watchdog", "compact_parent_context");
    let ToolSpec::Function(ResponsesApiTool {
        defer_loading: Some(deferred),
        ..
    }) = &compact_parent_context.spec
    else {
        panic!("compact_parent_context should be a function tool");
    };
    assert!(*deferred);
}

#[tokio::test]
async fn test_build_specs_multi_agent_v2_uses_task_names_and_hides_resume() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::Collab);
    features.enable(Feature::MultiAgentV2);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    assert_contains_tool_names(
        &tools,
        &[
            "spawn_agent",
            "send_message",
            "assign_task",
            "wait_agent",
            "close_agent",
            "list_agents",
        ],
    );

    let spawn_agent = find_tool(&tools, "spawn_agent");
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = &spawn_agent.spec
    else {
        panic!("spawn_agent should be a function tool");
    };
    let (properties, required) = schema_object_parts(parameters, "spawn_agent");
    assert!(properties.contains_key("task_name"));
    assert_eq!(
        required,
        Some(&vec!["task_name".to_string(), "message".to_string()])
    );
    let output_schema = output_schema
        .as_ref()
        .expect("spawn_agent should define output schema");
    assert_eq!(
        output_schema["required"],
        json!(["agent_id", "task_name", "nickname"])
    );

    let send_message = find_tool(&tools, "send_message");
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &send_message.spec else {
        panic!("send_message should be a function tool");
    };
    let (properties, required) = schema_object_parts(parameters, "send_message");
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(!properties.contains_key("items"));
    assert_eq!(
        required,
        Some(&vec!["target".to_string(), "message".to_string()])
    );

    let assign_task = find_tool(&tools, "assign_task");
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &assign_task.spec else {
        panic!("assign_task should be a function tool");
    };
    let (properties, required) = schema_object_parts(parameters, "assign_task");
    assert!(properties.contains_key("target"));
    assert!(properties.contains_key("message"));
    assert!(!properties.contains_key("items"));
    assert_eq!(
        required,
        Some(&vec!["target".to_string(), "message".to_string()])
    );

    let wait_agent = find_tool(&tools, "wait_agent");
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = &wait_agent.spec
    else {
        panic!("wait_agent should be a function tool");
    };
    let (properties, required) = schema_object_parts(parameters, "wait_agent");
    assert!(properties.contains_key("timeout_ms"));
    assert!(!properties.contains_key("targets"));
    assert_eq!(required, None);
    let output_schema = output_schema
        .as_ref()
        .expect("wait_agent should define output schema");
    assert_eq!(
        output_schema["properties"]["message"]["description"],
        json!("Brief wait summary without the agent's final content.")
    );

    let list_agents = find_tool(&tools, "list_agents");
    let ToolSpec::Function(ResponsesApiTool {
        parameters,
        output_schema,
        ..
    }) = &list_agents.spec
    else {
        panic!("list_agents should be a function tool");
    };
    let (properties, required) = schema_object_parts(parameters, "list_agents");
    assert!(properties.contains_key("path_prefix"));
    assert_eq!(required, None);
    let output_schema = output_schema
        .as_ref()
        .expect("list_agents should define output schema");
    assert_eq!(
        output_schema["properties"]["agents"]["items"]["required"],
        json!(["agent_name", "agent_status", "last_task_message"])
    );
    assert_lacks_tool_name(&tools, "send_input");
    assert_lacks_tool_name(&tools, "resume_agent");
}

#[tokio::test]
async fn test_build_specs_enable_fanout_enables_agent_jobs_and_collab_tools() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::SpawnCsv);
    features.normalize_dependencies();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    assert_contains_tool_names(
        &tools,
        &[
            "spawn_agent",
            "send_input",
            "wait_agent",
            "close_agent",
            "spawn_agents_on_csv",
        ],
    );
}

#[tokio::test]
async fn view_image_tool_omits_detail_without_original_detail_feature() {
    let config = test_config().await;
    let mut model_info = construct_model_info_offline("gpt-5-codex", &config);
    model_info.supports_image_detail_original = true;
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    let view_image = find_tool(&tools, VIEW_IMAGE_TOOL_NAME);
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &view_image.spec else {
        panic!("view_image should be a function tool");
    };
    let (properties, _) = schema_object_parts(parameters, "view_image");
    assert!(!properties.contains_key("detail"));
}

#[tokio::test]
async fn view_image_tool_includes_detail_with_original_detail_feature() {
    let config = test_config().await;
    let mut model_info = construct_model_info_offline("gpt-5-codex", &config);
    model_info.supports_image_detail_original = true;
    let mut features = Features::with_defaults();
    features.enable(Feature::ImageDetailOriginal);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    let view_image = find_tool(&tools, VIEW_IMAGE_TOOL_NAME);
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &view_image.spec else {
        panic!("view_image should be a function tool");
    };
    let (properties, _) = schema_object_parts(parameters, "view_image");
    assert!(properties.contains_key("detail"));
    let Some(description) = properties
        .get("detail")
        .and_then(|schema| schema.description.as_ref())
    else {
        panic!("view_image detail should include a description");
    };
    assert!(description.contains("only supported value is `original`"));
    assert!(description.contains("omit this field for default resized behavior"));
}

#[tokio::test]
async fn test_build_specs_agent_job_worker_tools_enabled() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::SpawnCsv);
    features.normalize_dependencies();
    features.enable(Feature::Sqlite);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::SubAgent(SubAgentSource::Other(
            "agent_job:test".to_string(),
        )),
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    assert_contains_tool_names(
        &tools,
        &[
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
            "spawn_agents_on_csv",
            "report_agent_job_result",
        ],
    );
    assert_lacks_tool_name(&tools, "request_user_input");
}

#[tokio::test]
async fn request_user_input_description_reflects_default_mode_feature_flag() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    let request_user_input_tool = find_tool(&tools, "request_user_input");
    assert_eq!(
        request_user_input_tool.spec,
        request_user_input_tool_spec(/*default_mode_request_user_input*/ false)
    );

    features.enable(Feature::DefaultModeRequestUserInput);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    let request_user_input_tool = find_tool(&tools, "request_user_input");
    assert_eq!(
        request_user_input_tool.spec,
        request_user_input_tool_spec(/*default_mode_request_user_input*/ true)
    );
}

#[tokio::test]
async fn request_permissions_requires_feature_flag() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let features = Features::with_defaults();
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    assert_lacks_tool_name(&tools, "request_permissions");

    let mut features = Features::with_defaults();
    features.enable(Feature::RequestPermissionsTool);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();
    let request_permissions_tool = find_tool(&tools, "request_permissions");
    assert_eq!(
        request_permissions_tool.spec,
        create_request_permissions_tool(request_permissions_tool_description())
    );
}

#[tokio::test]
async fn request_permissions_tool_is_independent_from_additional_permissions() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5-codex", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::ExecPermissionApprovals);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*app_tools*/ None,
        &[],
    )
    .build();

    assert_lacks_tool_name(&tools, "request_permissions");
}

#[tokio::test]
async fn get_memory_requires_feature_flag() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.disable(Feature::MemoryTool);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();
    assert!(
        !tools.iter().any(|t| t.spec.name() == "get_memory"),
        "get_memory should be disabled when memory_tool feature is off"
    );
}

async fn assert_model_tools(
    model_slug: &str,
    features: &Features,
    web_search_mode: Option<WebSearchMode>,
    expected_tools: &[&str],
) {
    let _config = test_config().await;
    let model_info = model_info_from_models_json(model_slug).await;
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features,
        image_generation_tool_auth_allowed: true,
        web_search_mode,
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let router = ToolRouter::from_config(
        &tools_config,
        ToolRouterParams {
            mcp_tools: None,
            deferred_mcp_tools: None,
            unavailable_called_tools: Vec::new(),
            parallel_mcp_server_names: std::collections::HashSet::new(),
            discoverable_tools: None,
            dynamic_tools: &[],
        },
    );
    let model_visible_specs = router.model_visible_specs();
    let tool_names = model_visible_specs
        .iter()
        .flat_map(|tool| match tool {
            ToolSpec::Namespace(namespace) => namespace
                .tools
                .iter()
                .map(|tool| match tool {
                    codex_tools::ResponsesApiNamespaceTool::Function(tool) => tool.name.as_str(),
                })
                .collect::<Vec<_>>(),
            _ => vec![tool.name()],
        })
        .collect::<Vec<_>>();
    assert_eq!(&tool_names, &expected_tools,);
}

async fn assert_default_model_tools(
    model_slug: &str,
    features: &Features,
    web_search_mode: Option<WebSearchMode>,
    shell_tool: &'static str,
    expected_tail: &[&str],
) {
    let mut expected = if features.enabled(Feature::UnifiedExec) {
        vec!["exec_command", "write_stdin"]
    } else {
        vec![shell_tool]
    };
    expected.extend(expected_tail);
    assert_model_tools(model_slug, features, web_search_mode, &expected).await;
}

#[tokio::test]
async fn test_build_specs_gpt5_codex_default() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_build_specs_gpt51_codex_default() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_build_specs_gpt5_codex_unified_exec_web_search() {
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    assert_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Live),
        &[
            "exec_command",
            "write_stdin",
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_build_specs_gpt51_codex_unified_exec_web_search() {
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    assert_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Live),
        &[
            "exec_command",
            "write_stdin",
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_gpt_5_1_codex_max_defaults() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_codex_5_1_mini_defaults() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.4-mini",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_gpt_5_defaults() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.2",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_gpt_5_1_defaults() {
    let features = Features::with_defaults();
    assert_default_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Cached),
        "shell_command",
        &[
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_gpt_5_1_codex_max_unified_exec_web_search() {
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    assert_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Live),
        &[
            "exec_command",
            "write_stdin",
            "update_plan",
            "request_user_input",
            "apply_patch",
            "web_search",
            "image_generation",
            "view_image",
            "spawn_agent",
            "send_input",
            "resume_agent",
            "wait_agent",
            "close_agent",
        ],
    )
    .await;
}

#[tokio::test]
async fn test_build_specs_default_shell_present() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("o3", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::new()),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    // Only check the shell variant and a couple of core tools.
    let mut subset = vec!["exec_command", "write_stdin", "update_plan"];
    if let Some(shell_tool) = shell_tool_name(&tools_config) {
        subset.push(shell_tool);
    }
    assert_contains_tool_names(&tools, &subset);
}

#[tokio::test]
async fn shell_zsh_fork_prefers_shell_command_over_unified_exec() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("o3", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    features.enable(Feature::ShellZshFork);

    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Live),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let user_shell = Shell {
        shell_type: ShellType::Zsh,
        shell_path: PathBuf::from("/bin/zsh"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };

    assert_eq!(tools_config.shell_type, ConfigShellToolType::ShellCommand);
    assert_eq!(
        tools_config.shell_command_backend,
        ShellCommandBackendConfig::ZshFork
    );
    assert_eq!(
        tools_config.unified_exec_shell_mode,
        UnifiedExecShellMode::Direct
    );
    assert_eq!(
        tools_config
            .with_unified_exec_shell_mode_for_session(
                tool_user_shell_type(&user_shell),
                Some(&PathBuf::from(if cfg!(windows) {
                    r"C:\opt\codex\zsh"
                } else {
                    "/opt/codex/zsh"
                })),
                Some(&PathBuf::from(if cfg!(windows) {
                    r"C:\opt\codex\codex-execve-wrapper"
                } else {
                    "/opt/codex/codex-execve-wrapper"
                })),
            )
            .unified_exec_shell_mode,
        if cfg!(unix) {
            UnifiedExecShellMode::ZshFork(ZshForkConfig {
                shell_zsh_path: AbsolutePathBuf::from_absolute_path("/opt/codex/zsh").unwrap(),
                main_execve_wrapper_exe: AbsolutePathBuf::from_absolute_path(
                    "/opt/codex/codex-execve-wrapper",
                )
                .unwrap(),
            })
        } else {
            UnifiedExecShellMode::Direct
        }
    );
}

#[tokio::test]
async fn spawn_agent_description_omits_usage_hint_when_disabled() {
    let tools_config = multi_agent_v2_tools_config()
        .await
        .with_spawn_agent_usage_hint(/*spawn_agent_usage_hint*/ false);
    let description = multi_agent_v2_spawn_agent_description(&tools_config);

    assert_regex_match(
        r#"(?sx)
            ^\s*
            No\ picker-visible\ model\ overrides\ are\ currently\ loaded\.
            \s+Spawns\ an\ agent\ to\ work\ on\ the\ specified\ task\.\ If\ your\ current\ task\ is\ `/root/task1`\ and\ you\ spawn_agent\ with\ task_name\ "task_3"\ the\ agent\ will\ have\ canonical\ task\ name\ `/root/task1/task_3`\.
            \s+You\ are\ then\ able\ to\ refer\ to\ this\ agent\ as\ `task_3`\ or\ `/root/task1/task_3`\ interchangeably\.\ However\ an\ agent\ `/root/task2/task_3`\ would\ only\ be\ able\ to\ communicate\ with\ this\ agent\ via\ its\ canonical\ name\ `/root/task1/task_3`\.
            \s+The\ spawned\ agent\ will\ have\ the\ same\ tools\ as\ you\ and\ the\ ability\ to\ spawn\ its\ own\ subagents\.
            \s+Spawned\ agents\ inherit\ your\ current\ model\ by\ default\.\ Omit\ `model`\ to\ use\ that\ preferred\ default;\ set\ `model`\ only\ when\ an\ explicit\ override\ is\ needed\.
            \s+It\ will\ be\ able\ to\ send\ you\ and\ other\ running\ agents\ messages,\ and\ its\ final\ answer\ will\ be\ provided\ to\ you\ when\ it\ finishes\.
            \s+The\ new\ agent's\ canonical\ task\ name\ will\ be\ provided\ to\ it\ along\ with\ the\ message\.
            \s+This\ session\ is\ configured\ with\ `max_concurrent_threads_per_session\ =\ 4`\ for\ concurrently\ open\ agent\ threads\.
            \s*$
        "#,
        &description,
    );
}

#[tokio::test]
async fn spawn_agent_description_uses_configured_usage_hint_text() {
    let tools_config = multi_agent_v2_tools_config()
        .await
        .with_spawn_agent_usage_hint_text(Some(
            /*spawn_agent_usage_hint_text*/ "Custom delegation guidance only.".to_string(),
        ));
    let description = multi_agent_v2_spawn_agent_description(&tools_config);

    assert_regex_match(
        r#"(?sx)
            ^\s*
            No\ picker-visible\ model\ overrides\ are\ currently\ loaded\.
            \s+Spawns\ an\ agent\ to\ work\ on\ the\ specified\ task\.\ If\ your\ current\ task\ is\ `/root/task1`\ and\ you\ spawn_agent\ with\ task_name\ "task_3"\ the\ agent\ will\ have\ canonical\ task\ name\ `/root/task1/task_3`\.
            \s+You\ are\ then\ able\ to\ refer\ to\ this\ agent\ as\ `task_3`\ or\ `/root/task1/task_3`\ interchangeably\.\ However\ an\ agent\ `/root/task2/task_3`\ would\ only\ be\ able\ to\ communicate\ with\ this\ agent\ via\ its\ canonical\ name\ `/root/task1/task_3`\.
            \s+The\ spawned\ agent\ will\ have\ the\ same\ tools\ as\ you\ and\ the\ ability\ to\ spawn\ its\ own\ subagents\.
            \s+Spawned\ agents\ inherit\ your\ current\ model\ by\ default\.\ Omit\ `model`\ to\ use\ that\ preferred\ default;\ set\ `model`\ only\ when\ an\ explicit\ override\ is\ needed\.
            \s+It\ will\ be\ able\ to\ send\ you\ and\ other\ running\ agents\ messages,\ and\ its\ final\ answer\ will\ be\ provided\ to\ you\ when\ it\ finishes\.
            \s+The\ new\ agent's\ canonical\ task\ name\ will\ be\ provided\ to\ it\ along\ with\ the\ message\.
            \s+This\ session\ is\ configured\ with\ `max_concurrent_threads_per_session\ =\ 4`\ for\ concurrently\ open\ agent\ threads\.
            \s+Custom\ delegation\ guidance\ only\.
            \s*$
        "#,
        &description,
    );
}

#[tokio::test]
async fn multi_agent_v2_wait_agent_schema_uses_configured_min_timeout() {
    let wait_agent_min_timeout_ms = Some(60_000);
    let tools_config = multi_agent_v2_tools_config()
        .await
        .with_wait_agent_min_timeout_ms(wait_agent_min_timeout_ms);
    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();
    let wait_agent = find_tool(&tools, "wait_agent");
    let ToolSpec::Function(ResponsesApiTool { parameters, .. }) = &wait_agent.spec else {
        panic!("wait_agent should be a function tool");
    };
    let timeout_description = parameters
        .properties
        .as_ref()
        .and_then(|properties| properties.get("timeout_ms"))
        .and_then(|schema| schema.description.as_deref());

    assert_eq!(
        timeout_description,
        Some("Optional timeout in milliseconds. Defaults to 60000, min 60000, max 3600000.")
    );
}

#[tokio::test]
async fn tool_suggest_requires_apps_and_plugins_features() {
    let model_info = search_capable_model_info().await;
    let discoverable_tools = Some(vec![discoverable_connector(
        "connector_2128aebfecb84f64a069897515042a44",
        "Google Calendar",
        "Plan events and schedules.",
    )]);
    let available_models = Vec::new();

    for disabled_feature in [Feature::Apps, Feature::Plugins] {
        let mut features = Features::with_defaults();
        features.enable(Feature::ToolSearch);
        features.enable(Feature::ToolSuggest);
        features.enable(Feature::Apps);
        features.enable(Feature::Plugins);
        features.disable(disabled_feature);

        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            available_models: &available_models,
            features: &features,
            image_generation_tool_auth_allowed: true,
            web_search_mode: Some(WebSearchMode::Cached),
            session_source: SessionSource::Cli,
            permission_profile: &PermissionProfile::Disabled,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
        });
        let (tools, _) = build_specs_with_discoverable_tools(
            &tools_config,
            /*mcp_tools*/ None,
            /*deferred_mcp_tools*/ None,
            Vec::new(),
            discoverable_tools.clone(),
            &[],
        )
        .build();

        assert!(
            !tools
                .iter()
                .any(|tool| tool.name() == TOOL_SUGGEST_TOOL_NAME),
            "tool_suggest should be absent when {disabled_feature:?} is disabled"
        );
    }
}

#[tokio::test]
async fn search_tool_description_handles_no_enabled_mcp_tools() {
    let model_info = search_capable_model_info().await;
    let mut features = Features::with_defaults();
    features.enable(Feature::Apps);
    features.enable(Feature::ToolSearch);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        Some(HashMap::new()),
        &[],
    )
    .build();
    let search_tool = find_tool(&tools, TOOL_SEARCH_TOOL_NAME);
    let ToolSpec::ToolSearch { description, .. } = &search_tool.spec else {
        panic!("expected tool_search tool");
    };

    assert!(description.contains("None currently enabled."));
    assert!(!description.contains("{{source_descriptions}}"));
}

#[tokio::test]
async fn search_tool_description_falls_back_to_connector_name_without_description() {
    let model_info = search_capable_model_info().await;
    let mut features = Features::with_defaults();
    features.enable(Feature::Apps);
    features.enable(Feature::ToolSearch);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        Some(HashMap::from([(
            "mcp__codex_apps__calendar_create_event".to_string(),
            ToolInfo {
                server_name: CODEX_APPS_MCP_SERVER_NAME.to_string(),
                callable_name: "_create_event".to_string(),
                callable_namespace: "mcp__codex_apps__calendar".to_string(),
                server_instructions: None,
                tool: mcp_tool(
                    "calendar_create_event",
                    "Create calendar event",
                    serde_json::json!({"type": "object"}),
                ),
                connector_id: Some("calendar".to_string()),
                connector_name: Some("Calendar".to_string()),
                plugin_display_names: Vec::new(),
                connector_description: None,
            },
        )])),
        &[],
    )
    .build();
    let search_tool = find_tool(&tools, TOOL_SEARCH_TOOL_NAME);
    let ToolSpec::ToolSearch { description, .. } = &search_tool.spec else {
        panic!("expected tool_search tool");
    };

    assert!(description.contains("- Calendar"));
    assert!(!description.contains("- Calendar:"));
}

#[tokio::test]
async fn search_tool_registers_namespaced_mcp_tool_aliases() {
    let model_info = search_capable_model_info().await;
    let mut features = Features::with_defaults();
    features.enable(Feature::Apps);
    features.enable(Feature::ToolSearch);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (_, registry) = build_specs(
        &tools_config,
        /*mcp_tools*/ None,
        Some(HashMap::from([
            (
                "mcp__codex_apps__calendar_create_event".to_string(),
                ToolInfo {
                    server_name: CODEX_APPS_MCP_SERVER_NAME.to_string(),
                    callable_name: "_create_event".to_string(),
                    callable_namespace: "mcp__codex_apps__calendar".to_string(),
                    server_instructions: None,
                    tool: mcp_tool(
                        "calendar-create-event",
                        "Create calendar event",
                        serde_json::json!({"type": "object"}),
                    ),
                    connector_id: Some("calendar".to_string()),
                    connector_name: Some("Calendar".to_string()),
                    connector_description: None,
                    plugin_display_names: Vec::new(),
                },
            ),
            (
                "mcp__codex_apps__calendar_list_events".to_string(),
                ToolInfo {
                    server_name: CODEX_APPS_MCP_SERVER_NAME.to_string(),
                    callable_name: "_list_events".to_string(),
                    callable_namespace: "mcp__codex_apps__calendar".to_string(),
                    server_instructions: None,
                    tool: mcp_tool(
                        "calendar-list-events",
                        "List calendar events",
                        serde_json::json!({"type": "object"}),
                    ),
                    connector_id: Some("calendar".to_string()),
                    connector_name: Some("Calendar".to_string()),
                    connector_description: None,
                    plugin_display_names: Vec::new(),
                },
            ),
            (
                "mcp__rmcp__echo".to_string(),
                ToolInfo {
                    server_name: "rmcp".to_string(),
                    callable_name: "echo".to_string(),
                    callable_namespace: "mcp__rmcp__".to_string(),
                    server_instructions: None,
                    tool: mcp_tool("echo", "Echo", serde_json::json!({"type": "object"})),
                    connector_id: None,
                    connector_name: None,
                    connector_description: None,
                    plugin_display_names: Vec::new(),
                },
            ),
        ])),
        &[],
    )
    .build();

    let app_alias = ToolName::namespaced("mcp__codex_apps__calendar", "_create_event");
    let mcp_alias = ToolName::namespaced("mcp__rmcp__", "echo");

    assert!(registry.has_handler(&ToolName::plain(TOOL_SEARCH_TOOL_NAME)));
    assert!(registry.has_handler(&app_alias));
    assert!(registry.has_handler(&mcp_alias));
}

#[tokio::test]
async fn tool_search_entries_skip_namespace_outputs_when_namespace_tools_are_disabled() {
    let model_info = search_capable_model_info().await;
    let mut features = Features::with_defaults();
    features.enable(Feature::ToolSearch);
    let available_models = Vec::new();
    let mut tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    tools_config.namespace_tools = false;
    let mcp_tools = HashMap::from([(
        "mcp__test_server__echo".to_string(),
        mcp_tool_info(mcp_tool(
            "echo",
            "Echo",
            serde_json::json!({"type": "object"}),
        )),
    )]);
    let dynamic_tools = vec![
        DynamicToolSpec {
            namespace: Some("codex_app".to_string()),
            name: "automation_update".to_string(),
            description: "Create or update automations.".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            defer_loading: true,
        },
        DynamicToolSpec {
            namespace: None,
            name: "plain_dynamic".to_string(),
            description: "Plain dynamic tool.".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            defer_loading: true,
        },
    ];

    let entries =
        build_tool_search_entries_for_config(&tools_config, Some(&mcp_tools), &dynamic_tools);
    let outputs = entries
        .into_iter()
        .map(|entry| entry.output)
        .collect::<Vec<_>>();

    assert_eq!(outputs.len(), 1);
    match &outputs[0] {
        LoadableToolSpec::Function(tool) => assert_eq!(tool.name, "plain_dynamic"),
        LoadableToolSpec::Namespace(_) => panic!("namespace tool_search output should be hidden"),
    }
}

#[tokio::test]
async fn direct_mcp_tools_register_namespaced_handlers() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (_, registry) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "mcp__test_server__echo".to_string(),
            mcp_tool_info(mcp_tool(
                "echo",
                "Echo",
                serde_json::json!({"type": "object"}),
            )),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    assert!(registry.has_handler(&ToolName::namespaced("mcp__test_server__", "echo")));
    assert!(!registry.has_handler(&ToolName::plain("mcp__test_server__echo")));
}

#[tokio::test]
async fn unavailable_mcp_tools_are_exposed_as_dummy_function_tools() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let unavailable_tool = ToolName::namespaced("mcp__codex_apps__calendar", "_create_event");
    let (tools, registry) = build_specs_with_unavailable_tools(
        &tools_config,
        /*mcp_tools*/ None,
        /*deferred_mcp_tools*/ None,
        vec![unavailable_tool],
        &[],
    )
    .build();

    let tool = find_tool(&tools, "mcp__codex_apps__calendar_create_event");
    let ToolSpec::Function(ResponsesApiTool {
        description,
        parameters,
        ..
    }) = &tool.spec
    else {
        panic!("unavailable MCP tool should be exposed as a function tool");
    };
    assert!(description.contains("not currently available"));
    assert_eq!(
        parameters.additional_properties,
        Some(AdditionalProperties::Boolean(false))
    );
    assert!(registry.has_handler(&ToolName::namespaced(
        "mcp__codex_apps__calendar",
        "_create_event"
    )));
    assert!(!registry.has_handler(&ToolName::plain("mcp__codex_apps__calendar_create_event")));
}

#[tokio::test]
async fn test_mcp_tool_property_missing_type_defaults_to_string() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/search".to_string(),
            mcp_tool_info_with_display_name(
                "dash/search",
                mcp_tool(
                    "search",
                    "Search docs",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"description": "search query"}
                        }
                    }),
                ),
            ),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    let tool = find_namespace_function_tool(&tools, "dash/", "search");
    assert_eq!(
        *tool,
        ResponsesApiTool {
            name: "search".to_string(),
            parameters: JsonSchema::object(
                /*properties*/
                BTreeMap::from([(
                    "query".to_string(),
                    JsonSchema::string(Some("search query".to_string())),
                )]),
                /*required*/ None,
                /*additional_properties*/ None
            ),
            description: "Search docs".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        }
    );
}

#[tokio::test]
async fn test_mcp_tool_preserves_integer_schema() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/paginate".to_string(),
            mcp_tool_info_with_display_name(
                "dash/paginate",
                mcp_tool(
                    "paginate",
                    "Pagination",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"page": {"type": "integer"}}
                    }),
                ),
            ),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    let tool = find_namespace_function_tool(&tools, "dash/", "paginate");
    assert_eq!(
        *tool,
        ResponsesApiTool {
            name: "paginate".to_string(),
            parameters: JsonSchema::object(
                /*properties*/
                BTreeMap::from([(
                    "page".to_string(),
                    JsonSchema::integer(/*description*/ None),
                )]),
                /*required*/ None,
                /*additional_properties*/ None
            ),
            description: "Pagination".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        }
    );
}

#[tokio::test]
async fn test_mcp_tool_array_without_items_gets_default_string_items() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    features.enable(Feature::ApplyPatchFreeform);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/tags".to_string(),
            mcp_tool_info_with_display_name(
                "dash/tags",
                mcp_tool(
                    "tags",
                    "Tags",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"tags": {"type": "array"}}
                    }),
                ),
            ),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    let tool = find_namespace_function_tool(&tools, "dash/", "tags");
    assert_eq!(
        *tool,
        ResponsesApiTool {
            name: "tags".to_string(),
            parameters: JsonSchema::object(
                /*properties*/
                BTreeMap::from([(
                    "tags".to_string(),
                    JsonSchema::array(
                        JsonSchema::string(/*description*/ None),
                        /*description*/ None,
                    ),
                )]),
                /*required*/ None,
                /*additional_properties*/ None
            ),
            description: "Tags".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        }
    );
}

#[tokio::test]
async fn test_mcp_tool_anyof_defaults_to_string() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });

    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "dash/value".to_string(),
            mcp_tool_info_with_display_name(
                "dash/value",
                mcp_tool(
                    "value",
                    "AnyOf Value",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "value": {"anyOf": [{"type": "string"}, {"type": "number"}]}
                        }
                    }),
                ),
            ),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    let tool = find_namespace_function_tool(&tools, "dash/", "value");
    assert_eq!(
        *tool,
        ResponsesApiTool {
            name: "value".to_string(),
            parameters: JsonSchema::object(
                /*properties*/
                BTreeMap::from([(
                    "value".to_string(),
                    JsonSchema::any_of(
                        vec![
                            JsonSchema::string(/*description*/ None),
                            JsonSchema::number(/*description*/ None),
                        ],
                        /*description*/ None,
                    ),
                )]),
                /*required*/ None,
                /*additional_properties*/ None
            ),
            description: "AnyOf Value".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        }
    );
}

#[tokio::test]
async fn test_get_openai_tools_mcp_tools_with_additional_properties_schema() {
    let config = test_config().await;
    let model_info = construct_model_info_offline("gpt-5.4", &config);
    let mut features = Features::with_defaults();
    features.enable(Feature::UnifiedExec);
    let available_models = Vec::new();
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &model_info,
        available_models: &available_models,
        features: &features,
        image_generation_tool_auth_allowed: true,
        web_search_mode: Some(WebSearchMode::Cached),
        session_source: SessionSource::Cli,
        permission_profile: &PermissionProfile::Disabled,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
    });
    let (tools, _) = build_specs(
        &tools_config,
        Some(HashMap::from([(
            "test_server/do_something_cool".to_string(),
            mcp_tool_info_with_display_name(
                "test_server/do_something_cool",
                mcp_tool(
                    "do_something_cool",
                    "Do something cool",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                        "string_argument": {"type": "string"},
                        "number_argument": {"type": "number"},
                        "object_argument": {
                            "type": "object",
                            "properties": {
                                "string_property": {"type": "string"},
                                "number_property": {"type": "number"}
                            },
                            "required": ["string_property", "number_property"],
                            "additionalProperties": {
                                "type": "object",
                                "properties": {
                                    "addtl_prop": {"type": "string"}
                                },
                                "required": ["addtl_prop"],
                                "additionalProperties": false
                                }
                            }
                        }
                    }),
                ),
            ),
        )])),
        /*deferred_mcp_tools*/ None,
        &[],
    )
    .build();

    let tool = find_namespace_function_tool(&tools, "test_server/", "do_something_cool");
    assert_eq!(
        *tool,
        ResponsesApiTool {
            name: "do_something_cool".to_string(),
            parameters: JsonSchema::object(
                /*properties*/
                BTreeMap::from([
                    (
                        "string_argument".to_string(),
                        JsonSchema::string(/*description*/ None),
                    ),
                    (
                        "number_argument".to_string(),
                        JsonSchema::number(/*description*/ None),
                    ),
                    (
                        "object_argument".to_string(),
                        JsonSchema::object(
                            BTreeMap::from([
                                (
                                    "string_property".to_string(),
                                    JsonSchema::string(/*description*/ None),
                                ),
                                (
                                    "number_property".to_string(),
                                    JsonSchema::number(/*description*/ None),
                                ),
                            ]),
                            Some(vec![
                                "string_property".to_string(),
                                "number_property".to_string(),
                            ]),
                            Some(
                                JsonSchema::object(
                                    BTreeMap::from([(
                                        "addtl_prop".to_string(),
                                        JsonSchema::string(/*description*/ None),
                                    )]),
                                    Some(vec!["addtl_prop".to_string()]),
                                    Some(false.into()),
                                )
                                .into(),
                            ),
                        ),
                    ),
                ]),
                /*required*/ None,
                /*additional_properties*/ None
            ),
            description: "Do something cool".to_string(),
            strict: false,
            output_schema: Some(mcp_call_tool_result_output_schema(serde_json::json!({}))),
            defer_loading: None,
        }
    );
}

#[tokio::test]
async fn code_mode_only_restricts_model_tools_to_exec_tools() {
    let mut features = Features::with_defaults();
    features.enable(Feature::CodeMode);
    features.enable(Feature::CodeModeOnly);

    assert_model_tools(
        "gpt-5.4",
        &features,
        Some(WebSearchMode::Live),
        &["exec", "wait"],
    )
    .await;
}
