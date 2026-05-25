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

const FOCUS_ENABLE: &[u8] = b"\x1b[?1004h";
const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";
const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

pub fn run(mut args: Vec<String>, addr: &str) -> Result<u8> {
    let agent = args.remove(0);
    let agent_kind = adapters::canonical_agent_kind(&agent);
    let client = ensure_daemon(addr)?;
    maybe_warn_about_tmux_focus_events();

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
    let focus_reporting_enabled = Arc::new(AtomicBool::new(false));

    let _focus_guard = FocusModeGuard::enter(Arc::clone(&focus_reporting_enabled)).ok();

    spawn_stdin_forwarder(
        ws.clone(),
        Arc::clone(&pty_writer),
        Arc::clone(&focus_reporting_enabled),
        Arc::clone(&stop),
    );
    spawn_rpc_forwarder(ws.clone(), Arc::clone(&pty_writer), Arc::clone(&stop));
    spawn_resize_forwarder(ws.clone(), child_pty.master.clone(), Arc::clone(&stop));
    let output_handle = spawn_output_forwarder(
        ws,
        child_pty.reader,
        Arc::clone(&focus_reporting_enabled),
        Arc::clone(&stop),
    );

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
    ws: InternalWsHandle,
    pty_writer: SharedPtyWriter,
    focus_reporting_enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 8192];
        let mut parser = FocusInputParser::default();

        while !stop.load(Ordering::SeqCst) {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let update = parser.process(
                        &buffer[..read],
                        focus_reporting_enabled.load(Ordering::SeqCst),
                    );
                    for &focused in &update.events {
                        let _ = ws.send_focus(focused);
                    }

                    if write_to_pty(&pty_writer, &update.bytes).is_err() {
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
    focus_reporting_enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let mut buffer = [0_u8; 8192];
        let mut parser = FocusModeParser::default();

        loop {
            match pty_reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    let filtered = parser.filter(chunk, &focus_reporting_enabled);
                    if stdout.write_all(&filtered).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                    let _ = ws.send_output(&filtered);
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

struct FocusModeGuard {
    enabled: bool,
    focus_reporting_enabled: Arc<AtomicBool>,
}

#[derive(Default)]
struct FocusModeParser {
    pending: Vec<u8>,
}

#[derive(Default)]
struct FocusInputParser {
    pending: Vec<u8>,
}

struct FocusInputUpdate {
    bytes: Vec<u8>,
    events: Vec<bool>,
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

impl FocusModeGuard {
    fn enter(focus_reporting_enabled: Arc<AtomicBool>) -> io::Result<Self> {
        if !io::stdout().is_terminal() {
            return Ok(Self {
                enabled: false,
                focus_reporting_enabled,
            });
        }

        io::stdout().write_all(FOCUS_ENABLE)?;
        io::stdout().flush()?;

        Ok(Self {
            enabled: true,
            focus_reporting_enabled,
        })
    }
}

impl FocusModeParser {
    fn filter(&mut self, chunk: &[u8], focus_reporting_enabled: &AtomicBool) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::with_capacity(self.pending.len());
        let mut index = 0;

        while index < self.pending.len() {
            let remaining = &self.pending[index..];
            if remaining.starts_with(FOCUS_ENABLE) {
                focus_reporting_enabled.store(true, Ordering::SeqCst);
                index += FOCUS_ENABLE.len();
                continue;
            }

            if remaining.starts_with(FOCUS_DISABLE) {
                focus_reporting_enabled.store(false, Ordering::SeqCst);
                index += FOCUS_DISABLE.len();
                continue;
            }

            let incomplete_enable = FOCUS_ENABLE
                .starts_with(remaining)
                .then_some(remaining.len())
                .unwrap_or(0);
            let incomplete_disable = FOCUS_DISABLE
                .starts_with(remaining)
                .then_some(remaining.len())
                .unwrap_or(0);
            if incomplete_enable > 0 || incomplete_disable > 0 {
                break;
            }

            output.push(self.pending[index]);
            index += 1;
        }

        self.pending.drain(..index);
        output
    }
}

impl FocusInputParser {
    fn process(&mut self, chunk: &[u8], forward_focus_events: bool) -> FocusInputUpdate {
        self.pending.extend_from_slice(chunk);
        let mut bytes = Vec::with_capacity(self.pending.len());
        let mut events = Vec::new();
        let mut index = 0;

        while index < self.pending.len() {
            let remaining = &self.pending[index..];
            if remaining.starts_with(FOCUS_IN) {
                events.push(true);
                if forward_focus_events {
                    bytes.extend_from_slice(FOCUS_IN);
                }
                index += FOCUS_IN.len();
                continue;
            }

            if remaining.starts_with(FOCUS_OUT) {
                events.push(false);
                if forward_focus_events {
                    bytes.extend_from_slice(FOCUS_OUT);
                }
                index += FOCUS_OUT.len();
                continue;
            }

            let incomplete_in = FOCUS_IN
                .starts_with(remaining)
                .then_some(remaining.len())
                .unwrap_or(0);
            let incomplete_out = FOCUS_OUT
                .starts_with(remaining)
                .then_some(remaining.len())
                .unwrap_or(0);
            if incomplete_in > 0 || incomplete_out > 0 {
                break;
            }

            bytes.push(self.pending[index]);
            index += 1;
        }

        self.pending.drain(..index);
        FocusInputUpdate { bytes, events }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.enabled {
            let _ = disable_raw_mode();
        }
    }
}

impl Drop for FocusModeGuard {
    fn drop(&mut self) {
        self.focus_reporting_enabled.store(false, Ordering::SeqCst);
        if self.enabled {
            let mut stdout = io::stdout();
            let _ = stdout.write_all(FOCUS_DISABLE);
            let _ = stdout.flush();
        }
    }
}

fn maybe_warn_about_tmux_focus_events() {
    if env::var_os("TMUX").is_some() {
        eprintln!(
            "t-acp: tmux detected; if focus tracking seems inaccurate, enable `set -g focus-events on`"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_mode_parser_strips_enable_and_disable_sequences() {
        let enabled = AtomicBool::new(false);
        let mut parser = FocusModeParser::default();

        let output = parser.filter(b"hello\x1b[?1004hworld\x1b[?1004l!", &enabled);

        assert_eq!(output, b"helloworld!");
        assert!(!enabled.load(Ordering::SeqCst));
    }

    #[test]
    fn focus_mode_parser_handles_split_sequences() {
        let enabled = AtomicBool::new(false);
        let mut parser = FocusModeParser::default();

        assert_eq!(parser.filter(b"a\x1b[?10", &enabled), b"a");
        assert_eq!(parser.filter(b"04hb", &enabled), b"b");
        assert!(enabled.load(Ordering::SeqCst));
    }

    #[test]
    fn focus_input_parser_detects_focus_events() {
        let mut parser = FocusInputParser::default();

        let update = parser.process(b"x\x1b[Iy\x1b[O", false);

        assert_eq!(update.bytes, b"xy");
        assert_eq!(update.events, vec![true, false]);
    }

    #[test]
    fn focus_input_parser_keeps_partial_sequence_pending() {
        let mut parser = FocusInputParser::default();

        let update = parser.process(b"ab\x1b[", false);

        assert!(update.events.is_empty());
        assert_eq!(update.bytes, b"ab");
        assert_eq!(parser.pending, b"\x1b[");
    }

    #[test]
    fn focus_input_parser_forwards_focus_sequences_only_when_enabled() {
        let mut parser = FocusInputParser::default();

        let update = parser.process(b"\x1b[I", true);

        assert_eq!(update.bytes, FOCUS_IN);
        assert_eq!(update.events, vec![true]);
    }
}
