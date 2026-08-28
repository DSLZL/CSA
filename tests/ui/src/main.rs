mod orbit;

use std::env;
use std::io;
use std::io::Write as _;
use std::time::Duration;
use std::time::Instant;

use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::terminal;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;

const ROW_BG_A: (u8, u8, u8) = (18, 19, 26);
const ROW_BG_B: (u8, u8, u8) = (22, 24, 33);
const ORBIT_FG: (u8, u8, u8) = (232, 234, 241);
const KITTY_IMAGE_ID_START: u32 = 0x4353_4100;
const NORMAL_FLOW_STEP: Duration = Duration::from_millis(1_200);
const NORMAL_FLOW_STAGE_COUNT: u128 = 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentStatus {
    Starting,
    Running,
    WaitingApproval,
    Waiting,
    Failed,
    Cancelled,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivityStatus {
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Declined,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GraphicsMode {
    Text,
    Sixel,
    Kitty,
}

impl GraphicsMode {
    fn detected() -> Self {
        if env::var_os("KITTY_WINDOW_ID").is_some()
            || env::var_os("WEZTERM_EXECUTABLE").is_some()
            || env::var_os("WEZTERM_VERSION").is_some()
            || env_contains_any("TERM", &["kitty", "ghostty", "wezterm"])
            || env_contains_any("TERM_PROGRAM", &["kitty", "ghostty", "wezterm", "vscode"])
        {
            Self::Kitty
        } else if env::var_os("WT_SESSION").is_some()
            || env_contains_any("TERM", &["sixel", "mlterm", "foot"])
        {
            Self::Sixel
        } else {
            Self::Text
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Text => Self::Sixel,
            Self::Sixel => Self::Kitty,
            Self::Kitty => Self::Text,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Sixel => "Sixel",
            Self::Kitty => "Kitty",
        }
    }
}

fn env_contains_any(name: &str, needles: &[&str]) -> bool {
    env::var(name).is_ok_and(|value| {
        let value = value.to_ascii_lowercase();
        needles.iter().any(|needle| value.contains(needle))
    })
}

#[derive(Clone, Copy)]
struct Activity {
    label: &'static str,
    status: ActivityStatus,
}

#[derive(Clone, Copy)]
struct Agent<'a> {
    label: &'static str,
    status: AgentStatus,
    activities: &'a [Activity],
    terminal: Option<&'static str>,
    phase_ms: u64,
}

const BUILD_WORK: &[Activity] = &[
    Activity {
        label: "read src/subagent_live_panel.rs",
        status: ActivityStatus::Succeeded,
    },
    Activity {
        label: "cargo test --all-targets",
        status: ActivityStatus::Running,
    },
    Activity {
        label: "inspect transparent Orbit raster",
        status: ActivityStatus::Failed,
    },
];
const APPROVAL_WORK: &[Activity] = &[Activity {
    label: "Remove cache",
    status: ActivityStatus::WaitingApproval,
}];
const STOPPED_WORK: &[Activity] = &[
    Activity {
        label: "decline destructive command",
        status: ActivityStatus::Declined,
    },
    Activity {
        label: "cleanup temporary build",
        status: ActivityStatus::Cancelled,
    },
];
const NORMAL_FLOW_WORK: [Activity; 3] = [
    Activity {
        label: "inspect repository",
        status: ActivityStatus::Succeeded,
    },
    Activity {
        label: "edit Orbit renderer",
        status: ActivityStatus::Succeeded,
    },
    Activity {
        label: "test lightweight UI",
        status: ActivityStatus::Succeeded,
    },
];

const ALL_STATES: &[Agent<'static>] = &[
    Agent {
        label: "Builder（starting）",
        status: AgentStatus::Starting,
        activities: &[],
        terminal: None,
        phase_ms: 0,
    },
    Agent {
        label: "Robie（running）",
        status: AgentStatus::Running,
        activities: BUILD_WORK,
        terminal: None,
        phase_ms: 220,
    },
    Agent {
        label: "Reviewer（waiting_approval）",
        status: AgentStatus::WaitingApproval,
        activities: APPROVAL_WORK,
        terminal: None,
        phase_ms: 0,
    },
    Agent {
        label: "Observer（waiting）",
        status: AgentStatus::Waiting,
        activities: STOPPED_WORK,
        terminal: None,
        phase_ms: 0,
    },
    Agent {
        label: "Finisher（completed）",
        status: AgentStatus::Completed,
        activities: &[],
        terminal: Some("Done (19 tool uses ⋅ 32.0k tokens ⋅ 2m 12.2s)"),
        phase_ms: 0,
    },
    Agent {
        label: "Tester（failed）",
        status: AgentStatus::Failed,
        activities: &[],
        terminal: Some("Failed"),
        phase_ms: 0,
    },
    Agent {
        label: "Cleaner（cancelled）",
        status: AgentStatus::Cancelled,
        activities: &[],
        terminal: Some("Cancelled"),
        phase_ms: 0,
    },
];

struct NormalFlowFrame {
    status: AgentStatus,
    activities: [Activity; 3],
    visible: usize,
    terminal: Option<&'static str>,
}

fn normal_flow_frame(elapsed: Duration) -> NormalFlowFrame {
    let stage = usize::try_from(
        (elapsed.as_millis() / NORMAL_FLOW_STEP.as_millis()) % NORMAL_FLOW_STAGE_COUNT,
    )
    .expect("normal flow stage is bounded");
    let mut activities = NORMAL_FLOW_WORK;
    if (1..=3).contains(&stage) {
        activities[stage - 1].status = ActivityStatus::Running;
    }
    let (status, visible, terminal) = match stage {
        0 => (AgentStatus::Starting, 0, None),
        1..=3 => (AgentStatus::Running, stage, None),
        4 => (AgentStatus::Running, activities.len(), None),
        _ => (
            AgentStatus::Completed,
            0,
            Some("Done (3 tool uses ⋅ 4.8k tokens ⋅ 6.0s)"),
        ),
    };
    NormalFlowFrame {
        status,
        activities,
        visible,
        terminal,
    }
}

struct App {
    reduced_motion: bool,
    graphics: GraphicsMode,
    started_at: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            reduced_motion: false,
            graphics: GraphicsMode::detected(),
            started_at: Instant::now(),
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('m') => self.reduced_motion = !self.reduced_motion,
            KeyCode::Char('g') => self.graphics = self.graphics.next(),
            KeyCode::Char('q') | KeyCode::Esc => return true,
            _ => {}
        }
        false
    }
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    x: u16,
    y: u16,
    phase_ms: u64,
    background: (u8, u8, u8),
    graphics: GraphicsMode,
    image_id: u32,
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = run(&mut terminal, &mut App::new());
    let restore = ratatui::try_restore();
    restore?;
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut placements = Vec::new();
    let result = run_loop(terminal, app, &mut placements);
    let cleanup = clear_orbits(&placements);
    result.and(cleanup)
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    placements: &mut Vec<Placement>,
) -> io::Result<()> {
    loop {
        clear_orbits(placements)?;
        let mut next = Vec::new();
        let elapsed = app.started_at.elapsed();
        terminal.draw(|frame| next = render(frame, app, elapsed))?;
        if raster_layer_needs_redraw(placements, &next) {
            terminal.clear()?;
            next.clear();
            terminal.draw(|frame| next = render(frame, app, elapsed))?;
        }
        *placements = next;
        draw_orbits(placements, elapsed)?;

        if event::poll(orbit::RENDER_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let previous = (app.reduced_motion, app.graphics);
            let quit = app.handle_key(key.code);
            let changed = previous != (app.reduced_motion, app.graphics);
            let had_raster = !placements.is_empty();
            if had_raster && (changed || quit) {
                clear_orbits(placements)?;
                placements.clear();
                terminal.clear()?;
            }
            if quit {
                return Ok(());
            }
        }
    }
}

fn render(frame: &mut Frame<'_>, app: &App, elapsed: Duration) -> Vec<Placement> {
    let screen = frame.area();
    if screen.is_empty() {
        return Vec::new();
    }
    paint_rows(frame, screen);
    let padding = u16::from(screen.width > 2);
    let content = Rect::new(
        screen.x + padding,
        screen.y,
        screen.width.saturating_sub(padding * 2),
        screen.height,
    );
    render_line(
        frame,
        content,
        0,
        Line::from(vec![
            "Subagent Live".bold(),
            "  ·  states + normal flow".cyan(),
        ]),
    );
    let gallery = Rect::new(
        content.x,
        content.y.saturating_add(1),
        content.width,
        content.height.saturating_sub(2),
    );
    let placements = render_gallery(frame, gallery, app, elapsed);
    if content.height > 1 {
        render_line(
            frame,
            content,
            content.height - 1,
            Line::from(format!(
                "m motion:{} · g graphics:{} · q quit",
                if app.reduced_motion {
                    "reduced"
                } else {
                    "animated"
                },
                app.graphics.label()
            ))
            .dim(),
        );
    }
    placements
}

fn paint_rows(frame: &mut Frame<'_>, area: Rect) {
    for y in area.y..area.bottom() {
        frame.render_widget(
            Block::new().style(background_style(row_background(y))),
            Rect::new(area.x, y, area.width, 1),
        );
    }
}

fn render_gallery(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    elapsed: Duration,
) -> Vec<Placement> {
    if area.is_empty() {
        return Vec::new();
    }

    let mut y = 0u16;
    let mut placements = Vec::new();
    for (agent_index, agent) in ALL_STATES.iter().enumerate() {
        if let Some(placement) = render_agent(
            frame,
            area,
            &mut y,
            agent,
            app,
            KITTY_IMAGE_ID_START + u32::try_from(agent_index).expect("fixed gallery index"),
        ) {
            placements.push(placement);
        }
    }

    render_line(
        frame,
        area,
        y,
        Line::from(vec!["Normal flow".bold(), "  ·  automatic loop".cyan()]),
    );
    y += 1;
    let flow = normal_flow_frame(elapsed);
    let flow_agent = Agent {
        label: "Flow（normal-flow）",
        status: flow.status,
        activities: &flow.activities[..flow.visible],
        terminal: flow.terminal,
        phase_ms: 440,
    };
    if let Some(placement) = render_agent(
        frame,
        area,
        &mut y,
        &flow_agent,
        app,
        KITTY_IMAGE_ID_START + u32::try_from(ALL_STATES.len()).expect("fixed gallery length"),
    ) {
        placements.push(placement);
    }
    placements
}

fn render_agent(
    frame: &mut Frame<'_>,
    area: Rect,
    y: &mut u16,
    agent: &Agent<'_>,
    app: &App,
    image_id: u32,
) -> Option<Placement> {
    let use_graphics = !app.reduced_motion
        && app.graphics != GraphicsMode::Text
        && matches!(agent.status, AgentStatus::Starting | AgentStatus::Running);
    render_line(frame, area, *y, agent_header(agent, use_graphics));
    let placement = (use_graphics && area.width > 0 && *y < area.height).then(|| {
        let absolute_y = area.y.saturating_add(*y);
        Placement {
            x: area.x,
            y: absolute_y,
            phase_ms: agent.phase_ms,
            background: row_background(absolute_y),
            graphics: app.graphics,
            image_id,
        }
    });
    *y = y.saturating_add(1);

    if let Some(terminal) = agent.terminal {
        render_line(frame, area, *y, terminal_line(terminal, agent.status));
        *y = y.saturating_add(1);
        return placement;
    }
    let visible = agent.activities.len().min(3);
    let start = agent.activities.len().saturating_sub(visible);
    for (index, activity) in agent.activities[start..].iter().enumerate() {
        render_line(
            frame,
            area,
            *y,
            activity_line(activity, index + 1 == visible),
        );
        *y = y.saturating_add(1);
    }
    placement
}

fn render_line(frame: &mut Frame<'_>, area: Rect, row: u16, line: Line<'static>) {
    if row < area.height {
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, area.y + row, area.width, 1),
        );
    }
}

fn agent_header(agent: &Agent<'_>, graphics: bool) -> Line<'static> {
    let symbol = if graphics {
        Span::raw(" ")
    } else {
        status_span(status_symbol(agent.status), agent.status)
    };
    let mut spans = vec![symbol, Span::raw(" "), agent.label.bold()];
    if let Some(word) = match agent.status {
        AgentStatus::WaitingApproval => Some("waiting approval"),
        AgentStatus::Waiting => Some("waiting"),
        _ => None,
    } {
        spans.push(Span::raw("  "));
        spans.push(status_span(word, agent.status));
    }
    Line::from(spans)
}

fn activity_line(activity: &Activity, last: bool) -> Line<'static> {
    let (action, detail) = activity
        .label
        .split_once(' ')
        .unwrap_or((activity.label, ""));
    let mut spans = vec![
        if last { "└─".dim() } else { "├─".dim() },
        activity_span(activity_status_symbol(activity.status), activity.status),
        Span::raw(" "),
        action.to_string().bold(),
    ];
    if !detail.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(detail.to_string().dim());
    }
    if activity.status == ActivityStatus::WaitingApproval {
        spans.push("  waiting approval".dim());
    }
    Line::from(spans)
}

fn terminal_line(label: &'static str, status: AgentStatus) -> Line<'static> {
    Line::from(vec!["└─ ".dim(), status_span(label, status)])
}

fn status_symbol(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting | AgentStatus::Running | AgentStatus::Waiting => "◌",
        AgentStatus::WaitingApproval => "!",
        AgentStatus::Failed => "✗",
        AgentStatus::Cancelled => "×",
        AgentStatus::Completed => "✓",
    }
}

fn activity_status_symbol(status: ActivityStatus) -> &'static str {
    match status {
        ActivityStatus::Running => "›",
        ActivityStatus::WaitingApproval => "!",
        ActivityStatus::Succeeded => "✓",
        ActivityStatus::Failed => "✗",
        ActivityStatus::Declined | ActivityStatus::Cancelled => "×",
    }
}

fn status_span(text: &'static str, status: AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::Starting | AgentStatus::Waiting => text.dim(),
        AgentStatus::Running | AgentStatus::Completed => text.green(),
        AgentStatus::WaitingApproval => text.yellow().bold(),
        AgentStatus::Failed | AgentStatus::Cancelled => text.red().bold(),
    }
}

fn activity_span(text: &'static str, status: ActivityStatus) -> Span<'static> {
    match status {
        ActivityStatus::Running | ActivityStatus::Succeeded => text.green(),
        ActivityStatus::WaitingApproval => text.yellow().bold(),
        ActivityStatus::Failed | ActivityStatus::Declined | ActivityStatus::Cancelled => text.red(),
    }
}

fn row_background(y: u16) -> (u8, u8, u8) {
    if y.is_multiple_of(2) {
        ROW_BG_A
    } else {
        ROW_BG_B
    }
}

fn background_style((red, green, blue): (u8, u8, u8)) -> Style {
    Style::new().bg(Color::Rgb(red, green, blue))
}

fn clear_orbits(placements: &[Placement]) -> io::Result<()> {
    if placements.is_empty() {
        return Ok(());
    }
    let mut output = io::stdout().lock();
    write_clear_orbits(&mut output, placements)?;
    output.flush()
}

fn raster_layer_needs_redraw(previous: &[Placement], next: &[Placement]) -> bool {
    previous.iter().any(|old| {
        !next
            .iter()
            .any(|new| new.graphics == old.graphics && new.x == old.x && new.y == old.y)
    })
}

fn write_clear_orbits(output: &mut impl io::Write, placements: &[Placement]) -> io::Result<()> {
    output.write_all(b"\x1b7")?;
    for placement in placements {
        match placement.graphics {
            GraphicsMode::Sixel => {
                let (red, green, blue) = placement.background;
                write!(
                    output,
                    "\x1b[{};{}H\x1b[48;2;{red};{green};{blue}m\x1b[1X\x1b[0m",
                    placement.y + 1,
                    placement.x + 1
                )?;
            }
            GraphicsMode::Kitty => {
                output.write_all(orbit::kitty_delete_image(placement.image_id).as_bytes())?;
            }
            GraphicsMode::Text => {}
        }
    }
    output.write_all(b"\x1b8")?;
    Ok(())
}

fn draw_orbits(placements: &[Placement], elapsed: Duration) -> io::Result<()> {
    if placements.is_empty() {
        return Ok(());
    }
    let (width, height) = cell_pixels();
    let mut output = io::stdout().lock();
    output.write_all(b"\x1b7")?;
    for placement in placements {
        let values = orbit::intensities(elapsed + Duration::from_millis(placement.phase_ms));
        let outer_spread = if placement.graphics == GraphicsMode::Kitty {
            orbit::KITTY_OUTER_SPREAD
        } else {
            0
        };
        let raster = orbit::rasterize(
            values,
            width,
            height,
            ORBIT_FG,
            placement.background,
            outer_spread,
        );
        let encoded = match placement.graphics {
            GraphicsMode::Sixel => orbit::encode_sixel(&raster),
            GraphicsMode::Kitty => orbit::encode_kitty(&raster, placement.image_id),
            GraphicsMode::Text => continue,
        };
        write!(
            output,
            "\x1b[{};{}H{}",
            placement.y + 1,
            placement.x + 1,
            encoded
        )?;
    }
    output.write_all(b"\x1b8")?;
    output.flush()
}

fn cell_pixels() -> (u16, u16) {
    terminal::window_size()
        .ok()
        .filter(|size| size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0)
        .map(|size| {
            (
                (size.width / size.columns).clamp(1, 32),
                (size.height / size.rows).clamp(1, 64),
            )
        })
        .unwrap_or((10, 20))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    #[test]
    fn harness_contract_stays_light_and_visible() {
        assert_eq!(orbit::ORDER, [0, 1, 2, 5, 8, 7, 6, 3]);
        assert_eq!(orbit::PIXEL_STAGGER, Duration::from_millis(110));
        assert_eq!(orbit::DURATION, Duration::from_millis(950));

        let raster = orbit::rasterize(
            [0.2, 0.4, 0.7, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            12,
            12,
            (255, 255, 255),
            (0, 0, 0),
            0,
        );
        let sample = |row: u16, column: u16| {
            raster.pixel(((2 * column + 1) * 12) / 6, ((2 * row + 1) * 12) / 6)
        };
        assert_eq!(raster.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(raster.pixel(1, 1), [51, 51, 51, 255]);
        assert_eq!(raster.pixel(3, 3), [0, 0, 0, 0]);
        assert_eq!(sample(1, 1), [0, 0, 0, 0]);
        assert_eq!(
            [
                sample(0, 0)[0],
                sample(0, 1)[0],
                sample(0, 2)[0],
                sample(1, 0)[0]
            ],
            [51, 102, 179, 255]
        );
        let kitty_raster = orbit::rasterize(
            [1.0; 9],
            12,
            12,
            (255, 255, 255),
            (0, 0, 0),
            orbit::KITTY_OUTER_SPREAD,
        );
        assert_eq!(kitty_raster.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(kitty_raster.pixel(1, 0), [255, 255, 255, 255]);
        assert!((2..=4).all(|x| kitty_raster.pixel(x, 0)[3] == 0));
        assert_eq!(kitty_raster.pixel(5, 0), [255, 255, 255, 255]);
        assert_eq!(kitty_raster.pixel(10, 0), [255, 255, 255, 255]);
        let sixel = orbit::encode_sixel(&raster);
        assert!(sixel.starts_with("\x1bP9;1;0q"));
        assert!(sixel.ends_with("\x1b\\"));
        let shifted_sixel = orbit::encode_sixel(&orbit::Raster {
            width: 1,
            height: 3,
            rgba: vec![0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 255],
        });
        assert!(shifted_sixel.contains("#0@"));
        let kitty = orbit::encode_kitty(
            &orbit::Raster {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            },
            7,
        );
        assert!(kitty.starts_with("\x1b_Ga=T,t=d,f=32,s=1,v=1,c=1,q=2,i=7,m=0;"));
        assert!(!kitty.contains(",r=1"));
        assert!(kitty.contains("AQIDBA=="));
        assert!(kitty.ends_with("\x1b\\"));
        assert_eq!(orbit::kitty_delete_image(7), "\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
        let chunked_kitty = orbit::encode_kitty(
            &orbit::Raster {
                width: 32,
                height: 64,
                rgba: vec![0; 32 * 64 * 4],
            },
            8,
        );
        assert!(chunked_kitty.contains("i=8,m=1;"));
        assert!(chunked_kitty.contains("\x1b_Gm=0;"));

        let starting = normal_flow_frame(Duration::ZERO);
        assert_eq!(starting.status, AgentStatus::Starting);
        assert_eq!(starting.visible, 0);
        for (stage, visible) in [(1, 1), (2, 2), (3, 3), (4, 3)] {
            let flow = normal_flow_frame(Duration::from_millis(1_200 * stage));
            assert_eq!(flow.status, AgentStatus::Running);
            assert_eq!(flow.visible, visible);
            if stage <= 3 {
                assert_eq!(flow.activities[visible - 1].status, ActivityStatus::Running);
                assert!(
                    flow.activities[..visible - 1]
                        .iter()
                        .all(|activity| activity.status == ActivityStatus::Succeeded)
                );
            } else {
                assert!(
                    flow.activities
                        .iter()
                        .all(|activity| activity.status == ActivityStatus::Succeeded)
                );
            }
        }
        let done = normal_flow_frame(Duration::from_millis(6_000));
        assert_eq!(done.status, AgentStatus::Completed);
        assert_eq!(done.visible, 0);
        assert_eq!(
            done.terminal,
            Some("Done (3 tool uses ⋅ 4.8k tokens ⋅ 6.0s)")
        );
        assert_eq!(
            normal_flow_frame(Duration::from_millis(8_400)).status,
            AgentStatus::Starting
        );

        let app = App {
            reduced_motion: true,
            graphics: GraphicsMode::Text,
            started_at: Instant::now(),
        };
        let (gallery, placements) = render_text(&app, 100, 24, Duration::from_millis(3_600));
        for state in [
            "◌ Builder",
            "◌ Robie",
            "! Reviewer",
            "◌ Observer",
            "✓ Finisher",
            "✗ Tester",
            "× Cleaner",
        ] {
            assert!(gallery.contains(state), "missing {state}");
        }
        assert!(gallery.contains("├─✓ read src/subagent_live_panel.rs"));
        assert!(gallery.contains("└─✗ inspect transparent Orbit raster"));
        assert!(gallery.contains("├─× decline destructive command"));
        assert!(gallery.contains("└─× cleanup temporary build"));
        assert!(gallery.contains("└─ Done (19 tool uses ⋅ 32.0k tokens ⋅ 2m 12.2s)"));
        assert!(gallery.contains("└─ Failed"));
        assert!(gallery.contains("└─ Cancelled"));
        assert!(gallery.contains("Normal flow  ·  automatic loop"));
        assert!(gallery.contains("◌ Flow"));
        assert!(gallery.contains("├─✓ inspect repository"));
        assert!(gallery.contains("├─✓ edit Orbit renderer"));
        assert!(gallery.contains("└─› test lightweight UI"));
        assert!(!gallery.contains("OpenAI Codex"));
        assert!(!gallery.contains("Message"));
        assert!(!gallery.contains("Ask Codex"));
        assert!(!gallery.contains("Scenario"));
        assert!(placements.is_empty());
        let (done_gallery, _) = render_text(&app, 100, 24, Duration::from_millis(6_000));
        assert!(done_gallery.contains("✓ Flow"));
        assert!(done_gallery.contains("└─ Done (3 tool uses ⋅ 4.8k tokens ⋅ 6.0s)"));

        let animated = App {
            reduced_motion: false,
            graphics: GraphicsMode::Sixel,
            started_at: Instant::now(),
        };
        let (_, sixel_placements) = render_text(&animated, 100, 24, Duration::ZERO);
        assert_eq!(sixel_placements.len(), 3);
        assert!(
            sixel_placements
                .iter()
                .all(|placement| placement.graphics == GraphicsMode::Sixel)
        );
        let mut sixel_clear = Vec::new();
        write_clear_orbits(&mut sixel_clear, &sixel_placements).unwrap();
        let sixel_clear = String::from_utf8(sixel_clear).unwrap();
        assert_eq!(sixel_clear.matches("\x1b[1X").count(), 3);
        let (_, done_sixel_placements) =
            render_text(&animated, 100, 24, Duration::from_millis(6_000));
        assert_eq!(done_sixel_placements.len(), 2);
        assert!(raster_layer_needs_redraw(
            &sixel_placements,
            &done_sixel_placements
        ));
        assert!(!raster_layer_needs_redraw(
            &sixel_placements,
            &sixel_placements
        ));

        let kitty = App {
            reduced_motion: false,
            graphics: GraphicsMode::Kitty,
            started_at: Instant::now(),
        };
        let (_, kitty_placements) = render_text(&kitty, 100, 24, Duration::ZERO);
        assert_eq!(kitty_placements.len(), 3);
        let (_, done_kitty_placements) = render_text(&kitty, 100, 24, Duration::from_millis(6_000));
        assert_eq!(done_kitty_placements.len(), 2);
        assert!(raster_layer_needs_redraw(
            &kitty_placements,
            &done_kitty_placements
        ));
        let mut kitty_clear = Vec::new();
        write_clear_orbits(&mut kitty_clear, &kitty_placements).unwrap();
        let kitty_clear = String::from_utf8(kitty_clear).unwrap();
        assert_eq!(kitty_clear.matches("a=d,d=I").count(), 3);
    }

    fn render_text(
        app: &App,
        width: u16,
        height: u16,
        elapsed: Duration,
    ) -> (String, Vec<Placement>) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut placements = Vec::new();
        let completed = terminal
            .draw(|frame| placements = render(frame, app, elapsed))
            .expect("draw fixture");
        (buffer_text(completed.buffer), placements)
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut output = String::new();
        for y in buffer.area.y..buffer.area.bottom() {
            for x in buffer.area.x..buffer.area.right() {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }
}
