//! Docker sandbox — ephemeral container per `exec()` call via bollard.
//!
//! Lifecycle: create → start → attach (collect stdout/stderr) → wait → remove.
//! The container is bound to the workdir at `/work` and the command runs there.

use crate::spec::{NetPolicy, Sandbox, SandboxError, SandboxKind, SandboxOutput, SandboxSpec};
use async_trait::async_trait;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountType};
use bollard::query_parameters::{
    CreateContainerOptions, KillContainerOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, WaitContainerOptions,
};
use futures_util::StreamExt;
use std::collections::HashMap;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct DockerSandbox {
    docker: Docker,
    image: String,
    memory_bytes: Option<i64>,
    cpus: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DockerConfig {
    pub image: String,
    pub memory: Option<String>, // "2g", "512m"
    pub cpus: Option<f64>,      // 1.5 = 150% of one CPU
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "alpine:3.20".to_string(),
            memory: Some("1g".to_string()),
            cpus: Some(1.0),
        }
    }
}

impl DockerSandbox {
    /// Connect to the local Docker daemon. Fails fast if the socket / named pipe
    /// is unreachable so the caller can degrade gracefully (e.g. fall back to native).
    pub async fn connect(cfg: DockerConfig) -> Result<Self, SandboxError> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| SandboxError::DockerUnavailable(e.to_string()))?;
        // Verify connectivity now, not at first exec.
        docker
            .ping()
            .await
            .map_err(|e| SandboxError::DockerUnavailable(format!("ping: {e}")))?;
        info!(image = %cfg.image, "docker sandbox connected");
        Ok(Self {
            docker,
            image: cfg.image,
            memory_bytes: cfg.memory.as_deref().and_then(parse_memory),
            cpus: cfg.cpus,
        })
    }

    /// Pull the configured image if it's not already present locally.
    pub async fn ensure_image(&self) -> Result<(), SandboxError> {
        use bollard::query_parameters::CreateImageOptions;
        if self.docker.inspect_image(&self.image).await.is_ok() {
            return Ok(());
        }
        info!(image = %self.image, "pulling image");
        let opts = CreateImageOptions {
            from_image: Some(self.image.clone()),
            ..Default::default()
        };
        let mut stream = self.docker.create_image(Some(opts), None, None);
        while let Some(item) = stream.next().await {
            match item {
                Ok(_) => {}
                Err(e) => return Err(SandboxError::ImagePull(e.to_string())),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    fn kind(&self) -> SandboxKind {
        SandboxKind::Docker
    }

    async fn exec(&self, spec: SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        let name = format!("jarvis-{}", Uuid::new_v4().simple());
        let net_mode = match spec.net {
            NetPolicy::None => Some("none".to_string()),
            NetPolicy::EgressOnly | NetPolicy::Full => Some("bridge".to_string()),
        };

        // Translate the host workdir path to a string for Docker.
        let workdir_str = path_to_docker(&spec.workdir)?;

        let mounts = vec![Mount {
            target: Some("/work".to_string()),
            source: Some(workdir_str.clone()),
            typ: Some(MountType::BIND),
            read_only: Some(false),
            ..Default::default()
        }];

        let host_cfg = HostConfig {
            mounts: Some(mounts),
            network_mode: net_mode,
            memory: self.memory_bytes,
            nano_cpus: self.cpus.map(|c| (c * 1_000_000_000.0) as i64),
            auto_remove: Some(false), // we remove explicitly to avoid race with logs collection
            ..Default::default()
        };

        let env_strs: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        // Encode the user command via `sh -c "<cmd>"`. Most images ship sh.
        let cmd_vec = vec!["sh".to_string(), "-c".to_string(), spec.cmd.clone()];

        let mut labels = HashMap::new();
        labels.insert("jarvis.task".to_string(), "1".to_string());

        let create_cfg = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(cmd_vec),
            working_dir: Some("/work".to_string()),
            env: Some(env_strs),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(false),
            host_config: Some(host_cfg),
            labels: Some(labels),
            ..Default::default()
        };

        debug!(name = %name, image = %self.image, "docker: create");
        let create_resp = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(name.clone()),
                    ..Default::default()
                }),
                create_cfg,
            )
            .await?;

        // Always try to clean up even on error.
        let result = run_to_completion(&self.docker, &create_resp.id, &spec).await;

        let _ = self
            .docker
            .remove_container(
                &create_resp.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        result
    }
}

async fn run_to_completion(
    docker: &Docker,
    container_id: &str,
    spec: &SandboxSpec,
) -> Result<SandboxOutput, SandboxError> {
    docker
        .start_container(container_id, None::<StartContainerOptions>)
        .await?;

    // Stream logs in parallel with the wait future.
    let logs_opts = LogsOptions {
        stdout: true,
        stderr: true,
        follow: true,
        ..Default::default()
    };
    let mut logs = docker.logs(container_id, Some(logs_opts));
    let mut stdout = String::new();
    let mut stderr = String::new();

    let mut wait_stream = docker.wait_container(
        container_id,
        Some(WaitContainerOptions {
            condition: "not-running".to_string(),
        }),
    );

    let timer = tokio::time::sleep(spec.timeout);
    tokio::pin!(timer);
    let mut timed_out = false;
    let mut exit_code: i64 = -1;

    loop {
        tokio::select! {
            biased;
            _ = &mut timer => {
                warn!("docker: timeout — killing container");
                let _ = docker.kill_container(container_id, None::<KillContainerOptions>).await;
                timed_out = true;
                break;
            }
            log = logs.next() => match log {
                Some(Ok(LogOutput::StdOut { message })) => stdout.push_str(&String::from_utf8_lossy(&message)),
                Some(Ok(LogOutput::StdErr { message })) => stderr.push_str(&String::from_utf8_lossy(&message)),
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    warn!(error = %e, "docker: log stream error");
                    break;
                }
                None => {
                    // logs stream ended, container is done
                    break;
                }
            },
            wait = wait_stream.next() => match wait {
                Some(Ok(resp)) => {
                    exit_code = resp.status_code;
                    // wait returned but logs may still have buffered data; drain quickly.
                    while let Ok(Some(log)) = tokio::time::timeout(std::time::Duration::from_millis(200), logs.next()).await {
                        match log {
                            Ok(LogOutput::StdOut { message }) => stdout.push_str(&String::from_utf8_lossy(&message)),
                            Ok(LogOutput::StdErr { message }) => stderr.push_str(&String::from_utf8_lossy(&message)),
                            _ => {}
                        }
                    }
                    break;
                }
                Some(Err(e)) => return Err(e.into()),
                None => break,
            }
        }
    }

    Ok(SandboxOutput {
        exit_code: exit_code as i32,
        stdout,
        stderr,
        timed_out,
        backend: "docker".to_string(),
    })
}

fn path_to_docker(p: &std::path::Path) -> Result<String, SandboxError> {
    // Docker on Windows accepts forward slashes and drive letters. On Linux this is already fine.
    #[cfg(windows)]
    {
        let s = p
            .canonicalize()
            .or_else(|_| Ok::<_, std::io::Error>(p.to_path_buf()))?
            .display()
            .to_string();
        // Strip any \\?\ verbatim prefix that windows canonicalize adds.
        let s = s.trim_start_matches(r"\\?\").to_string();
        Ok(s.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        Ok(p.display().to_string())
    }
}

fn parse_memory(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, mul) = if let Some(n) = s.strip_suffix('g').or_else(|| s.strip_suffix('G')) {
        (n, 1_073_741_824i64)
    } else if let Some(n) = s.strip_suffix('m').or_else(|| s.strip_suffix('M')) {
        (n, 1_048_576i64)
    } else if let Some(n) = s.strip_suffix('k').or_else(|| s.strip_suffix('K')) {
        (n, 1024)
    } else {
        (s, 1)
    };
    num.trim().parse::<i64>().ok().map(|v| v * mul)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_units() {
        assert_eq!(parse_memory("2g"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_memory("512m"), Some(512 * 1024 * 1024));
        assert_eq!(parse_memory("1024"), Some(1024));
        assert_eq!(parse_memory("bad"), None);
    }
}
