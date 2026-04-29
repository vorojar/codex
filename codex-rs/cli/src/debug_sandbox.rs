#[cfg(target_os = "macos")]
mod pid_tracker;
mod replay;
#[cfg(target_os = "macos")]
mod seatbelt;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use codex_config::LoaderOverrides;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::config::NetworkProxyAuditMetadata;
use codex_core::config::NetworkProxySpec;
use codex_core::exec_env::create_env;
#[cfg(target_os = "macos")]
use codex_core::spawn::CODEX_SANDBOX_ENV_VAR;
use codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_sandboxing::landlock::allow_network_for_proxy;
use codex_sandboxing::landlock::create_linux_sandbox_command_args_for_permission_profile;
#[cfg(target_os = "macos")]
use codex_sandboxing::seatbelt::CreateSeatbeltCommandArgsParams;
#[cfg(target_os = "macos")]
use codex_sandboxing::seatbelt::create_seatbelt_command_args;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use tokio::process::Child;
use tokio::process::Command as TokioCommand;
use toml::Value as TomlValue;

use crate::LandlockCommand;
use crate::SeatbeltCommand;
use crate::WindowsCommand;
use crate::exit_status::handle_exit_status;
use replay::SandboxReplayPayload;
use replay::parse_sandbox_replay_payload;

#[cfg(target_os = "macos")]
use seatbelt::DenialLogger;

#[cfg(target_os = "macos")]
pub async fn run_command_under_seatbelt(
    command: SeatbeltCommand,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let SeatbeltCommand {
        full_auto,
        permissions_profile,
        cwd,
        include_managed_config,
        permissions_json,
        permissions_json_file,
        allow_unix_sockets,
        log_denials,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        DebugSandboxConfigSource::from_flags(
            DebugSandboxConfigOptions {
                full_auto,
                permissions_profile,
                cwd,
                include_managed_config,
            },
            permissions_json,
            permissions_json_file,
            config_overrides,
        )?,
        command,
        codex_linux_sandbox_exe,
        SandboxType::Seatbelt {
            allow_unix_sockets,
            log_denials,
        },
    )
    .await
}

#[cfg(not(target_os = "macos"))]
pub async fn run_command_under_seatbelt(
    _command: SeatbeltCommand,
    _codex_linux_sandbox_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    anyhow::bail!("Seatbelt sandbox is only available on macOS");
}

pub async fn run_command_under_landlock(
    command: LandlockCommand,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let LandlockCommand {
        full_auto,
        permissions_profile,
        cwd,
        include_managed_config,
        permissions_json,
        permissions_json_file,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        DebugSandboxConfigSource::from_flags(
            DebugSandboxConfigOptions {
                full_auto,
                permissions_profile,
                cwd,
                include_managed_config,
            },
            permissions_json,
            permissions_json_file,
            config_overrides,
        )?,
        command,
        codex_linux_sandbox_exe,
        SandboxType::Landlock,
    )
    .await
}

pub async fn run_command_under_windows(
    command: WindowsCommand,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> anyhow::Result<()> {
    let WindowsCommand {
        full_auto,
        permissions_profile,
        cwd,
        include_managed_config,
        permissions_json,
        permissions_json_file,
        config_overrides,
        command,
    } = command;
    run_command_under_sandbox(
        DebugSandboxConfigSource::from_flags(
            DebugSandboxConfigOptions {
                full_auto,
                permissions_profile,
                cwd,
                include_managed_config,
            },
            permissions_json,
            permissions_json_file,
            config_overrides,
        )?,
        command,
        codex_linux_sandbox_exe,
        SandboxType::Windows,
    )
    .await
}

enum SandboxType {
    #[cfg(target_os = "macos")]
    Seatbelt {
        allow_unix_sockets: Vec<AbsolutePathBuf>,
        log_denials: bool,
    },
    Landlock,
    Windows,
}

#[derive(Debug)]
struct DebugSandboxConfigOptions {
    full_auto: bool,
    permissions_profile: Option<String>,
    cwd: Option<PathBuf>,
    include_managed_config: bool,
}

#[derive(Debug)]
enum DebugSandboxConfigSource {
    Config {
        options: DebugSandboxConfigOptions,
        overrides: CliConfigOverrides,
    },
    Replay(Box<SandboxReplayPayload>),
}

impl DebugSandboxConfigSource {
    fn from_flags(
        options: DebugSandboxConfigOptions,
        permissions_json: Option<String>,
        permissions_json_file: Option<PathBuf>,
        overrides: CliConfigOverrides,
    ) -> anyhow::Result<Self> {
        let replay = match (permissions_json, permissions_json_file) {
            (Some(json), None) => Some(parse_sandbox_replay_payload(&json)?),
            (None, Some(path)) => {
                let json = std::fs::read_to_string(&path).with_context(|| {
                    format!("failed to read sandbox replay file {}", path.display())
                })?;
                Some(parse_sandbox_replay_payload(&json)?)
            }
            (None, None) => None,
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "--permissions-json and --permissions-json-file are mutually exclusive"
                )
            }
        };

        if let Some(replay) = replay {
            if !overrides.raw_overrides.is_empty() {
                anyhow::bail!("sandbox replay JSON cannot be combined with -c/--config overrides");
            }
            return Ok(Self::Replay(Box::new(replay)));
        }

        Ok(Self::Config { options, overrides })
    }
}

#[derive(Debug, Clone, Copy)]
enum ManagedRequirementsMode {
    Include,
    Ignore,
}

struct SandboxRuntimeConfig {
    permission_profile: PermissionProfile,
    network: Option<NetworkProxySpec>,
    managed_network_requirements_enabled: bool,
    cwd: AbsolutePathBuf,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    codex_home: AbsolutePathBuf,
    env: HashMap<String, String>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    use_legacy_landlock: bool,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    windows_sandbox_level: WindowsSandboxLevel,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    windows_sandbox_private_desktop: bool,
}

impl SandboxRuntimeConfig {
    fn from_config(config: &Config) -> Self {
        Self {
            permission_profile: config.permissions.permission_profile(),
            network: config.permissions.network.clone(),
            managed_network_requirements_enabled: config.managed_network_requirements_enabled(),
            cwd: config.cwd.clone(),
            codex_home: config.codex_home.clone(),
            env: create_env(
                &config.permissions.shell_environment_policy,
                /*thread_id*/ None,
            ),
            codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
            use_legacy_landlock: config.features.use_legacy_landlock(),
            windows_sandbox_level: codex_core::windows_sandbox::windows_sandbox_level_from_config(
                config,
            ),
            windows_sandbox_private_desktop: config.permissions.windows_sandbox_private_desktop,
        }
    }

    fn from_replay(payload: SandboxReplayPayload) -> anyhow::Result<Self> {
        let permission_profile = payload.permission_profile;
        let network = payload
            .network_proxy
            .map(|network| {
                NetworkProxySpec::from_config_and_constraints(
                    network.config,
                    network.requirements,
                    &permission_profile,
                )
            })
            .transpose()?;

        Ok(Self {
            permission_profile,
            network,
            managed_network_requirements_enabled: payload.managed_network_requirements_enabled,
            cwd: payload.sandbox_cwd,
            codex_home: payload.codex_home,
            env: payload.env,
            codex_linux_sandbox_exe: payload.codex_linux_sandbox_exe,
            use_legacy_landlock: payload.use_legacy_landlock,
            windows_sandbox_level: payload.windows_sandbox_level,
            windows_sandbox_private_desktop: payload.windows_sandbox_private_desktop,
        })
    }

    fn file_system_sandbox_policy(&self) -> codex_protocol::permissions::FileSystemSandboxPolicy {
        self.permission_profile.file_system_sandbox_policy()
    }

    fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        self.permission_profile.network_sandbox_policy()
    }

    fn legacy_sandbox_policy(
        &self,
        sandbox_policy_cwd: &AbsolutePathBuf,
    ) -> anyhow::Result<codex_protocol::protocol::SandboxPolicy> {
        self.permission_profile
            .to_legacy_sandbox_policy(sandbox_policy_cwd.as_path())
            .map_err(Into::into)
    }
}

async fn load_sandbox_runtime_config(
    source: DebugSandboxConfigSource,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> anyhow::Result<SandboxRuntimeConfig> {
    match source {
        DebugSandboxConfigSource::Config { options, overrides } => {
            let config = load_debug_sandbox_config(
                overrides.parse_overrides().map_err(anyhow::Error::msg)?,
                codex_linux_sandbox_exe,
                options,
            )
            .await?;
            Ok(SandboxRuntimeConfig::from_config(&config))
        }
        DebugSandboxConfigSource::Replay(payload) => SandboxRuntimeConfig::from_replay(*payload),
    }
}

async fn run_command_under_sandbox(
    config_source: DebugSandboxConfigSource,
    command: Vec<String>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    sandbox_type: SandboxType,
) -> anyhow::Result<()> {
    let runtime_config =
        load_sandbox_runtime_config(config_source, codex_linux_sandbox_exe).await?;
    let cwd = runtime_config.cwd.clone();
    // For now, we always use the same cwd for both the command and the
    // sandbox policy. In the future, we could add a CLI option to set them
    // separately.
    let sandbox_policy_cwd = cwd.clone();

    let env = runtime_config.env.clone();

    // Special-case Windows sandbox: execute and exit the process to emulate inherited stdio.
    if let SandboxType::Windows = sandbox_type {
        #[cfg(target_os = "windows")]
        {
            run_command_under_windows_session(
                &runtime_config,
                command,
                cwd,
                sandbox_policy_cwd,
                env,
            )
            .await;
        }
        #[cfg(not(target_os = "windows"))]
        {
            anyhow::bail!("Windows sandbox is only available on Windows");
        }
    }

    #[cfg(target_os = "macos")]
    let mut denial_logger = match &sandbox_type {
        SandboxType::Seatbelt { log_denials, .. } => log_denials.then(DenialLogger::new).flatten(),
        SandboxType::Landlock | SandboxType::Windows => None,
    };

    let managed_network_requirements_enabled = runtime_config.managed_network_requirements_enabled;

    // This proxy should only live for the lifetime of the child process.
    let network_proxy = match runtime_config.network.as_ref() {
        Some(spec) => Some(
            spec.start_proxy(
                &runtime_config.permission_profile,
                /*policy_decider*/ None,
                /*blocked_request_observer*/ None,
                managed_network_requirements_enabled,
                NetworkProxyAuditMetadata::default(),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?,
        ),
        None => None,
    };
    let network = network_proxy
        .as_ref()
        .map(codex_core::config::StartedNetworkProxy::proxy);

    let mut child = match sandbox_type {
        #[cfg(target_os = "macos")]
        SandboxType::Seatbelt {
            allow_unix_sockets, ..
        } => {
            let file_system_sandbox_policy = runtime_config.file_system_sandbox_policy();
            let network_sandbox_policy = runtime_config.network_sandbox_policy();
            let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
                command,
                file_system_sandbox_policy: &file_system_sandbox_policy,
                network_sandbox_policy,
                sandbox_policy_cwd: sandbox_policy_cwd.as_path(),
                enforce_managed_network: false,
                network: network.as_ref(),
                extra_allow_unix_sockets: &allow_unix_sockets,
            });
            spawn_debug_sandbox_child(
                PathBuf::from("/usr/bin/sandbox-exec"),
                args,
                /*arg0*/ None,
                cwd.to_path_buf(),
                network_sandbox_policy,
                env,
                |env_map| {
                    env_map.insert(CODEX_SANDBOX_ENV_VAR.to_string(), "seatbelt".to_string());
                    if let Some(network) = network.as_ref() {
                        network.apply_to_env(env_map);
                    }
                },
            )
            .await?
        }
        SandboxType::Landlock => {
            let codex_linux_sandbox_exe = runtime_config
                .codex_linux_sandbox_exe
                .clone()
                .context("codex-linux-sandbox executable not found")?;
            let use_legacy_landlock = runtime_config.use_legacy_landlock;
            let network_sandbox_policy = runtime_config.network_sandbox_policy();
            let args = create_linux_sandbox_command_args_for_permission_profile(
                command,
                cwd.as_path(),
                &runtime_config.permission_profile,
                sandbox_policy_cwd.as_path(),
                use_legacy_landlock,
                allow_network_for_proxy(managed_network_requirements_enabled),
            );
            spawn_debug_sandbox_child(
                codex_linux_sandbox_exe,
                args,
                Some("codex-linux-sandbox"),
                cwd.to_path_buf(),
                network_sandbox_policy,
                env,
                |env_map| {
                    if let Some(network) = network.as_ref() {
                        network.apply_to_env(env_map);
                    }
                },
            )
            .await?
        }
        SandboxType::Windows => {
            unreachable!("Windows sandbox should have been handled above");
        }
    };

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = &mut denial_logger {
        denial_logger.on_child_spawn(&child);
    }

    let status = child.wait().await?;

    #[cfg(target_os = "macos")]
    if let Some(denial_logger) = denial_logger {
        let denials = denial_logger.finish().await;
        eprintln!("\n=== Sandbox denials ===");
        if denials.is_empty() {
            eprintln!("None found.");
        } else {
            for seatbelt::SandboxDenial { name, capability } in denials {
                eprintln!("({name}) {capability}");
            }
        }
    }

    handle_exit_status(status);
}

#[cfg(target_os = "windows")]
async fn run_command_under_windows_session(
    runtime_config: &SandboxRuntimeConfig,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    sandbox_policy_cwd: AbsolutePathBuf,
    env: std::collections::HashMap<String, String>,
) -> ! {
    use codex_windows_sandbox::spawn_windows_sandbox_session_elevated;
    use codex_windows_sandbox::spawn_windows_sandbox_session_legacy;

    let sandbox_policy = match runtime_config.legacy_sandbox_policy(&sandbox_policy_cwd) {
        Ok(sandbox_policy) => sandbox_policy,
        Err(err) => {
            eprintln!("windows sandbox failed to project policy: {err}");
            std::process::exit(1);
        }
    };
    let policy_str = match serde_json::to_string(&sandbox_policy) {
        Ok(policy_str) => policy_str,
        Err(err) => {
            eprintln!("windows sandbox failed to serialize policy: {err}");
            std::process::exit(1);
        }
    };

    let use_elevated = matches!(
        runtime_config.windows_sandbox_level,
        WindowsSandboxLevel::Elevated
    );

    let spawned = if use_elevated {
        spawn_windows_sandbox_session_elevated(
            policy_str.as_str(),
            sandbox_policy_cwd.as_path(),
            runtime_config.codex_home.as_path(),
            command,
            cwd.as_path(),
            env,
            None,
            /*tty*/ false,
            /*stdin_open*/ true,
            runtime_config.windows_sandbox_private_desktop,
        )
        .await
    } else {
        spawn_windows_sandbox_session_legacy(
            policy_str.as_str(),
            sandbox_policy_cwd.as_path(),
            runtime_config.codex_home.as_path(),
            command,
            cwd.as_path(),
            env,
            None,
            /*tty*/ false,
            /*stdin_open*/ true,
            runtime_config.windows_sandbox_private_desktop,
        )
        .await
    };

    let spawned = match spawned {
        Ok(spawned) => spawned,
        Err(err) => {
            eprintln!("windows sandbox failed: {err}");
            std::process::exit(1);
        }
    };

    let session = std::sync::Arc::new(spawned.session);
    let tokio_runtime = tokio::runtime::Handle::current();
    // Give large or slow tail output a better chance to finish draining
    // without letting rare EOF issues hang the wrapper indefinitely.
    let output_drain_timeout = std::time::Duration::from_secs(5);
    // A helper thread watches our stdin. When the input source closes it,
    // the thread tells the main async code so we can also close stdin for
    // the sandboxed child process.
    let (stdin_eof_tx, stdin_eof_rx) = tokio::sync::oneshot::channel();

    // Start background threads that copy stdin/stdout/stderr. We
    // intentionally do not keep their JoinHandles; dropping the handle does
    // not stop the thread, it just means we are not going to wait on it
    // later.
    drop(windows_stdio_bridge::spawn_input_forwarder(
        std::io::stdin(),
        session.writer_sender(),
        stdin_eof_tx,
    ));
    let (stdout_forwarder, stdout_forwarder_done_rx) = windows_stdio_bridge::spawn_output_forwarder(
        tokio_runtime.clone(),
        spawned.stdout_rx,
        std::io::stdout(),
    );
    drop(stdout_forwarder);
    let (stderr_forwarder, stderr_forwarder_done_rx) = windows_stdio_bridge::spawn_output_forwarder(
        tokio_runtime.clone(),
        spawned.stderr_rx,
        std::io::stderr(),
    );
    drop(stderr_forwarder);

    let stdin_close_task = tokio::spawn({
        let session = std::sync::Arc::clone(&session);
        async move {
            let _ = stdin_eof_rx.await;
            session.close_stdin();
        }
    });

    let mut exit_rx = spawned.exit_rx;
    let exit_code = tokio::select! {
        res = &mut exit_rx => res.unwrap_or(-1),
        res = tokio::signal::ctrl_c() => {
            if let Ok(()) = res {
                session.request_terminate();
            }
            exit_rx.await.unwrap_or(-1)
        }
    };

    stdin_close_task.abort();
    let _ = tokio::time::timeout(output_drain_timeout, async {
        let _ = stdout_forwarder_done_rx.await;
        let _ = stderr_forwarder_done_rx.await;
    })
    .await;
    std::process::exit(exit_code);
}

pub fn create_sandbox_mode(full_auto: bool) -> SandboxMode {
    if full_auto {
        SandboxMode::WorkspaceWrite
    } else {
        SandboxMode::ReadOnly
    }
}

async fn spawn_debug_sandbox_child(
    program: PathBuf,
    args: Vec<String>,
    arg0: Option<&str>,
    cwd: PathBuf,
    network_sandbox_policy: NetworkSandboxPolicy,
    mut env: std::collections::HashMap<String, String>,
    apply_env: impl FnOnce(&mut std::collections::HashMap<String, String>),
) -> std::io::Result<Child> {
    let mut cmd = TokioCommand::new(&program);
    #[cfg(unix)]
    cmd.arg0(arg0.map_or_else(|| program.to_string_lossy().to_string(), String::from));
    #[cfg(not(unix))]
    let _ = arg0;
    cmd.args(args);
    cmd.current_dir(cwd);
    apply_env(&mut env);
    cmd.env_clear();
    cmd.envs(env);

    if !network_sandbox_policy.is_enabled() {
        cmd.env(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR, "1");
    }

    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
}

#[cfg(target_os = "windows")]
mod windows_stdio_bridge {
    use std::io::Read;
    use std::io::Write;

    use tokio::sync::mpsc;
    use tokio::sync::oneshot;

    const STDIN_FORWARD_CHUNK_SIZE: usize = 8 * 1024;

    pub(super) fn spawn_input_forwarder<R>(
        mut input: R,
        writer_tx: mpsc::Sender<Vec<u8>>,
        stdin_eof_tx: oneshot::Sender<()>,
    ) -> std::thread::JoinHandle<()>
    where
        R: Read + Send + 'static,
    {
        std::thread::spawn(move || {
            let mut buffer = [0_u8; STDIN_FORWARD_CHUNK_SIZE];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => {
                        eprintln!("windows sandbox stdin forwarder failed: {err}");
                        break;
                    }
                }
            }
            let _ = stdin_eof_tx.send(());
        })
    }

    pub(super) fn spawn_output_forwarder<W>(
        tokio_runtime: tokio::runtime::Handle,
        output_rx: mpsc::Receiver<Vec<u8>>,
        mut writer: W,
    ) -> (std::thread::JoinHandle<()>, oneshot::Receiver<()>)
    where
        W: Write + Send + 'static,
    {
        let (done_tx, done_rx) = oneshot::channel();
        // The sandbox session emits output on Tokio channels, but writing to the
        // caller's stdio is simplest from a dedicated blocking thread.
        let handle = std::thread::spawn(move || {
            let mut output_rx = output_rx;
            while let Some(chunk) = tokio_runtime.block_on(output_rx.recv()) {
                if let Err(err) = writer.write_all(&chunk) {
                    eprintln!("windows sandbox output forwarder failed to write: {err}");
                    break;
                }
                if let Err(err) = writer.flush() {
                    eprintln!("windows sandbox output forwarder failed to flush: {err}");
                    break;
                }
            }
            let _ = done_tx.send(());
        });
        (handle, done_rx)
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Mutex;

        use pretty_assertions::assert_eq;

        use super::*;

        #[tokio::test]
        async fn input_forwarder_sends_chunks_and_reports_eof() -> anyhow::Result<()> {
            let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
            let (stdin_closed_tx, stdin_closed_rx) = tokio::sync::oneshot::channel();
            let input = std::io::Cursor::new(b"first\nsecond\n".to_vec());

            let forwarder = spawn_input_forwarder(input, writer_tx, stdin_closed_tx);
            let mut received = Vec::new();
            while let Some(chunk) = writer_rx.recv().await {
                received.extend_from_slice(&chunk);
            }
            stdin_closed_rx.await?;
            forwarder.join().expect("stdin forwarder should finish");

            assert_eq!(received, b"first\nsecond\n".to_vec());
            Ok(())
        }

        #[tokio::test]
        async fn output_forwarder_writes_all_chunks() -> anyhow::Result<()> {
            #[derive(Clone, Default)]
            struct SharedWriter(std::sync::Arc<Mutex<Vec<u8>>>);

            impl std::io::Write for SharedWriter {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    let mut guard = self
                        .0
                        .lock()
                        .map_err(|_| std::io::Error::other("writer poisoned"))?;
                    guard.extend_from_slice(buf);
                    Ok(buf.len())
                }

                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }

            let runtime = tokio::runtime::Handle::current();
            let (output_tx, output_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
            let writer = SharedWriter::default();
            let sink = std::sync::Arc::clone(&writer.0);

            let (forwarder, done_rx) = spawn_output_forwarder(runtime, output_rx, writer);
            output_tx.send(b"alpha".to_vec()).await?;
            output_tx.send(b"beta".to_vec()).await?;
            drop(output_tx);
            forwarder.join().expect("output forwarder should finish");
            done_rx.await?;

            let output = sink
                .lock()
                .map_err(|_| anyhow::anyhow!("writer poisoned"))?
                .clone();
            assert_eq!(output, b"alphabeta".to_vec());
            Ok(())
        }
    }
}

async fn load_debug_sandbox_config(
    cli_overrides: Vec<(String, TomlValue)>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    options: DebugSandboxConfigOptions,
) -> anyhow::Result<Config> {
    load_debug_sandbox_config_with_codex_home(
        cli_overrides,
        codex_linux_sandbox_exe,
        options,
        /*codex_home*/ None,
    )
    .await
}

async fn load_debug_sandbox_config_with_codex_home(
    mut cli_overrides: Vec<(String, TomlValue)>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    options: DebugSandboxConfigOptions,
    codex_home: Option<PathBuf>,
) -> anyhow::Result<Config> {
    let DebugSandboxConfigOptions {
        full_auto,
        permissions_profile,
        cwd,
        include_managed_config,
    } = options;

    let managed_requirements_mode = if permissions_profile.is_some() && !include_managed_config {
        ManagedRequirementsMode::Ignore
    } else {
        ManagedRequirementsMode::Include
    };

    if let Some(permissions_profile) = permissions_profile {
        cli_overrides.push((
            "default_permissions".to_string(),
            TomlValue::String(permissions_profile),
        ));
    }

    let config = build_debug_sandbox_config(
        cli_overrides.clone(),
        ConfigOverrides {
            cwd: cwd.clone(),
            codex_linux_sandbox_exe: codex_linux_sandbox_exe.clone(),
            ..Default::default()
        },
        codex_home.clone(),
        managed_requirements_mode,
    )
    .await?;

    if config_uses_permission_profiles(&config) {
        if full_auto {
            anyhow::bail!(
                "`codex sandbox --full-auto` is only supported for legacy `sandbox_mode` configs; choose a writable `[permissions]` profile instead"
            );
        }
        return Ok(config);
    }

    build_debug_sandbox_config(
        cli_overrides,
        ConfigOverrides {
            sandbox_mode: Some(create_sandbox_mode(full_auto)),
            cwd,
            codex_linux_sandbox_exe,
            ..Default::default()
        },
        codex_home,
        managed_requirements_mode,
    )
    .await
    .map_err(Into::into)
}

async fn build_debug_sandbox_config(
    cli_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    codex_home: Option<PathBuf>,
    managed_requirements_mode: ManagedRequirementsMode,
) -> std::io::Result<Config> {
    let mut builder = ConfigBuilder::default()
        .cli_overrides(cli_overrides)
        .harness_overrides(harness_overrides);
    if let ManagedRequirementsMode::Ignore = managed_requirements_mode {
        builder = builder.loader_overrides(LoaderOverrides {
            ignore_managed_requirements: true,
            ..Default::default()
        });
    }
    if let Some(codex_home) = codex_home {
        builder = builder
            .codex_home(codex_home.clone())
            .fallback_cwd(Some(codex_home));
    }
    builder.build().await
}

fn config_uses_permission_profiles(config: &Config) -> bool {
    config
        .config_layer_stack
        .effective_config()
        .get("default_permissions")
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_config::NetworkConstraints;
    use tempfile::TempDir;

    fn escape_toml_path(path: &std::path::Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }

    fn write_permissions_profile_config(
        codex_home: &TempDir,
        docs: &std::path::Path,
        private: &std::path::Path,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(private)?;
        let config = format!(
            "default_permissions = \"limited-read-test\"\n\
             [permissions.limited-read-test.filesystem]\n\
             \":minimal\" = \"read\"\n\
             \"{}\" = \"read\"\n\
             \"{}\" = \"none\"\n\
             \n\
             [permissions.limited-read-test.network]\n\
             enabled = true\n",
            escape_toml_path(docs),
            escape_toml_path(private),
        );
        std::fs::write(codex_home.path().join("config.toml"), config)?;
        Ok(())
    }

    fn sample_replay_payload(
        codex_home: &TempDir,
        cwd: &TempDir,
    ) -> anyhow::Result<SandboxReplayPayload> {
        Ok(SandboxReplayPayload {
            permission_profile: PermissionProfile::read_only(),
            network_proxy: None,
            managed_network_requirements_enabled: false,
            sandbox_cwd: AbsolutePathBuf::from_absolute_path(cwd.path())?,
            codex_home: AbsolutePathBuf::from_absolute_path(codex_home.path())?,
            env: HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            codex_linux_sandbox_exe: Some(PathBuf::from("/tmp/codex-linux-sandbox")),
            use_legacy_landlock: true,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: true,
        })
    }

    #[tokio::test]
    async fn debug_sandbox_honors_active_permission_profiles() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let sandbox_paths = TempDir::new()?;
        let docs = sandbox_paths.path().join("docs");
        let private = docs.join("private");
        write_permissions_profile_config(&codex_home, &docs, &private)?;
        let codex_home_path = codex_home.path().to_path_buf();

        let profile_config = build_debug_sandbox_config(
            Vec::new(),
            ConfigOverrides::default(),
            Some(codex_home_path.clone()),
            ManagedRequirementsMode::Include,
        )
        .await?;
        let legacy_config = build_debug_sandbox_config(
            Vec::new(),
            ConfigOverrides {
                sandbox_mode: Some(create_sandbox_mode(/*full_auto*/ false)),
                ..Default::default()
            },
            Some(codex_home_path.clone()),
            ManagedRequirementsMode::Include,
        )
        .await?;

        let config = load_debug_sandbox_config_with_codex_home(
            Vec::new(),
            /*codex_linux_sandbox_exe*/ None,
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: None,
                cwd: None,
                include_managed_config: false,
            },
            Some(codex_home_path),
        )
        .await?;

        assert!(config_uses_permission_profiles(&config));
        assert!(
            profile_config.permissions.file_system_sandbox_policy()
                != legacy_config.permissions.file_system_sandbox_policy(),
            "test fixture should distinguish profile syntax from legacy sandbox_mode"
        );
        assert_eq!(
            config.permissions.file_system_sandbox_policy(),
            profile_config.permissions.file_system_sandbox_policy(),
        );
        assert_ne!(
            config.permissions.file_system_sandbox_policy(),
            legacy_config.permissions.file_system_sandbox_policy(),
        );

        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_rejects_full_auto_for_permission_profiles() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let sandbox_paths = TempDir::new()?;
        let docs = sandbox_paths.path().join("docs");
        let private = docs.join("private");
        write_permissions_profile_config(&codex_home, &docs, &private)?;

        let err = load_debug_sandbox_config_with_codex_home(
            Vec::new(),
            /*codex_linux_sandbox_exe*/ None,
            DebugSandboxConfigOptions {
                full_auto: true,
                permissions_profile: None,
                cwd: None,
                include_managed_config: false,
            },
            Some(codex_home.path().to_path_buf()),
        )
        .await
        .expect_err("full-auto should be rejected for active permission profiles");

        assert!(
            err.to_string().contains("--full-auto"),
            "unexpected error: {err}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_honors_explicit_builtin_permission_profile() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;

        let config = load_debug_sandbox_config_with_codex_home(
            Vec::new(),
            /*codex_linux_sandbox_exe*/ None,
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: Some(":workspace".to_string()),
                cwd: None,
                include_managed_config: false,
            },
            Some(codex_home.path().to_path_buf()),
        )
        .await?;

        assert_eq!(
            config.permissions.file_system_sandbox_policy(),
            codex_protocol::models::PermissionProfile::workspace_write()
                .file_system_sandbox_policy()
        );

        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_honors_explicit_named_permission_profile() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let sandbox_paths = TempDir::new()?;
        let docs = sandbox_paths.path().join("docs");
        let private = docs.join("private");
        write_permissions_profile_config(&codex_home, &docs, &private)?;

        let config = load_debug_sandbox_config_with_codex_home(
            Vec::new(),
            /*codex_linux_sandbox_exe*/ None,
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: Some("limited-read-test".to_string()),
                cwd: None,
                include_managed_config: false,
            },
            Some(codex_home.path().to_path_buf()),
        )
        .await?;

        let expected = build_debug_sandbox_config(
            vec![(
                "default_permissions".to_string(),
                TomlValue::String("limited-read-test".to_string()),
            )],
            ConfigOverrides::default(),
            Some(codex_home.path().to_path_buf()),
            ManagedRequirementsMode::Include,
        )
        .await?;

        assert_eq!(
            config.permissions.file_system_sandbox_policy(),
            expected.permissions.file_system_sandbox_policy()
        );

        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_uses_explicit_profile_cwd() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;

        let config = load_debug_sandbox_config_with_codex_home(
            Vec::new(),
            /*codex_linux_sandbox_exe*/ None,
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: Some(":workspace".to_string()),
                cwd: Some(cwd.path().to_path_buf()),
                include_managed_config: false,
            },
            Some(codex_home.path().to_path_buf()),
        )
        .await?;

        assert_eq!(config.cwd.as_path(), cwd.path());

        Ok(())
    }

    #[test]
    fn sandbox_replay_payload_round_trips() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let payload = sample_replay_payload(&codex_home, &cwd)?;

        let json = serde_json::to_string(&payload)?;
        let reparsed = parse_sandbox_replay_payload(&json)?;

        assert_eq!(reparsed, payload);
        Ok(())
    }

    #[test]
    fn debug_sandbox_loads_replay_json_file() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let payload = sample_replay_payload(&codex_home, &cwd)?;
        let replay_file = codex_home.path().join("replay.json");
        std::fs::write(&replay_file, serde_json::to_vec(&payload)?)?;

        let source = DebugSandboxConfigSource::from_flags(
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: None,
                cwd: None,
                include_managed_config: false,
            },
            /*permissions_json*/ None,
            Some(replay_file),
            CliConfigOverrides::default(),
        )?;

        let DebugSandboxConfigSource::Replay(parsed) = source else {
            panic!("expected replay config source");
        };
        assert_eq!(*parsed, payload);
        Ok(())
    }

    #[test]
    fn debug_sandbox_replay_rejects_config_overrides() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let payload = sample_replay_payload(&codex_home, &cwd)?;

        let err = DebugSandboxConfigSource::from_flags(
            DebugSandboxConfigOptions {
                full_auto: false,
                permissions_profile: None,
                cwd: None,
                include_managed_config: false,
            },
            Some(serde_json::to_string(&payload)?),
            /*permissions_json_file*/ None,
            CliConfigOverrides {
                raw_overrides: vec!["model=o3".to_string()],
            },
        )
        .expect_err("config overrides should be rejected");

        assert!(
            err.to_string().contains("-c/--config"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_replay_bypasses_ambient_config() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        std::fs::write(codex_home.path().join("config.toml"), "invalid = [")?;
        let payload = sample_replay_payload(&codex_home, &cwd)?;

        let runtime = load_sandbox_runtime_config(
            DebugSandboxConfigSource::Replay(Box::new(payload.clone())),
            Some(PathBuf::from("/ignored/from/ambient/config")),
        )
        .await?;

        assert_eq!(runtime.permission_profile, payload.permission_profile);
        assert_eq!(runtime.cwd.as_path(), cwd.path());
        assert_eq!(
            runtime.codex_linux_sandbox_exe,
            payload.codex_linux_sandbox_exe
        );
        assert_eq!(runtime.env, payload.env);

        Ok(())
    }

    #[test]
    fn sandbox_replay_payload_rejects_relative_paths() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let payload = sample_replay_payload(&codex_home, &cwd)?;
        let mut json = serde_json::to_value(payload)?;
        json["sandboxCwd"] = serde_json::Value::String("relative".to_string());

        parse_sandbox_replay_payload(&serde_json::to_string(&json)?)
            .expect_err("relative sandboxCwd should be rejected");

        Ok(())
    }

    #[test]
    fn sandbox_replay_payload_preserves_managed_network_state() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let mut payload = sample_replay_payload(&codex_home, &cwd)?;
        let network_config = codex_network_proxy::NetworkProxyConfig::default();
        let network_requirements = NetworkConstraints::default();
        payload.network_proxy = Some(replay::SandboxReplayNetworkProxy {
            config: network_config.clone(),
            requirements: Some(network_requirements.clone()),
        });
        payload.managed_network_requirements_enabled = true;

        let expected_network = NetworkProxySpec::from_config_and_constraints(
            network_config,
            Some(network_requirements),
            &payload.permission_profile,
        )?;
        let runtime = SandboxRuntimeConfig::from_replay(payload)?;

        assert_eq!(runtime.network, Some(expected_network));
        assert!(runtime.managed_network_requirements_enabled);

        Ok(())
    }

    #[tokio::test]
    async fn debug_sandbox_replay_requires_linux_sandbox_executable() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let mut payload = sample_replay_payload(&codex_home, &cwd)?;
        payload.codex_linux_sandbox_exe = None;

        let err = run_command_under_sandbox(
            DebugSandboxConfigSource::Replay(Box::new(payload)),
            vec!["true".to_string()],
            /*codex_linux_sandbox_exe*/ None,
            SandboxType::Landlock,
        )
        .await
        .expect_err("missing codex-linux-sandbox should return an error");

        assert!(
            err.to_string()
                .contains("codex-linux-sandbox executable not found"),
            "unexpected error: {err}"
        );

        Ok(())
    }
}
