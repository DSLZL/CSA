use crate::error::{ManagerError, Result};
use crate::isolation::IsolationRequest;
use crate::manager::{
    DoctorOptions, ExecOptions, InstallOptions, OnlineInstallOptions, PrepareOptions,
};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
csa [--json] <command>

csa doctor [--manager-root PATH] [--official PATH] [--official-native PATH] [--manifest PATH]
csa install [--yes] [--manager-root PATH] [--official PATH] [--official-native PATH] [--compat ID | --manifest PATH (--artifact PATH | --source PATH)]
csa uninstall [--manager-root PATH]
csa prepare [--manager-root PATH] [--official PATH] [--official-native PATH] --manifest PATH (--artifact PATH | --source PATH)
csa plug [--manager-root PATH]
csa unplug [--manager-root PATH]
csa status [--manager-root PATH]
csa purge [--manager-root PATH]
csa exec --isolated [--manager-root PATH] --codex-home PATH --cwd PATH --logs-dir PATH --state-dir PATH --record PATH [--npm-prefix PATH] -- [CODEX_ARGS...]

Global option: --json writes machine-readable output and may appear before or after the command.";

#[derive(Clone, Debug)]
pub enum Cli {
    Doctor(DoctorOptions),
    Install { options: InstallOptions, yes: bool },
    Uninstall { manager_root: Option<PathBuf> },
    Prepare(PrepareOptions),
    Plug { manager_root: Option<PathBuf> },
    Unplug { manager_root: Option<PathBuf> },
    Status { manager_root: Option<PathBuf> },
    Purge { manager_root: Option<PathBuf> },
    Exec(ExecOptions),
    Help,
    Version,
}

#[derive(Clone, Debug)]
pub struct Invocation {
    pub command: Cli,
    pub explicit_json: bool,
}

impl Invocation {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut args: VecDeque<_> = args.into_iter().collect();
        let mut explicit_json = false;
        while args.front().is_some_and(|arg| arg == "--json") {
            args.pop_front();
            set_json(&mut explicit_json)?;
        }
        let Some(command) = args.pop_front() else {
            return Ok(Self {
                command: Cli::Help,
                explicit_json,
            });
        };
        let command = command.to_str().ok_or_else(|| {
            ManagerError::new("invalid_cli", "command name must be valid Unicode")
        })?;
        let command = match command {
            "doctor" => parse_doctor(args, &mut explicit_json),
            "install" => parse_install(args, &mut explicit_json),
            "uninstall" => parse_root_only(args, &mut explicit_json, |manager_root| {
                Cli::Uninstall { manager_root }
            }),
            "prepare" => parse_prepare(args, "prepare", &mut explicit_json, Cli::Prepare),
            "plug" => parse_root_only(args, &mut explicit_json, |manager_root| Cli::Plug {
                manager_root,
            }),
            "unplug" => parse_root_only(args, &mut explicit_json, |manager_root| Cli::Unplug {
                manager_root,
            }),
            "status" => parse_root_only(args, &mut explicit_json, |manager_root| Cli::Status {
                manager_root,
            }),
            "purge" => parse_root_only(args, &mut explicit_json, |manager_root| Cli::Purge {
                manager_root,
            }),
            "exec" => parse_exec(args, &mut explicit_json),
            "help" | "--help" | "-h" => parse_no_args(args, &mut explicit_json, Cli::Help),
            "--version" | "-V" => parse_no_args(args, &mut explicit_json, Cli::Version),
            _ => Err(ManagerError::new(
                "invalid_cli",
                format!("unknown command: {command}"),
            )),
        }?;
        Ok(Self {
            command,
            explicit_json,
        })
    }
}

pub fn json_requested(args: &[OsString]) -> bool {
    args.iter()
        .take_while(|argument| argument.as_os_str() != "--")
        .any(|argument| argument.as_os_str() == "--json")
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        Invocation::parse(args).map(|invocation| invocation.command)
    }
}

fn parse_install(mut args: VecDeque<OsString>, explicit_json: &mut bool) -> Result<Cli> {
    let mut manager_root = None;
    let mut official = None;
    let mut official_native = None;
    let mut manifest = None;
    let mut artifact = None;
    let mut source = None;
    let mut compat = None;
    let mut yes = false;
    while let Some(flag) = args.pop_front() {
        match unicode_flag(&flag)? {
            "--manager-root" => set_path(
                &mut manager_root,
                take_value(&mut args, "--manager-root")?,
                "--manager-root",
            )?,
            "--official" => set_path(
                &mut official,
                take_value(&mut args, "--official")?,
                "--official",
            )?,
            "--official-native" => set_path(
                &mut official_native,
                take_value(&mut args, "--official-native")?,
                "--official-native",
            )?,
            "--manifest" => set_path(
                &mut manifest,
                take_value(&mut args, "--manifest")?,
                "--manifest",
            )?,
            "--artifact" => set_path(
                &mut artifact,
                take_value(&mut args, "--artifact")?,
                "--artifact",
            )?,
            "--source" => set_path(&mut source, take_value(&mut args, "--source")?, "--source")?,
            "--compat" => set_string(&mut compat, take_value(&mut args, "--compat")?, "--compat")?,
            "--yes" if !yes => yes = true,
            "--yes" => return Err(duplicate_flag("--yes")),
            "--json" => set_json(explicit_json)?,
            "--help" | "-h" => return Ok(Cli::Help),
            flag => return Err(unknown_flag(flag)),
        }
    }
    let options = match (manifest, artifact, source) {
        (None, None, None) => InstallOptions::Online(OnlineInstallOptions {
            manager_root,
            official,
            official_native,
            compat,
        }),
        (Some(manifest), Some(artifact), None) if compat.is_none() && !yes => {
            InstallOptions::Local(PrepareOptions {
                manager_root,
                official,
                official_native,
                manifest,
                artifact: Some(artifact),
                source: None,
            })
        }
        (Some(manifest), None, Some(source)) if compat.is_none() && !yes => {
            InstallOptions::Local(PrepareOptions {
                manager_root,
                official,
                official_native,
                manifest,
                artifact: None,
                source: Some(source),
            })
        }
        _ => {
            return Err(ManagerError::new(
                "invalid_cli",
                "install accepts --yes/--compat only in online mode, or --manifest with exactly one of --artifact/--source for local mode",
            ));
        }
    };
    Ok(Cli::Install { options, yes })
}

fn parse_doctor(mut args: VecDeque<OsString>, explicit_json: &mut bool) -> Result<Cli> {
    let mut manager_root = None;
    let mut official = None;
    let mut official_native = None;
    let mut manifest = None;
    while let Some(flag) = args.pop_front() {
        match unicode_flag(&flag)? {
            "--manager-root" => set_path(
                &mut manager_root,
                take_value(&mut args, "--manager-root")?,
                "--manager-root",
            )?,
            "--official" => set_path(
                &mut official,
                take_value(&mut args, "--official")?,
                "--official",
            )?,
            "--official-native" => set_path(
                &mut official_native,
                take_value(&mut args, "--official-native")?,
                "--official-native",
            )?,
            "--manifest" => set_path(
                &mut manifest,
                take_value(&mut args, "--manifest")?,
                "--manifest",
            )?,
            "--json" => set_json(explicit_json)?,
            "--help" | "-h" => return Ok(Cli::Help),
            flag => return Err(unknown_flag(flag)),
        }
    }
    Ok(Cli::Doctor(DoctorOptions {
        manager_root,
        official,
        official_native,
        manifest,
    }))
}

fn parse_prepare(
    mut args: VecDeque<OsString>,
    command: &str,
    explicit_json: &mut bool,
    make: impl FnOnce(PrepareOptions) -> Cli,
) -> Result<Cli> {
    let mut manager_root = None;
    let mut official = None;
    let mut official_native = None;
    let mut manifest = None;
    let mut artifact = None;
    let mut source = None;
    while let Some(flag) = args.pop_front() {
        match unicode_flag(&flag)? {
            "--manager-root" => set_path(
                &mut manager_root,
                take_value(&mut args, "--manager-root")?,
                "--manager-root",
            )?,
            "--official" => set_path(
                &mut official,
                take_value(&mut args, "--official")?,
                "--official",
            )?,
            "--official-native" => set_path(
                &mut official_native,
                take_value(&mut args, "--official-native")?,
                "--official-native",
            )?,
            "--manifest" => set_path(
                &mut manifest,
                take_value(&mut args, "--manifest")?,
                "--manifest",
            )?,
            "--artifact" => set_path(
                &mut artifact,
                take_value(&mut args, "--artifact")?,
                "--artifact",
            )?,
            "--source" => set_path(&mut source, take_value(&mut args, "--source")?, "--source")?,
            "--json" => set_json(explicit_json)?,
            "--help" | "-h" => return Ok(Cli::Help),
            flag => return Err(unknown_flag(flag)),
        }
    }
    let manifest = manifest.ok_or_else(|| {
        ManagerError::new("invalid_cli", format!("{command} requires --manifest PATH"))
    })?;
    Ok(make(PrepareOptions {
        manager_root,
        official,
        official_native,
        manifest,
        artifact,
        source,
    }))
}

fn parse_root_only(
    mut args: VecDeque<OsString>,
    explicit_json: &mut bool,
    make: impl FnOnce(Option<PathBuf>) -> Cli,
) -> Result<Cli> {
    let mut manager_root = None;
    while let Some(flag) = args.pop_front() {
        match unicode_flag(&flag)? {
            "--manager-root" => set_path(
                &mut manager_root,
                take_value(&mut args, "--manager-root")?,
                "--manager-root",
            )?,
            "--json" => set_json(explicit_json)?,
            "--help" | "-h" => return Ok(Cli::Help),
            flag => return Err(unknown_flag(flag)),
        }
    }
    Ok(make(manager_root))
}

fn parse_exec(mut args: VecDeque<OsString>, explicit_json: &mut bool) -> Result<Cli> {
    let mut isolated = false;
    let mut manager_root = None;
    let mut codex_home = None;
    let mut cwd = None;
    let mut logs_dir = None;
    let mut state_dir = None;
    let mut record_path = None;
    let mut npm_prefix = None;
    let mut child_args = Vec::new();
    let mut separator = false;
    while let Some(flag) = args.pop_front() {
        if separator {
            child_args.push(flag);
            continue;
        }
        match unicode_flag(&flag)? {
            "--" => separator = true,
            "--isolated" if !isolated => isolated = true,
            "--isolated" => return Err(duplicate_flag("--isolated")),
            "--manager-root" => set_path(
                &mut manager_root,
                take_value(&mut args, "--manager-root")?,
                "--manager-root",
            )?,
            "--codex-home" => set_path(
                &mut codex_home,
                take_value(&mut args, "--codex-home")?,
                "--codex-home",
            )?,
            "--cwd" => set_path(&mut cwd, take_value(&mut args, "--cwd")?, "--cwd")?,
            "--logs-dir" => set_path(
                &mut logs_dir,
                take_value(&mut args, "--logs-dir")?,
                "--logs-dir",
            )?,
            "--state-dir" => set_path(
                &mut state_dir,
                take_value(&mut args, "--state-dir")?,
                "--state-dir",
            )?,
            "--record" => set_path(
                &mut record_path,
                take_value(&mut args, "--record")?,
                "--record",
            )?,
            "--npm-prefix" => set_path(
                &mut npm_prefix,
                take_value(&mut args, "--npm-prefix")?,
                "--npm-prefix",
            )?,
            "--json" => set_json(explicit_json)?,
            "--help" | "-h" => return Ok(Cli::Help),
            flag => return Err(unknown_flag(flag)),
        }
    }
    if !isolated {
        return Err(ManagerError::new(
            "invalid_cli",
            "exec requires --isolated; non-isolated execution is not supported",
        ));
    }
    if !separator {
        return Err(ManagerError::new(
            "invalid_cli",
            "exec requires -- before Codex arguments",
        ));
    }
    Ok(Cli::Exec(ExecOptions {
        manager_root,
        isolation: IsolationRequest {
            codex_home: required_path(codex_home, "--codex-home")?,
            cwd: required_path(cwd, "--cwd")?,
            logs_dir: required_path(logs_dir, "--logs-dir")?,
            state_dir: required_path(state_dir, "--state-dir")?,
            npm_prefix,
            record_path: required_path(record_path, "--record")?,
        },
        args: child_args,
    }))
}

fn parse_no_args(
    mut args: VecDeque<OsString>,
    explicit_json: &mut bool,
    command: Cli,
) -> Result<Cli> {
    while let Some(flag) = args.pop_front() {
        match unicode_flag(&flag)? {
            "--json" => set_json(explicit_json)?,
            flag => return Err(unknown_flag(flag)),
        }
    }
    Ok(command)
}

fn take_value(args: &mut VecDeque<OsString>, flag: &str) -> Result<OsString> {
    args.pop_front()
        .ok_or_else(|| ManagerError::new("invalid_cli", format!("{flag} requires a value")))
}

fn set_path(slot: &mut Option<PathBuf>, value: OsString, flag: &str) -> Result<()> {
    if slot.replace(PathBuf::from(value)).is_some() {
        return Err(duplicate_flag(flag));
    }
    Ok(())
}

fn set_string(slot: &mut Option<String>, value: OsString, flag: &str) -> Result<()> {
    let value = value
        .into_string()
        .map_err(|_| ManagerError::new("invalid_cli", format!("{flag} must be valid Unicode")))?;
    if value.is_empty() {
        return Err(ManagerError::new(
            "invalid_cli",
            format!("{flag} must not be empty"),
        ));
    }
    if slot.replace(value).is_some() {
        return Err(duplicate_flag(flag));
    }
    Ok(())
}

fn required_path(value: Option<PathBuf>, flag: &str) -> Result<PathBuf> {
    value.ok_or_else(|| ManagerError::new("invalid_cli", format!("exec requires {flag} PATH")))
}

fn unicode_flag(value: &OsString) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| ManagerError::new("invalid_cli", "option names must be valid Unicode"))
}

fn unknown_flag(flag: &str) -> ManagerError {
    ManagerError::new("invalid_cli", format!("unknown option: {flag}"))
}

fn duplicate_flag(flag: &str) -> ManagerError {
    ManagerError::new("invalid_cli", format!("duplicate option: {flag}"))
}

fn set_json(explicit_json: &mut bool) -> Result<()> {
    if *explicit_json {
        return Err(duplicate_flag("--json"));
    }
    *explicit_json = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, InstallOptions, Invocation, json_requested};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> crate::error::Result<Cli> {
        Cli::parse(args.iter().map(OsString::from))
    }

    fn invocation(args: &[&str]) -> crate::error::Result<Invocation> {
        Invocation::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn json_is_global_but_stops_at_the_exec_separator() {
        assert!(invocation(&["--json", "status"]).unwrap().explicit_json);
        assert!(invocation(&["status", "--json"]).unwrap().explicit_json);
        assert!(invocation(&["--json", "status", "--json"]).is_err());

        let invocation = invocation(&[
            "exec",
            "--isolated",
            "--codex-home",
            "C:/tmp/home",
            "--cwd",
            "C:/tmp/cwd",
            "--logs-dir",
            "C:/tmp/logs",
            "--state-dir",
            "C:/tmp/state",
            "--record",
            "C:/tmp/record.json",
            "--",
            "--json",
        ])
        .unwrap();
        assert!(!invocation.explicit_json);
        let Cli::Exec(options) = invocation.command else {
            panic!("expected exec")
        };
        assert_eq!(options.args, ["--json"].map(OsString::from));
        assert!(!json_requested(&[
            OsString::from("exec"),
            OsString::from("--"),
            OsString::from("--json"),
        ]));
    }

    #[test]
    fn exec_requires_isolation_and_separator() {
        assert!(parse(&["exec", "--"]).is_err());
        assert!(parse(&["exec", "--isolated"]).is_err());
    }

    #[test]
    fn duplicate_options_are_rejected() {
        assert!(parse(&["status", "--manager-root", "a", "--manager-root", "b"]).is_err());
    }

    #[test]
    fn install_supports_online_and_explicit_local_modes() {
        let Cli::Install {
            options: InstallOptions::Online(options),
            yes,
        } = parse(&["install"]).unwrap()
        else {
            panic!("expected online install")
        };
        assert!(!yes);
        assert!(options.manager_root.is_none());
        assert!(options.official.is_none());
        assert!(options.compat.is_none());

        let Cli::Install {
            options: InstallOptions::Online(options),
            yes,
        } = parse(&[
            "install",
            "--yes",
            "--compat",
            "rust-v0.149.0-native-join-p3",
        ])
        .unwrap()
        else {
            panic!("expected selected online install")
        };
        assert!(yes);
        assert_eq!(
            options.compat.as_deref(),
            Some("rust-v0.149.0-native-join-p3")
        );
        assert!(parse(&["install", "--compat", "a", "--compat", "b"]).is_err());
        assert!(parse(&["install", "--yes", "--yes"]).is_err());
        assert!(
            parse(&[
                "install",
                "--compat",
                "rust-v0.149.0-native-join-p3",
                "--manifest",
                "C:/tmp/manifest.toml",
                "--artifact",
                "C:/tmp/patched.exe",
            ])
            .is_err()
        );

        let cli = parse(&[
            "install",
            "--manager-root",
            "C:/tmp/csa",
            "--official",
            "C:/tmp/codex.exe",
            "--official-native",
            "C:/tmp/native.exe",
            "--manifest",
            "C:/tmp/manifest.toml",
            "--artifact",
            "C:/tmp/patched.exe",
        ])
        .unwrap();
        let Cli::Install {
            options: InstallOptions::Local(options),
            yes: false,
        } = cli
        else {
            panic!("expected local install")
        };
        assert_eq!(
            options.manager_root.unwrap(),
            std::path::Path::new("C:/tmp/csa")
        );
        assert_eq!(
            options.manifest,
            std::path::Path::new("C:/tmp/manifest.toml")
        );
        assert_eq!(
            options.artifact.unwrap(),
            std::path::Path::new("C:/tmp/patched.exe")
        );
        assert!(options.source.is_none());
        assert!(parse(&["install", "--manifest", "C:/tmp/manifest.toml"]).is_err());
        assert!(parse(&["install", "--artifact", "C:/tmp/patched.exe"]).is_err());
        assert!(
            parse(&[
                "install",
                "--yes",
                "--manifest",
                "C:/tmp/manifest.toml",
                "--artifact",
                "C:/tmp/patched.exe",
            ])
            .is_err()
        );

        let Cli::Uninstall { manager_root } =
            parse(&["uninstall", "--manager-root", "C:/tmp/csa"]).unwrap()
        else {
            panic!("expected uninstall")
        };
        assert_eq!(manager_root.unwrap(), std::path::Path::new("C:/tmp/csa"));
        assert!(parse(&["uninstall", "--manifest", "ignored"]).is_err());
    }

    #[test]
    fn child_arguments_are_not_reparsed() {
        let cli = parse(&[
            "exec",
            "--isolated",
            "--codex-home",
            "C:/tmp/home",
            "--cwd",
            "C:/tmp/cwd",
            "--logs-dir",
            "C:/tmp/logs",
            "--state-dir",
            "C:/tmp/state",
            "--record",
            "C:/tmp/record.json",
            "--",
            "--model",
            "o3",
        ])
        .unwrap();
        let Cli::Exec(options) = cli else {
            panic!("expected exec")
        };
        assert_eq!(options.args, ["--model", "o3"].map(OsString::from));
    }
}
