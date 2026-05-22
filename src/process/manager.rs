use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::RestartPolicy;
use crate::error::{LaraMuxError, Result};
use crate::event::Event;
use crate::process::types::{ProcessConfig, ProcessId};

/// Thread-safe registry of active process group IDs.
/// Accessible from sync contexts (signal handlers, panic hooks, Drop).
#[derive(Clone, Debug, Default)]
pub struct PidRegistry {
    inner: Arc<StdMutex<HashSet<u32>>>,
}

impl PidRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    pub fn insert(&self, pgid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.insert(pgid);
        }
    }

    pub fn remove(&self, pgid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.remove(&pgid);
        }
    }

    /// Synchronously kill all registered process groups and their descendants.
    /// Safe to call from panic hooks and signal handlers.
    pub fn kill_all_sync(&self) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            let pids: Vec<u32> = if let Ok(set) = self.inner.lock() {
                set.iter().copied().collect()
            } else {
                return;
            };

            for &pgid in &pids {
                let _ = kill(Pid::from_raw(-(pgid as i32)), Signal::SIGTERM);
            }

            std::thread::sleep(std::time::Duration::from_millis(200));

            for &pgid in &pids {
                let _ = kill(Pid::from_raw(-(pgid as i32)), Signal::SIGKILL);
                verify_and_cleanup(pgid);
            }
        }
    }
}

/// Maximum backoff delay for restarts (60 seconds)
const MAX_RESTART_BACKOFF_SECS: u64 = 60;

/// Track restart state for a process
#[derive(Debug, Clone, Default)]
pub struct RestartState {
    /// Consecutive failures count
    pub consecutive_failures: u32,
    /// Last restart time
    pub last_restart: Option<Instant>,
}

impl RestartState {
    /// Calculate the backoff delay based on consecutive failures (exponential: 2^failures, max 60s)
    pub fn backoff_delay(&self) -> Duration {
        let secs = 2u64
            .saturating_pow(self.consecutive_failures)
            .min(MAX_RESTART_BACKOFF_SECS);
        Duration::from_secs(secs)
    }

    /// Reset state on successful start
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.last_restart = Some(Instant::now());
    }

    /// Record a failure
    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_restart = Some(Instant::now());
    }
}

/// Manages spawning, killing, and restarting processes
pub struct ProcessManager {
    children: HashMap<ProcessId, Child>,
    configs: HashMap<ProcessId, ProcessConfig>,
    restart_states: HashMap<ProcessId, RestartState>,
    event_tx: mpsc::Sender<Event>,
    cancel_token: CancellationToken,
    pid_registry: PidRegistry,
}

impl ProcessManager {
    pub fn new(
        event_tx: mpsc::Sender<Event>,
        cancel_token: CancellationToken,
        pid_registry: PidRegistry,
    ) -> Self {
        Self {
            children: HashMap::new(),
            configs: HashMap::new(),
            restart_states: HashMap::new(),
            event_tx,
            cancel_token,
            pid_registry,
        }
    }

    #[allow(dead_code)]
    pub fn pid_registry(&self) -> &PidRegistry {
        &self.pid_registry
    }

    /// Register a process configuration
    pub fn register(&mut self, config: ProcessConfig) {
        self.configs.insert(config.id.clone(), config);
    }

    /// Spawn a process
    pub async fn spawn(&mut self, id: &ProcessId) -> Result<()> {
        let config = self
            .configs
            .get(id)
            .ok_or_else(|| LaraMuxError::ProcessNotFound(id.to_string()))?
            .clone();

        // Kill existing process if running
        self.kill(id).await?;

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .current_dir(&config.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Force color output even when not connected to a TTY
            .env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("COLORTERM", "truecolor");

        // Create a process group so we can kill the entire tree on exit
        #[cfg(unix)]
        cmd.process_group(0);

        // Apply configured environment variables
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            // Provide more helpful error messages
            let reason = if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Command '{}' not found. Make sure it is installed and in your PATH.",
                    config.command
                )
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "Permission denied when trying to execute '{}'. Check file permissions.",
                    config.command
                )
            } else {
                e.to_string()
            };
            LaraMuxError::SpawnFailed {
                name: id.to_string(),
                reason,
            }
        })?;

        // Reset restart state on successful spawn
        self.restart_states.entry(id.clone()).or_default().reset();

        let pid = child.id();

        if let Some(pid) = pid {
            self.pid_registry.insert(pid);
        }

        // Spawn stdout reader task
        if let Some(stdout) = child.stdout.take() {
            let tx = self.event_tx.clone();
            let token = self.cancel_token.clone();
            let process_id = id.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                loop {
                    tokio::select! {
                        _ = token.cancelled() => break,
                        result = lines.next_line() => {
                            match result {
                                Ok(Some(line)) => {
                                    let _ = tx.send(Event::ProcessOutput {
                                        id: process_id.clone(),
                                        line,
                                        is_stderr: false,
                                    }).await;
                                }
                                Ok(None) => break,
                                Err(_) => break,
                            }
                        }
                    }
                }
            });
        }

        // Spawn stderr reader task
        if let Some(stderr) = child.stderr.take() {
            let tx = self.event_tx.clone();
            let token = self.cancel_token.clone();
            let process_id = id.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                loop {
                    tokio::select! {
                        _ = token.cancelled() => break,
                        result = lines.next_line() => {
                            match result {
                                Ok(Some(line)) => {
                                    let _ = tx.send(Event::ProcessOutput {
                                        id: process_id.clone(),
                                        line,
                                        is_stderr: true,
                                    }).await;
                                }
                                Ok(None) => break,
                                Err(_) => break,
                            }
                        }
                    }
                }
            });
        }

        self.children.insert(id.clone(), child);

        // Send initial status via event
        let initial_msg = if config.supervised {
            format!(
                "Tailing supervisor logs for {} (PID: {:?})",
                config.supervisor_program.as_deref().unwrap_or("unknown"),
                pid
            )
        } else {
            format!("Started {} (PID: {:?})", id, pid)
        };
        let _ = self
            .event_tx
            .send(Event::ProcessOutput {
                id: id.clone(),
                line: initial_msg,
                is_stderr: false,
            })
            .await;

        Ok(())
    }

    /// Spawn all registered processes
    /// Returns a list of (process_id, error_message) for any that failed to spawn
    pub async fn spawn_all(&mut self) -> Result<Vec<(ProcessId, String)>> {
        let ids: Vec<ProcessId> = self.configs.keys().cloned().collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(e) = self.spawn(&id).await {
                errors.push((id.clone(), e.to_string()));
                // Send error message as process output so it's visible in the UI
                let _ = self
                    .event_tx
                    .send(Event::ProcessOutput {
                        id: id.clone(),
                        line: format!("ERROR: Failed to start process: {}", e),
                        is_stderr: true,
                    })
                    .await;
            }
        }
        Ok(errors)
    }

    /// Kill a process gracefully (SIGTERM, wait, then SIGKILL)
    pub async fn kill(&mut self, id: &ProcessId) -> Result<()> {
        if let Some(mut child) = self.children.remove(id) {
            let pid = child.id();

            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;

                if let Some(pid) = pid {
                    let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGTERM);
                }
            }

            #[cfg(not(unix))]
            {
                let _ = child.kill().await;
            }

            let timeout = tokio::time::timeout(tokio::time::Duration::from_secs(5), child.wait());

            match timeout.await {
                Ok(Ok(status)) => {
                    let _ = self
                        .event_tx
                        .send(Event::ProcessExited {
                            id: id.clone(),
                            exit_code: status.code(),
                        })
                        .await;
                }
                _ => {
                    #[cfg(unix)]
                    {
                        use nix::sys::signal::{kill, Signal};
                        use nix::unistd::Pid;

                        if let Some(pid) = pid {
                            let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGKILL);
                        }
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = self
                        .event_tx
                        .send(Event::ProcessExited {
                            id: id.clone(),
                            exit_code: None,
                        })
                        .await;
                }
            }

            if let Some(pid) = pid {
                #[cfg(unix)]
                verify_and_cleanup(pid);
                self.pid_registry.remove(pid);
            }
        }
        Ok(())
    }

    /// Kill all processes in parallel for fast shutdown
    pub async fn kill_all(&mut self) -> Result<()> {
        use futures::future::join_all;

        let children: Vec<(ProcessId, Child)> = self.children.drain().collect();
        if children.is_empty() {
            return Ok(());
        }

        let event_tx = self.event_tx.clone();
        let registry = self.pid_registry.clone();

        let futures: Vec<_> = children
            .into_iter()
            .map(|(id, child)| {
                let tx = event_tx.clone();
                let reg = registry.clone();
                async move {
                    kill_child(child, &id, &tx, &reg).await;
                }
            })
            .collect();

        join_all(futures).await;
        Ok(())
    }

    /// Restart a process
    pub async fn restart(&mut self, id: &ProcessId) -> Result<()> {
        self.kill(id).await?;
        // Small delay before restarting
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        self.spawn(id).await
    }

    /// Restart all processes
    pub async fn restart_all(&mut self) -> Result<()> {
        let ids: Vec<ProcessId> = self.configs.keys().cloned().collect();
        for id in ids {
            self.restart(&id).await?;
        }
        Ok(())
    }

    /// Check if a process is running
    pub fn is_running(&self, id: &ProcessId) -> bool {
        self.children.contains_key(id)
    }

    /// Get process PID
    pub fn get_pid(&self, id: &ProcessId) -> Option<u32> {
        self.children.get(id).and_then(|c| c.id())
    }

    /// Check if a process should be auto-restarted based on its exit code and restart policy
    pub fn should_restart(&self, id: &ProcessId, exit_code: Option<i32>) -> bool {
        let Some(config) = self.configs.get(id) else {
            return false;
        };

        // Supervised processes are managed by supervisor — never auto-restart
        if config.supervised {
            return false;
        }

        match config.restart_policy {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => {
                // Restart only if exit code is non-zero
                exit_code.map(|c| c != 0).unwrap_or(true)
            }
            RestartPolicy::Always => true,
        }
    }

    /// Check if a process is supervised (managed by Docker supervisor)
    pub fn is_supervised(&self, id: &ProcessId) -> bool {
        self.configs.get(id).map(|c| c.supervised).unwrap_or(false)
    }

    /// Get the restart policy for a process
    #[allow(dead_code)]
    pub fn get_restart_policy(&self, id: &ProcessId) -> RestartPolicy {
        self.configs
            .get(id)
            .map(|c| c.restart_policy)
            .unwrap_or_default()
    }

    /// Get the restart state for a process (for backoff calculation)
    #[allow(dead_code)]
    pub fn get_restart_state(&self, id: &ProcessId) -> Option<&RestartState> {
        self.restart_states.get(id)
    }

    /// Record a failure for a process (for backoff calculation)
    pub fn record_failure(&mut self, id: &ProcessId) {
        self.restart_states
            .entry(id.clone())
            .or_default()
            .record_failure();
    }

    /// Get the backoff delay for restarting a process
    pub fn get_backoff_delay(&self, id: &ProcessId) -> Duration {
        self.restart_states
            .get(id)
            .map(|s| s.backoff_delay())
            .unwrap_or(Duration::from_secs(1))
    }
}

/// Helper to kill a single child process with timeout
async fn kill_child(
    mut child: Child,
    id: &ProcessId,
    event_tx: &mpsc::Sender<Event>,
    registry: &PidRegistry,
) {
    let pid = child.id();

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        if let Some(pid) = pid {
            let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGTERM);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }

    let timeout = tokio::time::timeout(tokio::time::Duration::from_secs(3), child.wait());

    match timeout.await {
        Ok(Ok(status)) => {
            let _ = event_tx
                .send(Event::ProcessExited {
                    id: id.clone(),
                    exit_code: status.code(),
                })
                .await;
        }
        _ => {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;

                if let Some(pid) = pid {
                    let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGKILL);
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = event_tx
                .send(Event::ProcessExited {
                    id: id.clone(),
                    exit_code: None,
                })
                .await;
        }
    }

    if let Some(pid) = pid {
        #[cfg(unix)]
        verify_and_cleanup(pid);
        registry.remove(pid);
    }
}

/// Walk /proc to find all descendant PIDs of root_pid.
/// Returns children-first order for bottom-up killing.
#[cfg(target_os = "linux")]
fn find_descendant_pids(root_pid: u32) -> Vec<u32> {
    let mut descendants = Vec::new();
    let mut queue = vec![root_pid];

    while let Some(parent) = queue.pop() {
        let entries = match std::fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => break,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let Ok(pid) = name_str.parse::<u32>() else {
                continue;
            };
            let stat_path = format!("/proc/{}/stat", pid);
            let Ok(stat) = std::fs::read_to_string(&stat_path) else {
                continue;
            };
            // Format: pid (comm) state ppid ...
            let Some(after_comm) = stat.rfind(')') else {
                continue;
            };
            let rest = &stat[after_comm + 2..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if let Some(ppid_str) = fields.get(1) {
                if let Ok(ppid) = ppid_str.parse::<u32>() {
                    if ppid == parent {
                        descendants.push(pid);
                        queue.push(pid);
                    }
                }
            }
        }
    }

    descendants.reverse();
    descendants
}

/// After process group kill, find and kill any surviving descendants.
#[cfg(unix)]
fn verify_and_cleanup(root_pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    // Check if anything in the process group is still alive
    if kill(Pid::from_raw(-(root_pid as i32)), Signal::SIGCONT).is_err() {
        return;
    }

    // Process group still has survivors — walk the tree and kill individually
    #[cfg(target_os = "linux")]
    {
        let descendants = find_descendant_pids(root_pid);
        for pid in &descendants {
            let _ = kill(Pid::from_raw(*pid as i32), Signal::SIGKILL);
        }
    }

    let _ = kill(Pid::from_raw(root_pid as i32), Signal::SIGKILL);
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as StdCommandExt;
    use std::process::Command as StdCommand;

    /// Spawn a process in its own process group (matching production behavior).
    fn spawn_in_group(cmd: &str, args: &[&str]) -> std::process::Child {
        let mut c = StdCommand::new(cmd);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        c.spawn().expect("failed to spawn process")
    }

    #[test]
    fn find_descendant_pids_discovers_child_processes() {
        // Spawn: sh → sleep (a parent with one child)
        let mut parent = spawn_in_group("sh", &["-c", "sleep 300"]);
        let parent_pid = parent.id();

        // Give the shell time to fork sleep
        std::thread::sleep(std::time::Duration::from_millis(200));

        let descendants = find_descendant_pids(parent_pid);
        assert!(!descendants.is_empty(), "should find at least 1 descendant (sleep)");

        // Every descendant should be a real PID
        for pid in &descendants {
            assert!(
                std::path::Path::new(&format!("/proc/{}", pid)).exists(),
                "descendant PID {} should exist in /proc",
                pid
            );
        }

        // Cleanup
        let _ = parent.kill();
        let _ = parent.wait();
    }

    #[test]
    fn find_descendant_pids_discovers_nested_tree() {
        // Spawn: sh → sh → sleep (3 levels deep)
        let mut root = spawn_in_group("sh", &["-c", "sh -c 'sleep 300' & wait"]);

        let root_pid = root.id();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let descendants = find_descendant_pids(root_pid);
        assert!(
            descendants.len() >= 2,
            "should find at least 2 descendants (inner sh + sleep), got {}",
            descendants.len()
        );

        // Cleanup
        let _ = root.kill();
        let _ = root.wait();
        for pid in &descendants {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(*pid as i32), Signal::SIGKILL);
        }
    }

    #[test]
    fn find_descendant_pids_returns_empty_for_leaf_process() {
        // sleep has no children
        let mut leaf = spawn_in_group("sleep", &["300"]);

        let leaf_pid = leaf.id();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let descendants = find_descendant_pids(leaf_pid);
        assert!(descendants.is_empty(), "leaf process should have no descendants");

        let _ = leaf.kill();
        let _ = leaf.wait();
    }

    #[test]
    fn find_descendant_pids_returns_empty_for_nonexistent_pid() {
        // PID 999999999 almost certainly doesn't exist
        let descendants = find_descendant_pids(999_999_999);
        assert!(descendants.is_empty());
    }

    #[test]
    fn verify_and_cleanup_kills_entire_tree() {
        use nix::sys::signal::{kill as nix_kill, Signal};
        use nix::unistd::Pid;

        // Spawn a process group: sh → sleep & sleep & wait
        let mut root = spawn_in_group("sh", &["-c", "sleep 300 & sleep 300 & wait"]);

        let root_pid = root.id();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let descendants_before = find_descendant_pids(root_pid);
        assert!(
            !descendants_before.is_empty(),
            "should have descendants before cleanup"
        );

        verify_and_cleanup(root_pid);

        // Reap the root zombie (direct child of test process)
        let _ = root.wait();
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Root should be fully reaped — signal check returns ESRCH
        let root_dead = nix_kill(Pid::from_raw(root_pid as i32), Signal::SIGCONT).is_err();
        assert!(root_dead, "root process should be dead after verify_and_cleanup");

        // Descendants were orphaned to init, which reaps them after SIGKILL
        for pid in &descendants_before {
            let alive = nix_kill(Pid::from_raw(*pid as i32), Signal::SIGCONT).is_ok();
            assert!(!alive, "descendant PID {} should be dead after cleanup", pid);
        }
    }

    #[test]
    fn pid_registry_insert_remove() {
        let registry = PidRegistry::new();
        registry.insert(1234);
        registry.insert(5678);

        let set = registry.inner.lock().unwrap();
        assert!(set.contains(&1234));
        assert!(set.contains(&5678));
        assert_eq!(set.len(), 2);
        drop(set);

        registry.remove(1234);
        let set = registry.inner.lock().unwrap();
        assert!(!set.contains(&1234));
        assert!(set.contains(&5678));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn pid_registry_kill_all_sync_on_empty_is_noop() {
        let registry = PidRegistry::new();
        // Should not panic
        registry.kill_all_sync();
    }

    #[test]
    fn pid_registry_kill_all_sync_kills_real_processes() {
        use nix::sys::signal::{kill as nix_kill, Signal};
        use nix::unistd::Pid;

        let registry = PidRegistry::new();

        let mut child1 = spawn_in_group("sleep", &["300"]);
        let mut child2 = spawn_in_group("sleep", &["300"]);

        let pid1 = child1.id();
        let pid2 = child2.id();
        registry.insert(pid1);
        registry.insert(pid2);

        registry.kill_all_sync();

        // Reap zombies (direct children of test process)
        let _ = child1.wait();
        let _ = child2.wait();

        let dead1 = nix_kill(Pid::from_raw(pid1 as i32), Signal::SIGCONT).is_err();
        let dead2 = nix_kill(Pid::from_raw(pid2 as i32), Signal::SIGCONT).is_err();
        assert!(dead1, "child1 should be dead");
        assert!(dead2, "child2 should be dead");
    }
}
