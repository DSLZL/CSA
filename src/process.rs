use crate::error::{ManagerError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<OsString, OsString>,
    pub env_remove: BTreeSet<OsString>,
    pub inherit_stdio: bool,
}

impl CommandSpec {
    pub fn captured(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_remove: BTreeSet::new(),
            inherit_stdio: false,
        }
    }

    pub fn arg(mut self, value: impl Into<OsString>) -> Self {
        self.args.push(value.into());
        self
    }

    pub fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.insert(key.into());
        self
    }

    pub fn inherited(mut self) -> Self {
        self.inherit_stdio = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandResult {
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    pub fn require_success(self, context: &str) -> Result<Self> {
        if self.code == Some(0) {
            return Ok(self);
        }
        let detail = String::from_utf8_lossy(&self.stderr).trim().to_owned();
        Err(ManagerError::new(
            "command_failed",
            format!("{context} exited {:?}: {detail}", self.code),
        ))
    }
}

pub trait ProcessRunner: Send + Sync {
    fn run(&self, command: &CommandSpec) -> Result<CommandResult>;
}

#[derive(Debug, Default)]
pub struct RealProcessRunner;

impl ProcessRunner for RealProcessRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandResult> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        for key in &spec.env_remove {
            command.env_remove(key);
        }
        command.envs(&spec.env);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        if spec.inherit_stdio {
            let status = command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|error| command_error(&spec.program, error))?;
            Ok(CommandResult {
                code: status.code(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        } else {
            let output = command
                .stdin(Stdio::null())
                .output()
                .map_err(|error| command_error(&spec.program, error))?;
            Ok(CommandResult {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }
}

fn command_error(program: &Path, error: std::io::Error) -> ManagerError {
    ManagerError::io(&format!("run {}", program.display()), error)
}

pub fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}
