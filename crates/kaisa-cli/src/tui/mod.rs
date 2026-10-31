//! F10 terminal home. Display and refresh are read-only; only submitting a new goal starts work.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

mod messages;
mod model;

use messages::{Message, format as message, text};
use model::{Snapshot, details, safe};

const BACKGROUND: Color = Color::Rgb(13, 22, 38);
const TEXT: Color = Color::Rgb(214, 227, 236);
const VERIFIED: Color = Color::Rgb(45, 212, 191);
const AURORA: Color = Color::Rgb(139, 124, 246);
const WAITING: Color = Color::Rgb(245, 184, 65);
const FAILED: Color = Color::Rgb(240, 97, 109);
const MUTED: Color = Color::Rgb(75, 91, 115);
const POLL: Duration = Duration::from_millis(1000);

pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    if args
        .iter()
        .any(|argument| !matches!(argument.as_str(), "--plain" | "--help" | "-h"))
    {
        writeln!(err, "{}", text(Message::CommandUsage))?;
        return Ok(crate::EXIT_USAGE);
    }
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        writeln!(
            out,
            "kaisa tui [--plain]\n\n{}\n\nKAISA_LANG: case-insensitive ko prefix selects Korean; otherwise English.\nWhen unset: LC_ALL, LC_MESSAGES, LANG (first nonempty); default English.",
            text(Message::Help)
        )?;
        return Ok(crate::EXIT_SUCCESS);
    }
    let state = match kaisa_sys::state_location(|name| std::env::var_os(name)) {
        Ok(location) => location.directory,
        Err(error) => {
            writeln!(err, "{}", message(Message::Error, &[&error]))?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    let snapshot = match model::load(&state) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            writeln!(err, "{}", message(Message::LedgerError, &[&error]))?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    if args.iter().any(|argument| argument == "--plain")
        || !io::stdout().is_terminal()
        || !io::stdin().is_terminal()
    {
        plain(&snapshot, out)?;
        return Ok(crate::EXIT_SUCCESS);
    }
    match interactive(snapshot, &state) {
        Ok(()) => Ok(crate::EXIT_SUCCESS),
        Err(error) => {
            writeln!(err, "{}", message(Message::Error, &[&error]))?;
            Ok(crate::EXIT_FAILURE)
        }
    }
}

fn plain(snapshot: &Snapshot, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}", text(Message::PlainTitle))?;
    let mut waiting = false;
    for run in snapshot.runs.iter().filter(|run| run.needs_person()) {
        waiting = true;
        writeln!(
            out,
            "{} · {}\n  {}",
            safe(&run.id),
            safe(&run.goal),
            safe(&run.reason)
        )?;
    }
    if !waiting {
        writeln!(out, "  {}", text(Message::NoNeeds))?;
    }
    writeln!(out, "\n{}", text(Message::Runs))?;
    if snapshot.runs.is_empty() {
        writeln!(out, "{}", text(Message::Empty))?;
    }
    for run in &snapshot.runs {
        writeln!(out, "\n{}", details(run, None))?;
    }
    writeln!(out, "{}", totals(snapshot))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    run: usize,
    attempt: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    NeedsYou,
    Failed,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Input {
    #[default]
    Browse,
    Search,
    Goal(String),
}

#[derive(Default)]
struct App {
    snapshot: Snapshot,
    filter: Filter,
    search: String,
    input: Input,
    list: ListState,
    details_focused: bool,
    scroll: u16,
    help: bool,
    notice: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Quit,
    Refresh,
    Start(String),
}

impl App {
    fn new(snapshot: Snapshot) -> Self {
        let mut app = Self {
            snapshot,
            ..Self::default()
        };
        app.clamp();
        app
    }

    fn rows(&self) -> Vec<Selection> {
        let query = self.search.to_lowercase();
        let mut rows = Vec::new();
        for (index, run) in self.snapshot.runs.iter().enumerate() {
            if (self.filter == Filter::NeedsYou && !run.needs_person())
                || (self.filter == Filter::Failed && !run.failed())
                || (!query.is_empty() && !details(run, None).to_lowercase().contains(&query))
            {
                continue;
            }
            rows.push(Selection {
                run: index,
                attempt: None,
            });
            rows.extend((0..run.attempts.len()).map(|attempt| Selection {
                run: index,
                attempt: Some(attempt),
            }));
        }
        rows
    }

    fn clamp(&mut self) {
        let count = self.rows().len();
        self.list.select(if count == 0 {
            None
        } else {
            Some(self.list.selected().unwrap_or_default().min(count - 1))
        });
    }

    fn replace(&mut self, snapshot: Snapshot) {
        // Keep the same attempt/run selected when polling reorders Needs you or links an attempt.
        let selected = self.selected().map(|selected| {
            let run = &self.snapshot.runs[selected.run];
            selected.attempt.map_or_else(
                || run.id.clone(),
                |index| run.attempts[index].row.attempt.as_str().to_owned(),
            )
        });
        self.snapshot = snapshot;
        if let Some(id) = selected {
            let position = self.rows().iter().position(|selected| {
                let run = &self.snapshot.runs[selected.run];
                selected.attempt.map_or_else(
                    || run.id == id,
                    |index| run.attempts[index].row.attempt.as_str() == id,
                )
            });
            self.list.select(position);
        }
        self.clamp();
    }

    fn selected(&self) -> Option<Selection> {
        self.list
            .selected()
            .and_then(|index| self.rows().get(index).copied())
    }

    fn event(&mut self, event: Event) -> Action {
        match event {
            Event::Key(key) => self.key(key),
            Event::Paste(pasted) => {
                let literal = safe(&pasted.replace(['\r', '\n'], " "));
                match &mut self.input {
                    Input::Goal(goal) => goal.push_str(&literal),
                    Input::Search => self.search.push_str(&literal),
                    Input::Browse => {}
                }
                self.clamp();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return Action::Quit;
        }
        // Escape always quits, including during input and help.
        if key.code == KeyCode::Esc {
            return Action::Quit;
        }
        if self.help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Enter => self.help = false,
                KeyCode::Char('q') => return Action::Quit,
                _ => {}
            }
            return Action::None;
        }
        if self.input != Input::Browse {
            match key.code {
                KeyCode::Enter => {
                    let input = std::mem::take(&mut self.input);
                    if let Input::Goal(goal) = input {
                        if goal.trim().is_empty() {
                            self.notice = text(Message::GoalNeeded).to_owned();
                        } else {
                            return Action::Start(goal);
                        }
                    }
                }
                KeyCode::Backspace => match &mut self.input {
                    Input::Search => {
                        self.search.pop();
                    }
                    Input::Goal(goal) => {
                        goal.pop();
                    }
                    Input::Browse => {}
                },
                KeyCode::Char(character)
                    if key
                        .modifiers
                        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        || !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    match &mut self.input {
                        Input::Search => self.search.push(character),
                        Input::Goal(goal) => goal.push(character),
                        Input::Browse => {}
                    }
                }
                _ => {}
            }
            self.clamp();
            return Action::None;
        }
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('r') => return Action::Refresh,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => {
                self.input = Input::Search;
                self.search.clear();
                self.list.select(None);
            }
            KeyCode::Char('g') => self.input = Input::Goal(String::new()),
            KeyCode::Char('n' | 'f') => {
                let filter = if key.code == KeyCode::Char('n') {
                    Filter::NeedsYou
                } else {
                    Filter::Failed
                };
                self.filter = if self.filter == filter {
                    Filter::All
                } else {
                    filter
                };
                self.list.select(None);
                self.scroll = 0;
            }
            KeyCode::Tab => self.details_focused = !self.details_focused,
            KeyCode::Enter => self.details_focused = true,
            KeyCode::Down | KeyCode::Char('j') if self.details_focused => {
                self.scroll = self.scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if self.details_focused => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.rows().len().saturating_sub(1);
                self.list.select(Some(
                    self.list
                        .selected()
                        .unwrap_or_default()
                        .saturating_add(1)
                        .min(last),
                ));
                self.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.list.select(Some(
                    self.list.selected().unwrap_or_default().saturating_sub(1),
                ));
                self.scroll = 0;
            }
            _ => {}
        }
        self.clamp();
        Action::None
    }
}

fn block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .title(Span::styled(title, Style::default().fg(AURORA)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            AURORA
        } else {
            Color::Rgb(49, 67, 91)
        }))
}

fn compact_usage(attempt: &model::Attempt) -> String {
    let tokens = attempt.usage.as_ref().map_or_else(
        || "n/a".to_owned(),
        |usage| format!("{}/{}", usage.input_tokens, usage.output_tokens),
    );
    let cost = attempt
        .usage
        .as_ref()
        .and_then(|usage| usage.cost_usd)
        .map_or_else(|| "n/a".to_owned(), |cost| format!("${cost:.4}"));
    message(Message::CompactUsage, &[&tokens, &cost])
}

fn totals(snapshot: &Snapshot) -> String {
    let mut cost = 0.0;
    let mut unknown = 0;
    let mut tokens: u64 = 0;
    for attempt in snapshot.runs.iter().flat_map(|run| &run.attempts) {
        if let Some(usage) = &attempt.usage {
            tokens = tokens
                .saturating_add(usage.input_tokens)
                .saturating_add(usage.output_tokens);
        }
        if let Some(reported) = attempt.usage.as_ref().and_then(|usage| usage.cost_usd) {
            cost += reported;
        } else {
            unknown += 1;
        }
    }
    message(Message::Totals, &[&tokens, &format!("{cost:.4}"), &unknown])
}

// Keep untrusted display text inspectable before Ratatui's buffer drops control graphemes.
fn needs_lines(snapshot: &Snapshot) -> Vec<Line<'static>> {
    let waiting: Vec<_> = snapshot
        .runs
        .iter()
        .filter(|run| run.needs_person())
        .collect();
    let mut needs: Vec<Line<'_>> = waiting
        .iter()
        .take(3)
        .flat_map(|run| {
            [
                Line::from(Span::styled(
                    safe(&run.goal),
                    Style::default().fg(WAITING).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    safe(&run.reason),
                    Style::default().fg(WAITING),
                )),
            ]
        })
        .collect();
    if waiting.len() > 3 {
        needs.push(Line::from(Span::styled(
            message(Message::MoreNeeds, &[&(waiting.len() - 3)]),
            Style::default().fg(WAITING),
        )));
    }
    if needs.is_empty() {
        needs.push(Line::from(text(Message::NoNeeds)));
    }
    needs
}

fn row_lines(app: &App, selected: &Selection) -> Vec<Line<'static>> {
    let run = &app.snapshot.runs[selected.run];
    if let Some(index) = selected.attempt {
        let attempt = &run.attempts[index];
        vec![
            Line::from(safe(&format!(
                "  └ {} · {}",
                attempt.role,
                attempt.row.attempt.as_str()
            ))),
            Line::from(format!(
                "    {} / {}",
                attempt.row.execution.code(),
                attempt.row.cleanup.code()
            )),
            Line::from(format!("    {}", compact_usage(attempt))),
            Line::from(Span::styled(
                safe(&format!("    {}", attempt.containment)),
                Style::default().fg(WAITING),
            )),
        ]
    } else {
        let color = if run.needs_person() {
            WAITING
        } else if run.failed() {
            FAILED
        } else if run.decision == "Deliver" {
            VERIFIED
        } else {
            AURORA
        };
        vec![
            Line::from(Span::styled(
                safe(&run.goal),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                safe(&format!("{} · {}", run.id, run.status())),
                Style::default().fg(MUTED),
            )),
        ]
    }
}

fn runs_title(app: &App) -> String {
    let filter = text(match app.filter {
        Filter::All => Message::All,
        Filter::NeedsYou => Message::Needs,
        Filter::Failed => Message::Failed,
    });
    message(Message::RunsTitle, &[&filter, &safe(&app.search)])
}

fn input_prompt(app: &App) -> String {
    match &app.input {
        Input::Browse => message(Message::Footer, &[&safe(&app.notice)]),
        Input::Search => message(Message::SearchPrompt, &[&safe(&app.search)]),
        Input::Goal(goal) => message(Message::GoalPrompt, &[&safe(goal)]),
    }
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND).fg(TEXT)),
        area,
    );
    let outer = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    " KAISA ",
                    Style::default().fg(VERIFIED).add_modifier(Modifier::BOLD),
                ),
                Span::raw(text(Message::Header)),
            ]),
            Line::from(Span::styled(
                format!(" {}", totals(&app.snapshot)),
                Style::default().fg(MUTED),
            )),
        ]),
        outer[0],
    );
    let panes = if area.width >= 100 {
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).split(outer[1])
    } else {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(outer[1])
    };
    let needs = needs_lines(&app.snapshot);
    let needs_height = u16::try_from(needs.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .max(3);
    let left =
        Layout::vertical([Constraint::Length(needs_height), Constraint::Min(1)]).split(panes[0]);
    let needs = if needs.is_empty() {
        vec![Line::from(text(Message::NoNeeds))]
    } else {
        needs
    };
    // Each entry has two fixed lines; truncating long reasons keeps the count visible.
    frame.render_widget(
        Paragraph::new(needs).block(block(text(Message::NeedsTitle), false)),
        left[0],
    );
    let rows = app.rows();
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|selected| ListItem::new(row_lines(app, selected)))
        .collect();
    let title = runs_title(app);
    if items.is_empty() {
        let message = if app.snapshot.runs.is_empty() {
            text(Message::EmptyInteractive)
        } else {
            text(Message::NoMatches)
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block(&title, !app.details_focused))
                .wrap(Wrap { trim: false }),
            left[1],
        );
    } else {
        frame.render_stateful_widget(
            List::new(items)
                .block(block(&title, !app.details_focused))
                .highlight_symbol("› ")
                .highlight_style(Style::default().bg(AURORA).fg(BACKGROUND)),
            left[1],
            &mut app.list,
        );
    }
    let detail = app
        .selected()
        .map(|selected| details(&app.snapshot.runs[selected.run], selected.attempt))
        .unwrap_or_else(|| text(Message::Choose).to_owned());
    frame.render_widget(
        Paragraph::new(
            detail
                .lines()
                .map(|line| {
                    let color = if line.contains("NeedsPerson") {
                        WAITING
                    } else if line.contains("Refuse")
                        || line.contains("Blocking")
                        || line == text(Message::NoMerge).trim_end()
                    {
                        FAILED
                    } else if line.contains("Deliver") || line.starts_with("git merge --ff-only") {
                        VERIFIED
                    } else {
                        TEXT
                    };
                    Line::from(Span::styled(line.to_owned(), Style::default().fg(color)))
                })
                .collect::<Vec<_>>(),
        )
        .block(block(text(Message::DetailsTitle), app.details_focused))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0)),
        panes[1],
    );
    let prompt = input_prompt(app);
    frame.render_widget(
        Paragraph::new(prompt)
            .style(Style::default().fg(AURORA))
            .wrap(Wrap { trim: false }),
        outer[2],
    );
    if app.help {
        let popup = centered(area, 84, 20);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(text(Message::Help))
                .block(block(text(Message::KeysTitle), true))
                .style(Style::default().bg(BACKGROUND).fg(TEXT))
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Installed before initialization so even a partially failed setup restores the terminal.
struct RestoreGuard<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for RestoreGuard<F> {
    fn drop(&mut self) {
        if let Some(restore) = self.0.take() {
            restore();
        }
    }
}

fn restore_terminal() {
    let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

// Signal handlers only set an atomic flag (async-signal-safe). The event loop observes it
// within its 100ms poll, then returns through the same restore guard as keyboard exits.
#[cfg(unix)]
struct ExitSignals {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
}

#[cfg(unix)]
impl ExitSignals {
    fn install() -> io::Result<Self> {
        let mut signals = Self {
            flag: std::sync::Arc::default(),
            registrations: Vec::new(),
        };
        for signal in [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGHUP,
        ] {
            signals
                .registrations
                .push(signal_hook::flag::register(signal, signals.flag.clone())?);
        }
        Ok(signals)
    }

    fn received(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(unix)]
impl Drop for ExitSignals {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

fn interactive(snapshot: Snapshot, state: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let signals = ExitSignals::install()?;
    let _restore = RestoreGuard(Some(restore_terminal));
    // Ratatui also installs a panic hook that restores before the panic diagnostic is printed.
    let mut terminal = ratatui::try_init()?;
    // Include paste mode in panic restoration as well as return/error restoration.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous_hook(info);
    }));
    crossterm::execute!(io::stdout(), EnableBracketedPaste)?;
    let mut app = App::new(snapshot);
    let mut last_refresh = Instant::now();
    let mut children: Vec<(Child, PathBuf)> = Vec::new();
    loop {
        #[cfg(unix)]
        if signals.received() {
            return Ok(());
        }
        terminal.draw(|frame| render(frame, &mut app))?;
        let action = if event::poll(Duration::from_millis(100))? {
            app.event(event::read()?)
        } else {
            Action::None
        };
        match action {
            Action::Quit => return Ok(()),
            Action::Start(ref goal) => match start_goal(
                &std::env::current_exe()?,
                &std::env::current_dir()?,
                state,
                goal,
            ) {
                Ok((child, log)) => {
                    app.notice = message(Message::Started, &[&child.id(), &log.display()]);
                    children.push((child, log));
                }
                Err(error) => app.notice = message(Message::StartError, &[&error]),
            },
            Action::None | Action::Refresh => {}
        }
        if action == Action::Refresh || last_refresh.elapsed() >= POLL {
            match model::load(state) {
                Ok(snapshot) => app.replace(snapshot),
                Err(error) => app.notice = message(Message::RefreshError, &[&error]),
            }
            last_refresh = Instant::now();
        }
        children.retain_mut(|(child, log)| match child.try_wait() {
            Ok(Some(exit)) => {
                app.notice = message(Message::ProcessExit, &[&exit, &log.display()]);
                false
            }
            Ok(None) => true,
            Err(error) => {
                app.notice = message(Message::ProcessUnknown, &[&error, &log.display()]);
                true
            }
        });
    }
}

/// A separate process group keeps terminal Ctrl-C in this UI from cancelling the submitted goal.
fn goal_command(program: &Path, repository: &Path, goal: &str) -> Command {
    let mut command = Command::new(program);
    command
        .args(["--", goal])
        .current_dir(repository)
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
}

fn start_goal(
    program: &Path,
    repository: &Path,
    state: &Path,
    goal: &str,
) -> io::Result<(Child, PathBuf)> {
    // The only write path in this module: an explicit g + Enter, never a refresh.
    kaisa_sys::ensure_private_directory(state)?;
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let log = state.join(format!("tui-goal-{}-{suffix}.log", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(kaisa_sys::PRIVATE_FILE_MODE);
    }
    let output = options.open(&log)?;
    let mut command = goal_command(program, repository, goal);
    command
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output));
    Ok((command.spawn()?, log))
}

#[cfg(test)]
mod tests;
