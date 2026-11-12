use crate::files::{Problem, Result};
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

pub fn executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    let search = env::var_os("PATH")?;
    env::split_paths(&search)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

pub struct ProcessCommand {
    pub file: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl ProcessCommand {
    pub fn new(
        file: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            file: file.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: vec![],
        }
    }

    fn command(&self) -> Result<Command> {
        let mut command = std::process::Command::new(&self.file);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let extension = self
                .file
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if matches!(extension.as_str(), "cmd" | "bat") {
                let comspec =
                    env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
                command = std::process::Command::new(comspec);
                command.args(["/d", "/s", "/v:off", "/c"]);
                let mut parameters = Vec::new();
                for (index, value) in std::iter::once(self.file.as_os_str())
                    .chain(self.args.iter().map(OsString::as_os_str))
                    .enumerate()
                {
                    if value
                        .to_string_lossy()
                        .contains(['"', '%', '!', '\r', '\n', '\0'])
                    {
                        return Err(Problem::new(
                            422,
                            "The Windows Flutter batch launcher cannot use paths containing %, !, or quotes.",
                        ));
                    }
                    let variable = format!("FREEFLOW_CMD_ARG_{index}");
                    command.env(&variable, value);
                    parameters.push(format!(r#""%{variable}%""#));
                }
                command.raw_arg(format!(r#""{}""#, parameters.join(" ")));
            } else {
                command.args(&self.args);
            }
            command.creation_flags(0x0800_0000);
        }
        #[cfg(not(windows))]
        command.args(&self.args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.envs(self.env.iter().map(|(key, value)| (key, value)));
        Ok(Command::from(command))
    }

    pub fn spawn(&self, cwd: Option<&Path>, keep_input: bool) -> Result<Process> {
        let mut command = self.command()?;
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(if keep_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            Problem::new(
                500,
                format!("Could not start {}: {error}", self.file.display()),
            )
        })?;
        let stdin = child.stdin.take();
        let (sender, lines) = mpsc::channel(128);
        tokio::spawn(read_lines(child.stdout.take().unwrap(), sender.clone()));
        tokio::spawn(read_lines(child.stderr.take().unwrap(), sender));
        Ok(Process {
            child,
            stdin,
            lines,
        })
    }
}

async fn read_lines(mut input: impl AsyncRead + Unpin, sender: mpsc::Sender<String>) {
    let mut chunk = [0; 8192];
    let mut pending = Vec::new();
    while let Ok(length) = input.read(&mut chunk).await {
        if length == 0 {
            break;
        }
        for byte in &chunk[..length] {
            if matches!(*byte, b'\r' | b'\n') || pending.len() == 50_000 {
                if !pending.is_empty() {
                    if sender
                        .send(String::from_utf8_lossy(&pending).into_owned())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    pending.clear();
                }
                if matches!(*byte, b'\r' | b'\n') {
                    continue;
                }
            }
            pending.push(*byte);
        }
    }
    if !pending.is_empty() {
        let _ = sender
            .send(String::from_utf8_lossy(&pending).into_owned())
            .await;
    }
}

pub struct Process {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub lines: mpsc::Receiver<String>,
}

impl Process {
    pub async fn write(&mut self, input: &str) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Problem::new(500, "The Flutter command closed its input."))?;
        stdin.write_all(input.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn stop(&mut self) {
        let Some(pid) = self.child.id() else {
            return;
        };
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(windows)]
        {
            let mut command = Command::new("taskkill.exe");
            command
                .args(["/pid", &pid.to_string(), "/t", "/f"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .creation_flags(0x0800_0000);
            let _ =
                tokio::time::timeout(Duration::from_secs(3), command.kill_on_drop(true).status())
                    .await;
        }
        #[cfg(unix)]
        {
            // The child was started in its own process group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
                .await
                .is_ok()
            {
                return;
            }
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill().await;
    }
}

pub async fn run(
    command: &ProcessCommand,
    cwd: Option<&Path>,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(String),
) -> Result<ExitStatus> {
    if cancel.is_cancelled() {
        return Err(Problem::new(409, "Command stopped."));
    }
    let mut process = command.spawn(cwd, false)?;
    let result = loop {
        tokio::select! {
            status = process.child.wait() => break Ok(status?),
            line = process.lines.recv(), if !process.lines.is_closed() => {
                if let Some(line) = line { on_line(line); }
            }
            _ = cancel.cancelled() => {
                process.stop().await;
                break Err(Problem::new(409, "Command stopped."));
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(line) = process.lines.recv().await {
            on_line(line);
        }
    })
    .await;
    result
}
