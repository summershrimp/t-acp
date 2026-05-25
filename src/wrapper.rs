use crate::adapters;
use crate::api::{ExitRequest, RegisterAgentRequest};
use crate::http::{ControlClient, InternalWsHandle};
use crate::pty;
use crate::util::{now_millis, sanitize_id_part};
use anyhow::{Context, Result, bail};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use portable_pty::PtySize;
use signal_hook::consts::signal::SIGWINCH;
use signal_hook::iterator::Signals;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

type SharedPtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub fn run(mut args: Vec<String>, addr: &str) -> Result<u8> {
    let agent = args.remove(0);
    let agent_kind = adapters::canonical_agent_kind(&agent);
    let client = ensure_daemon(addr)?;

    let cwd = env::current_dir()
        .context("failed to read current directory")?
        .display()
        .to_string();
    let instance_id = format!(
        "{}-{}-{}",
        sanitize_id_part(&agent_kind),
        std::process::id(),
        now_millis()
    );
    let command = build_command_string(&agent, &args);
    let initial_size = current_pty_size();
    let mut child_pty = pty::spawn(&agent, &args, initial_size)
        .with_context(|| format!("failed to launch {agent}"))?;

    register_instance(
        &client,
        &instance_id,
        &agent_kind,
        child_pty.pid,
        &cwd,
        &command,
        initial_size.rows,
        initial_size.cols,
    )?;
    let ws = client.connect_agent_ws(&instance_id)?;

    let _raw_mode = RawMode::enter().ok();
    let stop = Arc::new(AtomicBool::new(false));
    let pty_writer = Arc::new(Mutex::new(child_pty.writer));

    spawn_stdin_forwarder(Arc::clone(&pty_writer), Arc::clone(&stop));
    spawn_rpc_forwarder(ws.clone(), Arc::clone(&pty_writer), Arc::clone(&stop));
    spawn_resize_forwarder(ws.clone(), child_pty.master.clone(), Arc::clone(&stop));
    let output_handle = spawn_output_forwarder(ws, child_pty.reader, Arc::clone(&stop));

    let exit_status = child_pty.child.wait().context("failed to wait for child")?;
    stop.store(true, Ordering::SeqCst);
    let exit_code = exit_status.exit_code().min(255) as u8;
    let _ = post_exit(&client, &instance_id, exit_code);
    let _ = output_handle.join();

    Ok(exit_code)
}

fn ensure_daemon(addr: &str) -> Result<ControlClient> {
    let client = ControlClient::new(addr)?;
    if client.is_healthy() {
        return Ok(client);
    }

    let current_exe = env::current_exe().context("failed to locate current executable")?;
    Command::new(current_exe)
        .arg("daemon")
        .arg("--addr")
        .arg(addr)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start background daemon")?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if client.is_healthy() {
            return Ok(client);
        }
        thread::sleep(Duration::from_millis(50));
    }

    bail!("background daemon did not become healthy at {addr}")
}

fn register_instance(
    client: &ControlClient,
    instance_id: &str,
    agent: &str,
    pid: Option<u32>,
    cwd: &str,
    command: &str,
    rows: u16,
    cols: u16,
) -> Result<()> {
    client
        .register_agent(&RegisterAgentRequest {
            id: instance_id.to_string(),
            agent_kind: agent.to_string(),
            pid,
            cwd: cwd.to_string(),
            command: command.to_string(),
            rows,
            cols,
        })
        .map(|_| ())
}

fn post_exit(client: &ControlClient, instance_id: &str, exit_code: u8) -> Result<()> {
    client.post_exit(
        instance_id,
        &ExitRequest {
            status: exit_code.to_string(),
        },
    )
}

fn spawn_stdin_forwarder(
    pty_writer: SharedPtyWriter,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 8192];

        while !stop.load(Ordering::SeqCst) {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if write_to_pty(&pty_writer, &buffer[..read]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn spawn_rpc_forwarder(
    mut ws: InternalWsHandle,
    pty_writer: SharedPtyWriter,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match ws.recv_command_timeout(Duration::from_millis(100)) {
                Some(command) if !command.is_empty() => {
                    if write_to_pty(&pty_writer, &command).is_err() {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
}

fn spawn_resize_forwarder(
    ws: InternalWsHandle,
    pty_master: pty::SharedMasterPty,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Ok(mut signals) = Signals::new([SIGWINCH]) else {
            return;
        };

        for _ in signals.forever() {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            let (cols, rows) = size().unwrap_or((80, 24));
            let _ = pty::resize(&pty_master, rows, cols);
            let _ = ws.send_resize(rows, cols);
        }
    })
}

fn spawn_output_forwarder(
    ws: InternalWsHandle,
    mut pty_reader: Box<dyn Read + Send>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let mut buffer = [0_u8; 8192];

        loop {
            match pty_reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    if stdout.write_all(chunk).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                    let _ = ws.send_output(chunk);
                }
                Err(_) => break,
            }

            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
    })
}

fn current_pty_size() -> PtySize {
    let (cols, rows) = size().unwrap_or((80, 24));
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn write_to_pty(pty_writer: &SharedPtyWriter, bytes: &[u8]) -> io::Result<()> {
    let mut writer = pty_writer
        .lock()
        .map_err(|_| io::Error::other("PTY writer lock poisoned"))?;
    writer.write_all(bytes)?;
    writer.flush()
}

fn build_command_string(agent: &str, args: &[String]) -> String {
    let mut command = vec![agent.to_string()];
    command.extend(args.iter().cloned());
    command.join(" ")
}

struct RawMode {
    enabled: bool,
}

impl RawMode {
    fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() {
            return Ok(Self { enabled: false });
        }

        enable_raw_mode()?;
        Ok(Self { enabled: true })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.enabled {
            let _ = disable_raw_mode();
        }
    }
}
