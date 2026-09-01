use crossterm::cursor::{Hide, MoveDown, MoveToColumn, MoveUp, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, size};
use crossterm::{SynchronizedUpdate, execute, queue};
use csa::activation::{CommandResolution, PlugReport, PurgeReport, UnplugReport};
use csa::error::{ManagerError, Result};
use csa::i18n::Language;
use csa::manager::{
    DoctorReport, InstallEvent, InstallReport, PrepareReport, StatusReport, UninstallReport,
};
use csa::online::InstallCandidate;
use serde::Serialize;
use std::cmp::min;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

const PROGRESS_REFRESH: Duration = Duration::from_millis(100);
const PICKER_ROWS: usize = 5;
pub(crate) const INSTALLATION_CANCELLED: &str = "installation_cancelled";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputMode {
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    Parse,
    Shim,
    Doctor,
    Install,
    Uninstall,
    Prepare,
    Plug,
    Unplug,
    Status,
    Purge,
    Exec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckSeverity {
    Pass,
    Warn,
    Fail,
}

struct DoctorCheck {
    severity: CheckSeverity,
    name: &'static str,
    detail: String,
    impact: Option<&'static str>,
    action: Option<&'static str>,
}

struct DoctorAssessment {
    checks: Vec<DoctorCheck>,
    incomplete: bool,
}

struct StatusView {
    conclusion: &'static str,
    installed: bool,
    active: bool,
    healthy: bool,
}

pub(crate) fn output_mode(explicit_json: bool) -> OutputMode {
    resolve_output_mode(explicit_json, io::stdout().is_terminal())
}

pub(crate) fn streams_are_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
}

pub(crate) struct InstallProgress {
    mode: OutputMode,
    language: Language,
    download_started: Option<Instant>,
    last_draw: Option<Instant>,
    line_active: bool,
    picker: bool,
}

impl InstallProgress {
    pub(crate) fn new(mode: OutputMode, picker: bool) -> Self {
        Self {
            mode,
            language: Language::detected(),
            download_started: None,
            last_draw: None,
            line_active: false,
            picker,
        }
    }

    pub(crate) fn event(&mut self, event: InstallEvent) {
        let stderr = io::stderr();
        let _ = self.write_event_to(&mut stderr.lock(), event, Instant::now());
    }

    pub(crate) fn finish(&mut self) {
        let stderr = io::stderr();
        let _ = self.finish_to(&mut stderr.lock());
    }

    fn write_event_to(
        &mut self,
        writer: &mut dyn Write,
        event: InstallEvent,
        now: Instant,
    ) -> io::Result<()> {
        if self.mode == OutputMode::Json {
            return Ok(());
        }
        if self.picker
            && matches!(
                event,
                InstallEvent::DetectingOfficial
                    | InstallEvent::DiscoveringCompatibility
                    | InstallEvent::SelectingCompatibility
            )
        {
            return Ok(());
        }
        if let InstallEvent::ArtifactProgress {
            downloaded_bytes,
            total_bytes,
        } = event
        {
            if self.last_draw.is_some_and(|last| {
                now.saturating_duration_since(last) < PROGRESS_REFRESH
                    && downloaded_bytes < total_bytes
            }) {
                return Ok(());
            }
            let started = *self.download_started.get_or_insert(now);
            self.last_draw = Some(now);
            self.line_active = true;
            let total_bytes = total_bytes.max(1);
            let percent = downloaded_bytes.saturating_mul(100) / total_bytes;
            let mib = 1024.0 * 1024.0;
            let rate = downloaded_bytes as f64
                / mib
                / now
                    .saturating_duration_since(started)
                    .as_secs_f64()
                    .max(0.001);
            write!(
                writer,
                "\r{}: {percent:>3}% ({:.1}/{:.1} MiB, {rate:.1} MiB/s)    ",
                self.language
                    .text("Downloading patched Codex", "正在下载补丁版 Codex"),
                downloaded_bytes as f64 / mib,
                total_bytes as f64 / mib,
            )?;
            return writer.flush();
        }

        self.finish_to(writer)?;
        match event {
            InstallEvent::DetectingOfficial => writeln!(
                writer,
                "{}",
                self.language
                    .text("Detecting official Codex...", "正在检测官方 Codex……")
            ),
            InstallEvent::DiscoveringCompatibility => {
                writeln!(
                    writer,
                    "{}",
                    self.language
                        .text("Discovering compatible releases...", "正在查找兼容版本……")
                )
            }
            InstallEvent::SelectingCompatibility => Ok(()),
            InstallEvent::SelectedCompatibility { compat_id } => writeln!(
                writer,
                "{}: {compat_id}",
                self.language
                    .text("Selected compatibility", "已选择兼容版本")
            ),
            InstallEvent::DownloadingReleaseMetadata => writeln!(
                writer,
                "{}",
                self.language
                    .text("Verifying release metadata...", "正在验证发布信息……")
            ),
            InstallEvent::ConnectingArtifact => writeln!(
                writer,
                "{}",
                self.language.text(
                    "Connecting to the patched Codex download...",
                    "正在连接补丁版 Codex 下载……"
                )
            ),
            InstallEvent::VerifyingArtifact => writeln!(
                writer,
                "{}",
                self.language
                    .text("Verifying downloaded artifact...", "正在验证下载的产物……")
            ),
            InstallEvent::Preparing => writeln!(
                writer,
                "{}",
                self.language
                    .text("Preparing patched Codex...", "正在准备补丁版 Codex……")
            ),
            InstallEvent::Activating => writeln!(
                writer,
                "{}",
                self.language
                    .text("Activating patched Codex...", "正在激活补丁版 Codex……")
            ),
            InstallEvent::Activated => writeln!(
                writer,
                "{}",
                self.language.text(
                    "Patched Codex shim activated.",
                    "补丁版 Codex 启动器已激活。"
                )
            ),
            InstallEvent::RollingBack => writeln!(
                writer,
                "{}",
                self.language
                    .text("Rolling back activation...", "正在回滚激活操作……")
            ),
            InstallEvent::PrioritizingCommand => {
                writeln!(
                    writer,
                    "{}",
                    self.language.text(
                        "Prioritizing and verifying the codex command; Windows may request administrator permission...",
                        "正在提高 codex 命令优先级并进行验证；Windows 可能会请求管理员权限……"
                    )
                )
            }
            InstallEvent::Completed => Ok(()),
            InstallEvent::ArtifactProgress { .. } => unreachable!(),
        }?;
        writer.flush()
    }

    fn finish_to(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        if self.mode == OutputMode::Human && self.line_active {
            self.line_active = false;
            writeln!(writer)?;
            writer.flush()?;
        }
        Ok(())
    }
}

struct PickerState<'a> {
    candidates: &'a [InstallCandidate],
    installed: Option<&'a str>,
    visible: Vec<usize>,
    selected: usize,
    viewport: usize,
    query: String,
    search_active: bool,
}

enum PickerDecision {
    Continue,
    Confirm(String),
    Cancel,
}

impl<'a> PickerState<'a> {
    fn new(candidates: &'a [InstallCandidate], installed: Option<&'a str>) -> Self {
        let visible: Vec<_> = (0..candidates.len()).collect();
        let selected = visible
            .iter()
            .position(|index| candidates[*index].recommended)
            .unwrap_or(0);
        let mut state = Self {
            candidates,
            installed,
            visible,
            selected,
            viewport: 0,
            query: String::new(),
            search_active: false,
        };
        state.keep_selected_visible();
        state
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.keep_selected_visible();
    }

    fn move_one(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = if delta < 0 {
            self.selected
                .checked_sub(1)
                .unwrap_or(self.visible.len() - 1)
        } else {
            (self.selected + 1) % self.visible.len()
        };
        self.keep_selected_visible();
    }

    fn home(&mut self) {
        if !self.visible.is_empty() {
            self.selected = 0;
            self.keep_selected_visible();
        }
    }

    fn end(&mut self) {
        if !self.visible.is_empty() {
            self.selected = self.visible.len() - 1;
            self.keep_selected_visible();
        }
    }

    fn begin_search(&mut self) {
        self.search_active = true;
        self.query.clear();
        self.rebuild_filter();
    }

    fn push_query(&mut self, character: char) {
        self.search_active = true;
        self.query.push(character);
        self.rebuild_filter();
    }

    fn backspace(&mut self) {
        if self.search_active {
            self.query.pop();
            self.rebuild_filter();
        }
    }

    fn escape(&mut self) -> PickerDecision {
        if self.search_active {
            self.search_active = false;
            self.query.clear();
            self.rebuild_filter();
            PickerDecision::Continue
        } else {
            PickerDecision::Cancel
        }
    }

    fn confirm(&self) -> PickerDecision {
        self.visible
            .get(self.selected)
            .map(|index| PickerDecision::Confirm(self.candidates[*index].compat_id.clone()))
            .unwrap_or(PickerDecision::Continue)
    }

    fn rebuild_filter(&mut self) {
        let query = self.query.to_ascii_lowercase();
        let mut ranked: Vec<_> = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                search_rank(candidate, &query).map(|rank| (index, rank))
            })
            .collect();
        ranked.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| {
                    self.candidates[right.0]
                        .patch_revision
                        .cmp(&self.candidates[left.0].patch_revision)
                })
                .then_with(|| {
                    self.candidates[left.0]
                        .compat_id
                        .cmp(&self.candidates[right.0].compat_id)
                })
        });
        self.visible = ranked.into_iter().map(|(index, _)| index).collect();
        self.selected = 0;
        self.viewport = 0;
    }

    fn keep_selected_visible(&mut self) {
        if self.selected < self.viewport {
            self.viewport = self.selected;
        } else if self.selected >= self.viewport + PICKER_ROWS {
            self.viewport = self.selected + 1 - PICKER_ROWS;
        }
    }

    fn render(&self, width: u16, language: Language) -> Vec<String> {
        let end = min(self.viewport + PICKER_ROWS, self.visible.len());
        let mut lines = vec![
            language
                .text(
                    "Select a patched Codex CLI version",
                    "选择补丁版 Codex CLI 版本",
                )
                .to_owned(),
            format!(
                "  {} {}",
                self.viewport,
                language.text("newer hidden", "个较新版本已隐藏")
            ),
        ];
        for row in 0..PICKER_ROWS {
            let position = self.viewport + row;
            let line = self
                .visible
                .get(position)
                .map_or_else(String::new, |index| {
                    let candidate = &self.candidates[*index];
                    let selected = if position == self.selected { '>' } else { ' ' };
                    let mut markers = Vec::new();
                    if candidate.recommended {
                        markers.push(language.text("Recommended", "推荐"));
                    }
                    if self.installed == Some(candidate.compat_id.as_str()) {
                        markers.push(language.text("Installed", "已安装"));
                    }
                    let suffix = if markers.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", markers.join("  "))
                    };
                    format!(
                        "{selected} p{:<3} Codex {:<8} {}{suffix}",
                        candidate.patch_revision, candidate.codex_version, candidate.recorded_on
                    )
                });
            lines.push(line);
        }
        lines.push(format!(
            "  {} {}",
            self.visible.len().saturating_sub(end),
            language.text("older hidden", "个较旧版本已隐藏")
        ));
        lines.push(if self.search_active {
            format!("{}: {}_", language.text("Search", "搜索"), self.query)
        } else {
            language
                .text("Search: / to filter", "搜索：按 / 筛选")
                .to_owned()
        });
        lines.push(
            self.visible
                .get(self.selected)
                .map(|index| format!("ID: {}", self.candidates[*index].compat_id))
                .unwrap_or_else(|| {
                    language
                        .text("No matching versions", "没有匹配的版本")
                        .to_owned()
                }),
        );
        lines.push(
            language
                .text(
                    "Up/Down move  PgUp/PgDn page  Enter install  Esc cancel",
                    "上/下移动  PgUp/PgDn 翻页  Enter 安装  Esc 取消",
                )
                .to_owned(),
        );
        lines
            .into_iter()
            .map(|line| line.chars().take(width as usize).collect())
            .collect()
    }
}

fn search_rank(candidate: &InstallCandidate, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let revision = format!("p{}", candidate.patch_revision);
    let fields = [
        revision.as_str(),
        candidate.compat_id.as_str(),
        candidate.codex_version.as_str(),
        candidate.recorded_on.as_str(),
    ];
    if revision == query {
        Some(0)
    } else if fields
        .iter()
        .any(|field| field.to_ascii_lowercase().starts_with(query))
    {
        Some(1)
    } else if fields
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(query))
    {
        Some(2)
    } else {
        None
    }
}

struct PickerTerminal {
    stderr: io::Stderr,
    frame: Vec<String>,
    active: bool,
}

impl PickerTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode()
            .map_err(|error| ManagerError::io("enable install picker raw mode", error))?;
        let mut stderr = io::stderr();
        if let Err(error) = execute!(stderr, Hide) {
            let _ = disable_raw_mode();
            return Err(ManagerError::io("hide install picker cursor", error));
        }
        Ok(Self {
            stderr,
            frame: Vec::new(),
            active: true,
        })
    }

    fn draw(&mut self, lines: &[String]) -> Result<()> {
        let update = picker_frame_update(&self.frame, lines)
            .map_err(|error| ManagerError::io("draw install picker", error))?;
        if !update.is_empty() {
            self.stderr
                .sync_update(|stderr| stderr.write_all(&update))
                .and_then(|result| result)
                .map_err(|error| ManagerError::io("draw install picker", error))?;
        }
        self.frame.clear();
        self.frame.extend_from_slice(lines);
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let clear_result = (|| {
            if !self.frame.is_empty() {
                let lines = self.frame.len() as u16;
                execute!(self.stderr, MoveUp(lines))?;
                for line in 0..lines {
                    execute!(self.stderr, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    if line + 1 < lines {
                        execute!(self.stderr, MoveDown(1))?;
                    }
                }
                if lines > 1 {
                    execute!(self.stderr, MoveUp(lines - 1))?;
                }
                self.stderr.flush()?;
            }
            Ok(())
        })();
        let cursor_result = execute!(self.stderr, Show);
        let raw_result = disable_raw_mode();
        clear_result.and(cursor_result).and(raw_result)
    }
}

fn picker_frame_update(previous: &[String], current: &[String]) -> io::Result<Vec<u8>> {
    if !previous.is_empty() && previous.len() != current.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "install picker frame height changed",
        ));
    }

    let mut update = Vec::new();
    if previous.is_empty() {
        for line in current {
            queue!(update, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            writeln!(update, "{line}")?;
        }
        return Ok(update);
    }

    let height = current.len() as u16;
    for (index, (old, new)) in previous.iter().zip(current).enumerate() {
        if old == new {
            continue;
        }
        let distance = height - index as u16;
        queue!(
            update,
            MoveUp(distance),
            MoveToColumn(0),
            Clear(ClearType::CurrentLine)
        )?;
        write!(update, "{new}")?;
        queue!(update, MoveDown(distance), MoveToColumn(0))?;
    }
    Ok(update)
}

impl Drop for PickerTerminal {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

pub(crate) fn pick_install_candidate(
    candidates: &[InstallCandidate],
    installed: Option<&str>,
) -> Result<String> {
    let language = Language::detected();
    let mut state = PickerState::new(candidates, installed);
    let mut terminal = PickerTerminal::enter()?;
    loop {
        let width = size().map(|(width, _)| width).unwrap_or(100).max(1);
        terminal.draw(&state.render(width, language))?;
        let Event::Key(key) =
            event::read().map_err(|error| ManagerError::io("read install picker input", error))?
        else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let decision = if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            PickerDecision::Cancel
        } else {
            match key.code {
                KeyCode::Up => {
                    state.move_one(-1);
                    PickerDecision::Continue
                }
                KeyCode::Down => {
                    state.move_one(1);
                    PickerDecision::Continue
                }
                KeyCode::PageUp => {
                    state.move_by(-(PICKER_ROWS as isize));
                    PickerDecision::Continue
                }
                KeyCode::PageDown => {
                    state.move_by(PICKER_ROWS as isize);
                    PickerDecision::Continue
                }
                KeyCode::Home => {
                    state.home();
                    PickerDecision::Continue
                }
                KeyCode::End => {
                    state.end();
                    PickerDecision::Continue
                }
                KeyCode::Enter => state.confirm(),
                KeyCode::Esc => state.escape(),
                KeyCode::Backspace => {
                    state.backspace();
                    PickerDecision::Continue
                }
                KeyCode::Char('/') => {
                    state.begin_search();
                    PickerDecision::Continue
                }
                KeyCode::Char(character)
                    if !character.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    state.push_query(character);
                    PickerDecision::Continue
                }
                _ => PickerDecision::Continue,
            }
        };
        match decision {
            PickerDecision::Continue => {}
            PickerDecision::Confirm(compat_id) => {
                terminal.leave().map_err(|error| {
                    ManagerError::io("restore terminal after install picker", error)
                })?;
                return Ok(compat_id);
            }
            PickerDecision::Cancel => {
                let _ = terminal.leave();
                return Err(ManagerError::new(
                    INSTALLATION_CANCELLED,
                    language.text("Installation cancelled.", "安装已取消。"),
                ));
            }
        }
    }
}

pub(crate) fn write_install_cancelled() -> i32 {
    let _ = writeln!(
        io::stderr().lock(),
        "{}",
        Language::detected().text("Installation cancelled.", "安装已取消。")
    );
    130
}

fn resolve_output_mode(explicit_json: bool, stdout_is_terminal: bool) -> OutputMode {
    if explicit_json || !stdout_is_terminal {
        OutputMode::Json
    } else {
        OutputMode::Human
    }
}

pub(crate) trait HumanReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()>;
}

pub(crate) fn write_report<T: HumanReport + Serialize>(mode: OutputMode, report: &T) -> Result<()> {
    let stdout = io::stdout();
    write_report_to(&mut stdout.lock(), mode, Language::detected(), report)
}

pub(crate) fn write_doctor_report(
    mode: OutputMode,
    report: &DoctorReport,
    status: Option<&StatusReport>,
) -> Result<i32> {
    let stdout = io::stdout();
    write_doctor_report_to(
        &mut stdout.lock(),
        mode,
        Language::detected(),
        report,
        status,
    )
}

pub(crate) fn write_doctor_error(mode: OutputMode, error: &ManagerError) -> i32 {
    let stderr = io::stderr();
    write_doctor_error_to(&mut stderr.lock(), mode, Language::detected(), error).unwrap_or(2)
}

fn write_report_to<T: HumanReport + Serialize>(
    writer: &mut dyn Write,
    mode: OutputMode,
    language: Language,
    report: &T,
) -> Result<()> {
    match mode {
        OutputMode::Json => write_json(writer, report),
        OutputMode::Human => report
            .write_human(writer, language)
            .map_err(|error| ManagerError::io("write human output", error)),
    }
}

fn write_doctor_report_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    language: Language,
    report: &DoctorReport,
    status: Option<&StatusReport>,
) -> Result<i32> {
    if mode == OutputMode::Json {
        write_json(writer, report)?;
        return Ok(0);
    }
    let status = status.ok_or_else(|| {
        ManagerError::new(
            "doctor_status_unavailable",
            "status data is required for Human doctor output",
        )
    })?;
    let assessment = doctor_assessment(report, status, language);
    write_doctor_assessment(writer, &assessment, language)
        .map_err(|error| ManagerError::io("write human doctor output", error))?;
    Ok(assessment.exit_code())
}

fn write_doctor_error_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    language: Language,
    error: &ManagerError,
) -> Result<i32> {
    let diagnosed = is_diagnosed_doctor_error(error.code);
    if mode == OutputMode::Json || !diagnosed {
        write_error_to(writer, mode, language, Operation::Doctor, error)?;
    } else {
        (|| -> io::Result<()> {
            writeln!(writer, "CSA doctor")?;
            writeln!(
                writer,
                "{} {}: {}",
                language.text("FAIL", "失败"),
                language.text("Official Codex", "官方 Codex"),
                error.message
            )?;
            writeln!(
                writer,
                "     {}: {}",
                language.text("Impact", "影响"),
                language.text(
                    "CSA cannot safely verify the official Codex installation.",
                    "CSA 无法安全验证官方 Codex 安装。"
                )
            )?;
            writeln!(
                writer,
                "     {}: {}",
                language.text("Safety", "安全性"),
                language.text(
                    "the official Codex installation was not modified.",
                    "官方 Codex 安装未被修改。"
                )
            )?;
            writeln!(
                writer,
                "     {}: {}",
                language.text("Next", "下一步"),
                recovery_hint(language, Operation::Doctor, error.code)
            )?;
            writeln!(
                writer,
                "{}",
                language.text(
                    "Summary: 0 passed, 0 warnings, 1 failed",
                    "汇总：0 项通过，0 项警告，1 项失败"
                )
            )
        })()
        .map_err(|error| ManagerError::io("write human doctor error", error))?;
    }
    Ok(if diagnosed { 1 } else { 2 })
}

pub(crate) fn write_error(mode: OutputMode, operation: Operation, error: &ManagerError) -> i32 {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    let _ = write_error_to(&mut lock, mode, Language::detected(), operation, error);
    2
}

fn write_error_to(
    writer: &mut dyn Write,
    mode: OutputMode,
    language: Language,
    operation: Operation,
    error: &ManagerError,
) -> Result<()> {
    match mode {
        OutputMode::Json => {
            let envelope = serde_json::json!({
                "schema": 1,
                "error": {
                    "code": error.code,
                    "message": error.message,
                }
            });
            write_json(writer, &envelope)
        }
        OutputMode::Human => write_human_error(writer, language, operation, error)
            .map_err(|error| ManagerError::io("write human error", error)),
    }
}

fn write_human_error(
    writer: &mut dyn Write,
    language: Language,
    operation: Operation,
    error: &ManagerError,
) -> io::Result<()> {
    writeln!(
        writer,
        "{} [{}] {}",
        language.text("ERROR", "错误"),
        error.code,
        error.message
    )?;
    writeln!(
        writer,
        "{}: {}",
        language.text("Safety", "安全性"),
        language.text(
            "the official Codex installation was not modified.",
            "官方 Codex 安装未被修改。"
        )
    )?;
    if operation == Operation::Install {
        let state = if error.code == "output_error" {
            language.text(
                "Installation may have completed; run `csa status --json` to confirm the active state.",
                "安装可能已经完成；请运行 `csa status --json` 确认激活状态。",
            )
        } else if matches!(
            error.code,
            "install_rollback_failed" | "path_activation_rollback_failed"
        ) {
            language.text(
                "Activation rollback did not complete; inspect CSA state before retrying.",
                "激活回滚未完成；重试前请检查 CSA 状态。",
            )
        } else {
            language.text(
                "The failed install did not keep a new CSA activation.",
                "失败的安装没有保留新的 CSA 激活状态。",
            )
        };
        writeln!(writer, "{}: {state}", language.text("State", "状态"))?;
    }
    writeln!(
        writer,
        "{}: {}",
        language.text("Recovery", "恢复建议"),
        recovery_hint(language, operation, error.code)
    )
}

fn write_json(writer: &mut dyn Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *writer, value).map_err(|error| {
        ManagerError::new("output_error", format!("serialize JSON output: {error}"))
    })?;
    writeln!(writer).map_err(|error| ManagerError::io("write JSON output", error))
}

fn recovery_hint(language: Language, operation: Operation, code: &str) -> &'static str {
    match code {
        "invalid_cli" => language.text(
            "Run `csa --help` and retry with valid options.",
            "请运行 `csa --help`，然后使用有效选项重试。",
        ),
        "official_not_found" => language.text(
            "Install official `@openai/codex` or put its launcher on PATH, then retry.",
            "请安装官方 `@openai/codex`，或将其启动器加入 PATH，然后重试。",
        ),
        "official_runtime_incomplete"
        | "official_runtime_ambiguous"
        | "official_version_mismatch" => language.text(
            "Reinstall the official `@openai/codex` package, then retry.",
            "请重新安装官方 `@openai/codex` 包，然后重试。",
        ),
        "official_in_manager_root" => language.text(
            "Use an official Codex installation outside the CSA manager root.",
            "请使用位于 CSA 管理目录之外的官方 Codex 安装。",
        ),
        "ambiguous_compatibility_revision" => language.text(
            "Retry with `csa install --compat <compat-id>` to select an exact release.",
            "请使用 `csa install --compat <compat-id>` 选择精确版本后重试。",
        ),
        "no_installable_compatibility_releases" => language.text(
            "Install an official Codex version with an exact CSA compatibility release, then retry.",
            "请安装存在精确 CSA 兼容版本的官方 Codex，然后重试。",
        ),
        "not_prepared" | "state_upgrade_required" => {
            language.text("Run `csa install` again.", "请重新运行 `csa install`。")
        }
        "unsupported_official_version" | "unsupported_build_target" => language.text(
            "Choose a release that exactly matches the installed official Codex and target.",
            "请选择与已安装官方 Codex 及目标平台精确匹配的版本。",
        ),
        "github_api_forbidden" | "network_error" => language.text(
            "Check the network or proxy, then retry the command.",
            "请检查网络或代理，然后重试该命令。",
        ),
        "path_precedence_conflict" => language.text(
            "A Windows policy still prevents CSA from taking command priority; ask an administrator to inspect the machine PATH, then run `csa plug` again.",
            "Windows 策略仍阻止 CSA 取得命令优先级；请让管理员检查系统 PATH，然后重新运行 `csa plug`。",
        ),
        "path_elevation_failed" => language.text(
            "Run the command again and approve the Windows administrator prompt.",
            "请重新运行该命令，并允许 Windows 管理员权限请求。",
        ),
        "path_activation_failed" | "path_activation_rollback_failed" => language.text(
            "Run `csa doctor --json`, inspect the activation result, then retry.",
            "请运行 `csa doctor --json` 检查激活结果，然后重试。",
        ),
        "install_rollback_failed" => language.text(
            "Run `csa doctor --json` before retrying installation.",
            "请先运行 `csa doctor --json`，再重试安装。",
        ),
        _ if matches!(operation, Operation::Doctor | Operation::Status) => language.text(
            "Retry with `--json` for machine-readable diagnostics.",
            "请使用 `--json` 重试以获取机器可读的诊断信息。",
        ),
        _ => language.text(
            "Run `csa doctor --json` for diagnostics, then retry.",
            "请运行 `csa doctor --json` 进行诊断，然后重试。",
        ),
    }
}

fn is_diagnosed_doctor_error(code: &str) -> bool {
    matches!(
        code,
        "official_not_found"
            | "official_runtime_incomplete"
            | "official_runtime_ambiguous"
            | "official_version_mismatch"
            | "official_in_manager_root"
    )
}

fn status_view(report: &StatusReport, language: Language) -> StatusView {
    let active = report.activation.status == "plugged" && report.activation.effective;
    let (conclusion, healthy) = match (
        report.status,
        report.activation.status,
        report.activation.effective,
    ) {
        ("unprepared", "fallback", _) => (
            language.text("not installed, activation fallback", "未安装，激活已回退"),
            false,
        ),
        ("unprepared", _, _) => (language.text("not installed", "未安装"), true),
        ("prepared", "unplugged", _) => {
            (language.text("installed, inactive", "已安装，未激活"), true)
        }
        ("prepared", "plugged", true) => (
            language.text("active and healthy", "已激活且状态正常"),
            true,
        ),
        ("prepared", "plugged", false) => (
            language.text("installed, activation ineffective", "已安装，激活未生效"),
            false,
        ),
        ("prepared", "fallback", _) => (
            language.text("installed, activation fallback", "已安装，激活已回退"),
            false,
        ),
        ("invalidated", _, _) => (
            language.text("installed state invalidated", "已安装状态已失效"),
            false,
        ),
        _ => (language.text("unknown state", "未知状态"), false),
    };
    StatusView {
        conclusion,
        installed: report.state.is_some(),
        active,
        healthy,
    }
}

fn doctor_assessment(
    report: &DoctorReport,
    status: &StatusReport,
    language: Language,
) -> DoctorAssessment {
    let mut checks = vec![
        check(
            CheckSeverity::Pass,
            language.text("Official Codex", "官方 Codex"),
            if language == Language::Chinese {
                format!(
                    "版本 {}，位置 {}",
                    report.official.version,
                    report.official.executable.path.display()
                )
            } else {
                format!(
                    "{} at {}",
                    report.official.version,
                    report.official.executable.path.display()
                )
            },
            None,
            None,
        ),
        check(
            CheckSeverity::Pass,
            language.text("Manager target", "管理器目标平台"),
            report.manager_build_target.to_owned(),
            None,
            None,
        ),
    ];

    if let Some(compatibility) = &report.compatibility {
        checks.push(if compatibility.exact_official_version {
            check(
                CheckSeverity::Pass,
                language.text("Compatibility version", "兼容版本"),
                if language == Language::Chinese {
                    format!("{} 与官方 Codex 匹配", compatibility.codex_version)
                } else {
                    format!("{} matches official Codex", compatibility.codex_version)
                },
                None,
                None,
            )
        } else {
            check(
                CheckSeverity::Fail,
                language.text("Compatibility version", "兼容版本"),
                if language == Language::Chinese {
                    format!(
                        "清单要求 {}，官方 Codex 为 {}",
                        compatibility.codex_version, report.official.version
                    )
                } else {
                    format!(
                        "manifest requires {}, official Codex is {}",
                        compatibility.codex_version, report.official.version
                    )
                },
                Some(language.text(
                    "This payload cannot safely patch the installed official Codex.",
                    "此载荷无法安全修补已安装的官方 Codex。",
                )),
                Some(language.text(
                    "Choose a compatibility release matching the official Codex version.",
                    "请选择与官方 Codex 版本匹配的兼容版本。",
                )),
            )
        });
        checks.push(if compatibility.supported_build_target {
            check(
                CheckSeverity::Pass,
                language.text("Compatibility target", "兼容目标平台"),
                compatibility.build_target.clone(),
                None,
                None,
            )
        } else {
            check(
                CheckSeverity::Fail,
                language.text("Compatibility target", "兼容目标平台"),
                if language == Language::Chinese {
                    format!(
                        "清单目标为 {}，管理器目标为 {}",
                        compatibility.build_target, report.manager_build_target
                    )
                } else {
                    format!(
                        "manifest targets {}, manager targets {}",
                        compatibility.build_target, report.manager_build_target
                    )
                },
                Some(language.text(
                    "This payload cannot run on the current CSA build target.",
                    "此载荷无法在当前 CSA 构建目标上运行。",
                )),
                Some(language.text(
                    "Choose a compatibility release matching the manager target.",
                    "请选择与管理器目标平台匹配的兼容版本。",
                )),
            )
        });
    }

    let mut incomplete = false;
    match (status.status, status.state.as_ref()) {
        ("unprepared", None) => checks.push(check(
            CheckSeverity::Warn,
            language.text("Prepared state", "准备状态"),
            language
                .text("no patched Codex is installed", "尚未安装补丁版 Codex")
                .to_owned(),
            Some(language.text(
                "The official Codex remains available, but CSA is not managing it.",
                "官方 Codex 仍可使用，但 CSA 尚未管理它。",
            )),
            Some(language.text("Run `csa install`.", "请运行 `csa install`。")),
        )),
        ("prepared", Some(state)) => checks.push(check(
            CheckSeverity::Pass,
            language.text("Prepared state", "准备状态"),
            if language == Language::Chinese {
                format!("{} 已验证", state.compat_id)
            } else {
                format!("{} is verified", state.compat_id)
            },
            None,
            None,
        )),
        ("invalidated", Some(_)) => checks.push(check(
            CheckSeverity::Fail,
            language.text("Prepared state", "准备状态"),
            status.reason.clone().unwrap_or_else(|| {
                language
                    .text("prepared state failed verification", "准备状态验证失败")
                    .to_owned()
            }),
            Some(language.text(
                "CSA will not trust or execute the prepared patched binary.",
                "CSA 不会信任或执行已准备的补丁版二进制文件。",
            )),
            Some(language.text(
                "Run `csa install` to replace the invalid state.",
                "请运行 `csa install` 替换失效状态。",
            )),
        )),
        _ => {
            incomplete = true;
            checks.push(check(
                CheckSeverity::Fail,
                language.text("Prepared state", "准备状态"),
                if language == Language::Chinese {
                    format!("无法识别的状态：{}", status.status)
                } else {
                    format!("unrecognized status: {}", status.status)
                },
                Some(language.text(
                    "CSA could not complete a reliable state assessment.",
                    "CSA 无法完成可靠的状态评估。",
                )),
                Some(language.text(
                    "Retry with `csa doctor --json` and inspect the raw report.",
                    "请使用 `csa doctor --json` 重试并检查原始报告。",
                )),
            ));
        }
    }

    match (status.activation.status, status.activation.effective) {
        ("unplugged", _) => checks.push(check(
            CheckSeverity::Warn,
            language.text("Activation", "激活"),
            language
                .text("patched Codex is inactive", "补丁版 Codex 未激活")
                .to_owned(),
            Some(language.text(
                "Running `codex` will not use the prepared patched binary.",
                "运行 `codex` 时不会使用已准备的补丁版二进制文件。",
            )),
            Some(if status.state.is_some() {
                language.text(
                    "Run `csa plug`, then open a new terminal.",
                    "请运行 `csa plug`，然后打开新终端。",
                )
            } else {
                language.text("Run `csa install`.", "请运行 `csa install`。")
            }),
        )),
        ("plugged", true) => checks.push(check(
            CheckSeverity::Pass,
            language.text("Activation", "激活"),
            language
                .text("patched Codex is active", "补丁版 Codex 已激活")
                .to_owned(),
            None,
            None,
        )),
        ("plugged", false) => checks.push(check(
            CheckSeverity::Fail,
            language.text("Activation", "激活"),
            language
                .text(
                    "the CSA shim exists but is not command-effective",
                    "CSA 启动器存在，但未对命令生效",
                )
                .to_owned(),
            Some(language.text(
                "Running `codex` selects another installation.",
                "运行 `codex` 时选择了其他安装。",
            )),
            Some(language.text(
                "Run `csa plug`, open a new terminal, then rerun `csa doctor`.",
                "请运行 `csa plug`，打开新终端，然后重新运行 `csa doctor`。",
            )),
        )),
        ("fallback", _) => checks.push(check(
            CheckSeverity::Fail,
            language.text("Activation", "激活"),
            status.activation.reason.clone().unwrap_or_else(|| {
                language
                    .text("activation entered fallback mode", "激活已进入回退模式")
                    .to_owned()
            }),
            Some(language.text(
                "CSA will fall back instead of trusting the patched activation.",
                "CSA 将执行回退，而不会信任补丁版激活状态。",
            )),
            Some(language.text(
                "Run `csa install` to rebuild and reactivate the patched Codex.",
                "请运行 `csa install`，重新构建并激活补丁版 Codex。",
            )),
        )),
        _ => {
            incomplete = true;
            checks.push(check(
                CheckSeverity::Fail,
                language.text("Activation", "激活"),
                if language == Language::Chinese {
                    format!("无法识别的激活状态：{}", status.activation.status)
                } else {
                    format!("unrecognized activation: {}", status.activation.status)
                },
                Some(language.text(
                    "CSA could not determine whether patched Codex is active.",
                    "CSA 无法确定补丁版 Codex 是否已激活。",
                )),
                Some(language.text(
                    "Retry with `csa doctor --json` and inspect the raw report.",
                    "请使用 `csa doctor --json` 重试并检查原始报告。",
                )),
            ));
        }
    }

    let resolved = report
        .command_resolution
        .resolved_codex
        .as_ref()
        .map_or_else(
            || language.text("not found", "未找到").to_owned(),
            |path| path.display().to_string(),
        );
    if matches!(status.activation.status, "plugged" | "fallback")
        && report.command_resolution.resolves_to_managed_shim
    {
        checks.push(check(
            CheckSeverity::Pass,
            language.text("Command precedence", "命令优先级"),
            if language == Language::Chinese {
                format!("codex 解析到 {resolved}")
            } else {
                format!("codex resolves to {resolved}")
            },
            None,
            None,
        ));
    } else if status.activation.status == "unplugged" {
        checks.push(check(
            CheckSeverity::Warn,
            language.text("Command precedence", "命令优先级"),
            if language == Language::Chinese {
                format!("codex 解析到 {resolved}")
            } else {
                format!("codex resolves to {resolved}")
            },
            Some(language.text(
                "The current command does not use CSA's patched Codex.",
                "当前命令没有使用 CSA 的补丁版 Codex。",
            )),
            Some(if status.state.is_some() {
                language.text(
                    "Run `csa plug`, then open a new terminal.",
                    "请运行 `csa plug`，然后打开新终端。",
                )
            } else {
                language.text("Run `csa install`.", "请运行 `csa install`。")
            }),
        ));
    } else if matches!(status.activation.status, "plugged" | "fallback") {
        checks.push(check(
            CheckSeverity::Fail,
            language.text("Command precedence", "命令优先级"),
            if language == Language::Chinese {
                format!("codex 解析到 {resolved}，而不是受管启动器")
            } else {
                format!("codex resolves to {resolved}, not the managed shim")
            },
            Some(language.text(
                "The patched Codex is not selected by the current command.",
                "当前命令没有选择补丁版 Codex。",
            )),
            Some(language.text(
                "Run `csa plug`, open a new terminal, then rerun `csa doctor`.",
                "请运行 `csa plug`，打开新终端，然后重新运行 `csa doctor`。",
            )),
        ));
    } else {
        incomplete = true;
        checks.push(check(
            CheckSeverity::Fail,
            language.text("Command precedence", "命令优先级"),
            if language == Language::Chinese {
                format!("无法评估 {resolved} 的命令解析")
            } else {
                format!("cannot assess command resolution for {resolved}")
            },
            Some(language.text(
                "CSA could not complete a reliable PATH assessment.",
                "CSA 无法完成可靠的 PATH 评估。",
            )),
            Some(language.text(
                "Retry with `csa doctor --json` and inspect the raw report.",
                "请使用 `csa doctor --json` 重试并检查原始报告。",
            )),
        ));
    }

    DoctorAssessment { checks, incomplete }
}

fn check(
    severity: CheckSeverity,
    name: &'static str,
    detail: String,
    impact: Option<&'static str>,
    action: Option<&'static str>,
) -> DoctorCheck {
    DoctorCheck {
        severity,
        name,
        detail,
        impact,
        action,
    }
}

impl DoctorAssessment {
    fn exit_code(&self) -> i32 {
        if self.incomplete {
            2
        } else if self
            .checks
            .iter()
            .any(|check| check.severity == CheckSeverity::Fail)
        {
            1
        } else {
            0
        }
    }
}

fn write_doctor_assessment(
    writer: &mut dyn Write,
    assessment: &DoctorAssessment,
    language: Language,
) -> io::Result<()> {
    writeln!(writer, "CSA doctor")?;
    let mut totals = [0_u32; 3];
    for check in &assessment.checks {
        let (label, index) = match check.severity {
            CheckSeverity::Pass => (language.text("PASS", "通过"), 0),
            CheckSeverity::Warn => (language.text("WARN", "警告"), 1),
            CheckSeverity::Fail => (language.text("FAIL", "失败"), 2),
        };
        totals[index] += 1;
        writeln!(writer, "{label} {}: {}", check.name, check.detail)?;
        if let Some(impact) = check.impact {
            writeln!(writer, "     {}: {impact}", language.text("Impact", "影响"))?;
        }
        if let Some(action) = check.action {
            writeln!(writer, "     {}: {action}", language.text("Next", "下一步"))?;
        }
    }
    if language == Language::Chinese {
        writeln!(
            writer,
            "汇总：{} 项通过，{} 项警告，{} 项失败",
            totals[0], totals[1], totals[2]
        )
    } else {
        writeln!(
            writer,
            "Summary: {} passed, {} warnings, {} failed",
            totals[0], totals[1], totals[2]
        )
    }
}

fn yes_no(value: bool, language: Language) -> &'static str {
    if value {
        language.text("yes", "是")
    } else {
        language.text("no", "否")
    }
}

impl HumanReport for PrepareReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        writeln!(
            writer,
            "{}",
            language.text("OK Patched Codex prepared", "成功：补丁版 Codex 已准备完成")
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Compatibility", "兼容版本"),
            self.state.compat_id
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Artifact", "产物"),
            self.state.artifact_path.display()
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Official Codex unchanged", "官方 Codex 未被修改"),
            yes_no(true, language)
        )
    }
}

impl HumanReport for InstallReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        writeln!(
            writer,
            "{}",
            language.text(
                "OK Patched Codex installed; activation is ready for new terminals",
                "成功：补丁版 Codex 已安装，新终端激活已就绪",
            )
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Compatibility", "兼容版本"),
            self.prepare.state.compat_id
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Managed command", "受管命令"),
            self.activation.activation.shim_path.display()
        )?;
        if let Some(user_path) = &self.activation.user_path {
            writeln!(
                writer,
                "{}: {}",
                language.text("User PATH", "用户 PATH"),
                user_path.status
            )?;
        }
        writeln!(
            writer,
            "{}: {}",
            language.text("Next", "下一步"),
            language.text(
                "close all terminals and terminal-hosting apps, reopen one, then run `where.exe codex` and `codex --version`.",
                "请关闭所有终端及承载终端的应用，重新打开后运行 `where.exe codex` 和 `codex --version`。",
            )
        )
    }
}

impl HumanReport for UninstallReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        let message = if self.changed {
            language.text("OK CSA uninstalled", "成功：CSA 已卸载")
        } else {
            language.text("OK CSA was already uninstalled", "成功：CSA 已处于卸载状态")
        };
        writeln!(writer, "{message}")?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Manager root", "管理器目录"),
            self.manager_root.display()
        )
    }
}

impl HumanReport for PlugReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        let message = if self.changed {
            language.text(
                "OK Patched Codex activation is ready for new terminals",
                "成功：补丁版 Codex 的新终端激活已就绪",
            )
        } else {
            language.text(
                "OK Patched Codex activation was already ready for new terminals",
                "成功：补丁版 Codex 的新终端激活已就绪",
            )
        };
        writeln!(writer, "{message}")?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Managed command", "受管命令"),
            self.activation.shim_path.display()
        )?;
        if let Some(user_path) = &self.user_path {
            writeln!(
                writer,
                "{}: {}",
                language.text("User PATH", "用户 PATH"),
                user_path.status
            )?;
        }
        writeln!(
            writer,
            "{}: {}",
            language.text("Next", "下一步"),
            language.text(
                "close all terminals and terminal-hosting apps, reopen one, then run `where.exe codex` and `codex --version`.",
                "请关闭所有终端及承载终端的应用，重新打开后运行 `where.exe codex` 和 `codex --version`。",
            )
        )
    }
}

impl HumanReport for UnplugReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        let message = if self.changed {
            language.text("OK Patched Codex deactivated", "成功：补丁版 Codex 已停用")
        } else {
            language.text(
                "OK Patched Codex was already inactive",
                "成功：补丁版 Codex 已处于停用状态",
            )
        };
        writeln!(writer, "{message}")?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Managed bin", "受管二进制目录"),
            self.managed_bin.display()
        )
    }
}

impl HumanReport for PurgeReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        let message = if self.changed {
            language.text("OK CSA managed data removed", "成功：CSA 受管数据已删除")
        } else {
            language.text(
                "OK CSA had no managed data to remove",
                "成功：CSA 没有需要删除的受管数据",
            )
        };
        writeln!(writer, "{message}")?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Manager root", "管理器目录"),
            self.manager_root.display()
        )
    }
}

impl HumanReport for StatusReport {
    fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
        let view = status_view(self, language);
        writeln!(
            writer,
            "{}: {}",
            language.text("CSA status", "CSA 状态"),
            view.conclusion
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Installed", "已安装"),
            yes_no(view.installed, language)
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Active", "已激活"),
            yes_no(view.active, language)
        )?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Healthy", "状态正常"),
            yes_no(view.healthy, language)
        )?;
        match &self.state {
            Some(state) => {
                writeln!(
                    writer,
                    "{}: {}",
                    language.text("Official Codex", "官方 Codex"),
                    state.official.version
                )?;
                writeln!(
                    writer,
                    "{}: {}",
                    language.text("Patched Codex", "补丁版 Codex"),
                    state.compat_id
                )?;
            }
            None => {
                writeln!(
                    writer,
                    "{}: {}",
                    language.text("Official Codex", "官方 Codex"),
                    language.text("not recorded", "未记录")
                )?;
                writeln!(
                    writer,
                    "{}: {}",
                    language.text("Patched Codex", "补丁版 Codex"),
                    language.text("not installed", "未安装")
                )?;
            }
        }
        write_resolution(writer, language, &self.activation.command_resolution)?;
        writeln!(
            writer,
            "{}: {}",
            language.text("Activation detail", "激活详情"),
            self.activation.status
        )?;
        if let Some(reason) = &self.reason {
            writeln!(writer, "{}: {reason}", language.text("Reason", "原因"))?;
        }
        Ok(())
    }
}

fn write_resolution(
    writer: &mut dyn Write,
    language: Language,
    resolution: &CommandResolution,
) -> io::Result<()> {
    match &resolution.resolved_codex {
        Some(path) => writeln!(
            writer,
            "{}: {}",
            language.text("Codex command", "Codex 命令"),
            path.display()
        ),
        None => writeln!(
            writer,
            "{}: {}",
            language.text("Codex command", "Codex 命令"),
            language.text("not found", "未找到")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HumanReport, InstallProgress, Operation, OutputMode, PickerDecision, PickerState,
        picker_frame_update, resolve_output_mode, write_doctor_error_to, write_doctor_report_to,
        write_error_to, write_report_to,
    };
    use csa::activation::{ActivationReport, CommandResolution};
    use csa::detect::{FileFingerprint, OfficialCodex};
    use csa::i18n::Language;
    use csa::manager::{CompatibilityReport, DoctorReport, InstallEvent, StatusReport};
    use csa::online::InstallCandidate;
    use csa::state::PreparedState;
    use serde::Serialize;
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[derive(Serialize)]
    struct Report {
        schema: u32,
        status: &'static str,
    }

    impl HumanReport for Report {
        fn write_human(&self, writer: &mut dyn Write, language: Language) -> io::Result<()> {
            writeln!(writer, "{} {}", language.text("OK", "成功"), self.status)
        }
    }

    fn official() -> OfficialCodex {
        OfficialCodex {
            executable: FileFingerprint {
                path: PathBuf::from("C:/official/codex.exe"),
                sha256: "a".repeat(64),
                size: 10,
            },
            version: "0.150.1".to_owned(),
            native: None,
            runtime: None,
        }
    }

    fn prepared() -> PreparedState {
        PreparedState {
            schema: 2,
            compat_id: "rust-v0.150.1-native-join-p10".to_owned(),
            manifest_path: PathBuf::from("C:/csa/manifest.toml"),
            build_target: "x86_64-pc-windows-msvc".to_owned(),
            artifact_path: PathBuf::from("C:/csa/patched-codex.exe"),
            artifact_sha256: "b".repeat(64),
            artifact_size: 20,
            official: official(),
            prepared_at_unix_seconds: 1,
        }
    }

    fn status_fixture(
        status: &'static str,
        activation: &'static str,
        effective: bool,
        installed: bool,
    ) -> StatusReport {
        let command_resolution = CommandResolution {
            managed_bin_on_path: effective,
            resolved_codex: Some(if effective {
                PathBuf::from("C:/csa/bin/codex.exe")
            } else {
                PathBuf::from("C:/official/codex.exe")
            }),
            resolves_to_managed_shim: effective,
        };
        StatusReport {
            schema: 1,
            status,
            manager_root: PathBuf::from("C:/csa"),
            state: installed.then(prepared),
            reason: (status == "invalidated").then(|| "artifact_invalidated: changed".to_owned()),
            activation: ActivationReport {
                status: activation,
                effective,
                managed_bin: PathBuf::from("C:/csa/bin"),
                shim_path: PathBuf::from("C:/csa/bin/codex.exe"),
                command_resolution,
                state: None,
                reason: (activation == "fallback")
                    .then(|| "activation_fallback: invalid state".to_owned()),
            },
        }
    }

    fn doctor_fixture(resolves_to_managed_shim: bool) -> DoctorReport {
        DoctorReport {
            schema: 1,
            manager_root: PathBuf::from("C:/csa"),
            manager_build_target: "x86_64-pc-windows-msvc",
            official: official(),
            command_resolution: CommandResolution {
                managed_bin_on_path: resolves_to_managed_shim,
                resolved_codex: Some(if resolves_to_managed_shim {
                    PathBuf::from("C:/csa/bin/codex.exe")
                } else {
                    PathBuf::from("C:/official/codex.exe")
                }),
                resolves_to_managed_shim,
            },
            compatibility: None,
        }
    }

    fn render_status(report: &StatusReport) -> String {
        render_status_in(report, Language::English)
    }

    fn render_status_in(report: &StatusReport, language: Language) -> String {
        let mut output = Vec::new();
        report.write_human(&mut output, language).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn render_doctor(report: &DoctorReport, status: &StatusReport) -> (i32, String) {
        render_doctor_in(report, status, Language::English)
    }

    fn render_doctor_in(
        report: &DoctorReport,
        status: &StatusReport,
        language: Language,
    ) -> (i32, String) {
        let mut output = Vec::new();
        let exit_code = write_doctor_report_to(
            &mut output,
            OutputMode::Human,
            language,
            report,
            Some(status),
        )
        .unwrap();
        (exit_code, String::from_utf8(output).unwrap())
    }

    #[test]
    fn status_views_cover_all_manager_states_without_changing_json() {
        for (report, expected) in [
            (
                status_fixture("unprepared", "unplugged", false, false),
                "CSA status: not installed\nInstalled: no\nActive: no\nHealthy: yes\n",
            ),
            (
                status_fixture("unprepared", "fallback", false, false),
                "CSA status: not installed, activation fallback\nInstalled: no\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("prepared", "unplugged", false, true),
                "CSA status: installed, inactive\nInstalled: yes\nActive: no\nHealthy: yes\n",
            ),
            (
                status_fixture("prepared", "plugged", true, true),
                "CSA status: active and healthy\nInstalled: yes\nActive: yes\nHealthy: yes\n",
            ),
            (
                status_fixture("prepared", "plugged", false, true),
                "CSA status: installed, activation ineffective\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("prepared", "fallback", false, true),
                "CSA status: installed, activation fallback\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
            (
                status_fixture("invalidated", "fallback", false, true),
                "CSA status: installed state invalidated\nInstalled: yes\nActive: no\nHealthy: no\n",
            ),
        ] {
            assert!(render_status(&report).starts_with(expected));
        }

        let report = status_fixture("prepared", "plugged", true, true);
        let mut output = Vec::new();
        write_report_to(&mut output, OutputMode::Json, Language::Chinese, &report).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "activation",
                "manager_root",
                "reason",
                "schema",
                "state",
                "status"
            ]
        );
    }

    #[test]
    fn doctor_checks_actions_json_and_exit_codes_are_stable() {
        let active = status_fixture("prepared", "plugged", true, true);
        let (exit_code, output) = render_doctor(&doctor_fixture(true), &active);
        assert_eq!(exit_code, 0);
        assert!(output.contains("Summary: 5 passed, 0 warnings, 0 failed"));

        for (doctor, status, exit_code, summary) in [
            (
                doctor_fixture(false),
                status_fixture("unprepared", "unplugged", false, false),
                0,
                "Summary: 2 passed, 3 warnings, 0 failed",
            ),
            (
                doctor_fixture(false),
                status_fixture("prepared", "unplugged", false, true),
                0,
                "Summary: 3 passed, 2 warnings, 0 failed",
            ),
            (
                doctor_fixture(false),
                status_fixture("prepared", "fallback", false, true),
                1,
                "Summary: 3 passed, 0 warnings, 2 failed",
            ),
        ] {
            let (actual_exit, output) = render_doctor(&doctor, &status);
            assert_eq!(actual_exit, exit_code);
            assert!(output.contains(summary));
            let problems = output.matches("WARN ").count() + output.matches("FAIL ").count();
            assert_eq!(output.matches("Impact:").count(), problems);
            assert_eq!(output.matches("Next:").count(), problems);
        }

        let mut incompatible = doctor_fixture(true);
        incompatible.compatibility = Some(CompatibilityReport {
            compat_id: "wrong".to_owned(),
            manifest_path: PathBuf::from("C:/wrong/manifest.toml"),
            codex_version: "0.1.0".to_owned(),
            build_target: "wrong-target".to_owned(),
            exact_official_version: false,
            supported_build_target: false,
        });
        let (exit_code, output) = render_doctor(&incompatible, &active);
        assert_eq!(exit_code, 1);
        assert!(output.contains("Summary: 5 passed, 0 warnings, 2 failed"));

        let fallback = status_fixture("prepared", "fallback", false, true);
        let (exit_code, output) = render_doctor(&doctor_fixture(true), &fallback);
        assert_eq!(exit_code, 1);
        assert!(output.contains("PASS Command precedence:"));
        assert!(output.contains("Summary: 4 passed, 0 warnings, 1 failed"));

        let unknown = status_fixture("future", "future", false, true);
        assert_eq!(render_doctor(&doctor_fixture(false), &unknown).0, 2);

        let mut output = Vec::new();
        assert_eq!(
            write_doctor_report_to(
                &mut output,
                OutputMode::Json,
                Language::Chinese,
                &doctor_fixture(true),
                None,
            )
            .unwrap(),
            0
        );
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "command_resolution",
                "compatibility",
                "manager_build_target",
                "manager_root",
                "official",
                "schema"
            ]
        );

        let diagnosed = csa::error::ManagerError::new("official_not_found", "missing");
        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(
                &mut output,
                OutputMode::Human,
                Language::English,
                &diagnosed,
            )
            .unwrap(),
            1
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("FAIL Official Codex: missing"));
        assert!(output.contains("Safety: the official Codex installation was not modified."));
        assert!(output.contains("Summary: 0 passed, 0 warnings, 1 failed"));

        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(&mut output, OutputMode::Json, Language::Chinese, &diagnosed,)
                .unwrap(),
            1
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"error\": {\n    \"code\": \"official_not_found\",\n    \"message\": \"missing\"\n  },\n  \"schema\": 1\n}\n"
        );

        let unknown = csa::error::ManagerError::new("io_error", "unreadable");
        let mut output = Vec::new();
        assert_eq!(
            write_doctor_error_to(&mut output, OutputMode::Human, Language::English, &unknown,)
                .unwrap(),
            2
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("ERROR [io_error] unreadable")
        );
    }

    #[test]
    fn install_picker_filters_navigates_and_marks_the_current_version() {
        let candidates: Vec<_> = (1..=8)
            .rev()
            .map(|revision| {
                let compat_id = format!("rust-v0.150.1-native-join-p{revision}");
                InstallCandidate {
                    repository: "DSLZL/CSA-codex".to_owned(),
                    compat_id: compat_id.clone(),
                    codex_version: "0.150.1".to_owned(),
                    build_target: "x86_64-pc-windows-msvc".to_owned(),
                    patch_revision: revision,
                    recorded_on: format!("2026-08-{:02}", 20 + revision),
                    recommended: revision == 8,
                    release_tag: format!("compat-{compat_id}"),
                    release_commit: "a".repeat(40),
                }
            })
            .collect();
        let installed = candidates[2].compat_id.clone();
        let mut state = PickerState::new(&candidates, Some(&installed));
        assert!(matches!(state.confirm(), PickerDecision::Confirm(ref id) if id.ends_with("p8")));
        state.move_one(-1);
        assert_eq!(state.selected, 7);
        assert_eq!(state.viewport, 3);
        state.move_one(1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.viewport, 0);
        let rendered = state.render(200, Language::English);
        assert_eq!(rendered.len(), 11);
        assert_eq!(rendered[2..7].len(), 5);
        assert!(rendered.iter().any(|line| line.contains("Recommended")));
        let chinese = state.render(200, Language::Chinese);
        assert_eq!(chinese[0], "选择补丁版 Codex CLI 版本");
        assert!(chinese.iter().any(|line| line.contains("推荐")));
        assert!(chinese.iter().any(|line| line.contains("已安装")));
        assert!(chinese.last().unwrap().contains("Enter 安装"));
        state.move_by(2);
        assert!(
            state
                .render(200, Language::English)
                .iter()
                .any(|line| line.contains("Installed"))
        );
        state.end();
        assert_eq!(state.selected, 7);
        assert_eq!(state.viewport, 3);
        state.home();
        assert_eq!(state.selected, 0);
        state.move_by(5);
        assert_eq!(state.selected, 5);
        assert_eq!(state.viewport, 1);
        assert!(
            state
                .render(20, Language::English)
                .iter()
                .all(|line| line.chars().count() <= 20)
        );

        state.begin_search();
        state.push_query('p');
        state.push_query('3');
        assert_eq!(state.visible.len(), 1);
        assert!(matches!(state.confirm(), PickerDecision::Confirm(ref id) if id.ends_with("p3")));
        state.backspace();
        assert_eq!(state.visible.len(), 8);
        assert!(matches!(state.escape(), PickerDecision::Continue));
        assert!(matches!(state.escape(), PickerDecision::Cancel));

        state.push_query('z');
        assert!(state.visible.is_empty());
        assert!(matches!(state.confirm(), PickerDecision::Continue));
    }

    #[test]
    fn install_picker_redraw_skips_unchanged_lines() {
        let first = vec![
            "Select a patched Codex CLI version".to_owned(),
            "> p9".to_owned(),
            "Enter install".to_owned(),
        ];
        let initial = String::from_utf8(picker_frame_update(&[], &first).unwrap()).unwrap();
        assert!(initial.contains(&first[0]));
        assert!(picker_frame_update(&first, &first).unwrap().is_empty());

        let mut next = first.clone();
        next[1] = "> p8".to_owned();
        let incremental = String::from_utf8(picker_frame_update(&first, &next).unwrap()).unwrap();
        assert!(incremental.contains("> p8"));
        assert!(!incremental.contains(&first[0]));
        assert!(!incremental.contains(&first[2]));
    }

    #[test]
    fn output_mode_and_json_contract_are_stable() {
        assert_eq!(resolve_output_mode(false, true), OutputMode::Human);
        assert_eq!(resolve_output_mode(false, false), OutputMode::Json);
        assert_eq!(resolve_output_mode(true, true), OutputMode::Json);

        let mut output = Vec::new();
        write_report_to(
            &mut output,
            OutputMode::Json,
            Language::Chinese,
            &Report {
                schema: 1,
                status: "prepared",
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"schema\": 1,\n  \"status\": \"prepared\"\n}\n"
        );

        let mut output = Vec::new();
        write_report_to(
            &mut output,
            OutputMode::Human,
            Language::English,
            &Report {
                schema: 1,
                status: "prepared",
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "OK prepared\n");

        let error = csa::error::ManagerError::new("invalid_cli", "bad option");
        let mut output = Vec::new();
        write_error_to(
            &mut output,
            OutputMode::Json,
            Language::Chinese,
            Operation::Parse,
            &error,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\n  \"error\": {\n    \"code\": \"invalid_cli\",\n    \"message\": \"bad option\"\n  },\n  \"schema\": 1\n}\n"
        );

        let mut output = Vec::new();
        write_error_to(
            &mut output,
            OutputMode::Human,
            Language::English,
            Operation::Parse,
            &error,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("ERROR [invalid_cli] bad option"));
        assert!(output.contains("Safety:"));
        assert!(output.contains("Recovery:"));

        let start = Instant::now();
        let mut progress = InstallProgress::new(OutputMode::Human, false);
        progress.language = Language::English;
        let mut output = Vec::new();
        progress
            .write_event_to(&mut output, InstallEvent::DetectingOfficial, start)
            .unwrap();
        progress
            .write_event_to(&mut output, InstallEvent::DownloadingReleaseMetadata, start)
            .unwrap();
        progress
            .write_event_to(&mut output, InstallEvent::ConnectingArtifact, start)
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 1024 * 1024,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_secs(1),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 1024 * 1024 + 1,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_millis(1_050),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 2 * 1024 * 1024,
                    total_bytes: 2 * 1024 * 1024,
                },
                start + Duration::from_secs(2),
            )
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::VerifyingArtifact,
                start + Duration::from_secs(2),
            )
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("Downloading patched Codex").count(), 2);
        assert!(output.contains(" 50%"));
        assert!(output.contains("100%"));
        assert!(output.contains("Verifying release metadata..."));
        assert!(output.contains("Connecting to the patched Codex download..."));
        assert!(output.contains("Verifying downloaded artifact..."));

        let mut progress = InstallProgress::new(OutputMode::Json, false);
        let mut output = Vec::new();
        progress
            .write_event_to(&mut output, InstallEvent::DetectingOfficial, start)
            .unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn chinese_human_reports_errors_and_progress_use_simplified_chinese() {
        let status = status_fixture("prepared", "plugged", true, true);
        let output = render_status_in(&status, Language::Chinese);
        assert!(
            output
                .starts_with("CSA 状态: 已激活且状态正常\n已安装: 是\n已激活: 是\n状态正常: 是\n")
        );
        assert!(output.contains("Codex 命令: C:/csa/bin/codex.exe"));

        let (exit_code, output) =
            render_doctor_in(&doctor_fixture(true), &status, Language::Chinese);
        assert_eq!(exit_code, 0);
        assert!(output.contains("通过 官方 Codex:"));
        assert!(output.contains("汇总：5 项通过，0 项警告，0 项失败"));

        let error = csa::error::ManagerError::new("invalid_cli", "bad option");
        let mut output = Vec::new();
        write_error_to(
            &mut output,
            OutputMode::Human,
            Language::Chinese,
            Operation::Parse,
            &error,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("错误 [invalid_cli] bad option"));
        assert!(output.contains("安全性:"));
        assert!(output.contains("恢复建议:"));

        let start = Instant::now();
        let mut progress = InstallProgress::new(OutputMode::Human, false);
        progress.language = Language::Chinese;
        let mut output = Vec::new();
        progress
            .write_event_to(&mut output, InstallEvent::DetectingOfficial, start)
            .unwrap();
        progress
            .write_event_to(&mut output, InstallEvent::DownloadingReleaseMetadata, start)
            .unwrap();
        progress
            .write_event_to(
                &mut output,
                InstallEvent::ArtifactProgress {
                    downloaded_bytes: 1,
                    total_bytes: 2,
                },
                start + Duration::from_secs(1),
            )
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("正在检测官方 Codex……"));
        assert!(output.contains("正在验证发布信息……"));
        assert!(output.contains("正在下载补丁版 Codex"));
    }
}
