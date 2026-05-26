use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub struct ChildPty {
    pub pid: Option<u32>,
    pub child: Box<dyn Child + Send + Sync>,
    pub master: SharedMasterPty,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

pub type SharedMasterPty = Arc<Mutex<Box<dyn MasterPty + Send>>>;

pub fn spawn(command: &str, args: &[String], cwd: &str, size: PtySize) -> Result<ChildPty> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).context("failed to open PTY")?;

    let command_builder = build_command(command, args, cwd);

    let child = pair
        .slave
        .spawn_command(command_builder)
        .context("failed to spawn command in PTY")?;
    let pid = child.process_id();
    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = pair
        .master
        .take_writer()
        .context("failed to take PTY writer")?;

    Ok(ChildPty {
        pid,
        child,
        master: Arc::new(Mutex::new(pair.master)),
        reader,
        writer,
    })
}

fn build_command(command: &str, args: &[String], cwd: &str) -> CommandBuilder {
    let mut command_builder = CommandBuilder::new(command);
    for arg in args {
        command_builder.arg(arg);
    }
    command_builder.cwd(cwd);
    command_builder
}

pub fn resize(master: &SharedMasterPty, rows: u16, cols: u16) -> Result<()> {
    let master = master.lock().expect("PTY master lock poisoned");
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to resize PTY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_sets_working_directory() {
        let command = build_command(
            "/bin/pwd",
            &["-L".to_string()],
            "/tmp/opencode/child working dir",
        );

        assert_eq!(command.get_argv()[0].to_string_lossy(), "/bin/pwd");
        assert_eq!(command.get_argv()[1].to_string_lossy(), "-L");
        assert_eq!(
            command
                .get_cwd()
                .map(|value| value.to_string_lossy().to_string()),
            Some("/tmp/opencode/child working dir".to_string())
        );
    }
}
