use std::{
    path::Path,
    sync::{mpsc, Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt as _, MessageDialogKind};

use crate::{
    claude_settings::{ClaudeSettingsManager, ClaudeTrackingState},
    coordinator::{start_supervisor, CollectionCoordinator, CollectorSupervisor},
    model::ProviderSnapshot,
    paths::resolve_codex_roots,
    providers::codex::CodexCollector,
    startup::{
        disable_startup, enable_startup, repair_startup, startup_status, StartupError,
        StartupRegistration,
    },
    state_store::{
        default_state_path, JsonStateStore, PersistedState, StateMutation, StateStore,
        WindowPlacement,
    },
};

const MAIN_WINDOW: &str = "main";
const TRAY_ID: &str = "usage-widget";
const WIDGET_WIDTH: f64 = 356.0;
const TITLE_HEIGHT: i32 = 36;
const POSITION_DEBOUNCE: Duration = Duration::from_millis(300);
const TRAY_ERROR_MESSAGE: &str = "Usage Widget could not complete that tray action.";

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    Disabled,
    Enabled,
    NeedsRepair,
    Conflict,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Empty,
    Single,
    Dual,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct WidgetView {
    pub providers: Vec<ProviderSnapshot>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WorkArea {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

pub struct ShellState {
    coordinator: Arc<CollectionCoordinator>,
    store: Arc<dyn StateStore>,
    claude_settings: ClaudeSettingsManager,
    startup: Arc<dyn StartupRegistration>,
    supervisor: Mutex<Option<CollectorSupervisor>>,
    position_saver: Mutex<Option<PositionSaver>>,
}

impl ShellState {
    fn stop_and_join(&self) {
        if let Some(mut supervisor) = lock_unpoisoned(&self.supervisor).take() {
            supervisor.stop_and_join();
        }
        if let Some(mut saver) = lock_unpoisoned(&self.position_saver).take() {
            saver.stop_and_join();
        }
    }

    fn note_position(&self, position: WindowPlacement) {
        if let Some(saver) = lock_unpoisoned(&self.position_saver).as_ref() {
            saver.note(position);
        }
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("GUI startup failed")]
pub struct GuiStartError;

struct TauriStartupRegistration {
    app: AppHandle,
}

impl StartupRegistration for TauriStartupRegistration {
    fn enable(&self) -> Result<(), StartupError> {
        self.app
            .autolaunch()
            .enable()
            .map_err(|_| StartupError::OperationFailed)
    }

    fn disable(&self) -> Result<(), StartupError> {
        self.app
            .autolaunch()
            .disable()
            .map_err(|_| StartupError::OperationFailed)
    }

    fn is_enabled(&self) -> Result<bool, StartupError> {
        self.app
            .autolaunch()
            .is_enabled()
            .map_err(|_| StartupError::Unavailable)
    }
}

enum PositionSignal {
    Moved(WindowPlacement),
    Stop,
}

struct PositionSaver {
    sender: mpsc::Sender<PositionSignal>,
    join: Option<thread::JoinHandle<()>>,
}

impl PositionSaver {
    fn start(store: Arc<dyn StateStore>) -> Result<Self, GuiStartError> {
        let (sender, receiver) = mpsc::channel();
        let join = thread::Builder::new()
            .name("usage-widget-position".into())
            .spawn(move || position_worker(receiver, store))
            .map_err(|_| GuiStartError)?;
        Ok(Self {
            sender,
            join: Some(join),
        })
    }

    fn note(&self, position: WindowPlacement) {
        let _ = self.sender.send(PositionSignal::Moved(position));
    }

    fn stop_and_join(&mut self) {
        let _ = self.sender.send(PositionSignal::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PositionSaver {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn position_worker(receiver: mpsc::Receiver<PositionSignal>, store: Arc<dyn StateStore>) {
    let mut pending = None;
    loop {
        let signal = if pending.is_some() {
            match receiver.recv_timeout(POSITION_DEBOUNCE) {
                Ok(signal) => signal,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    persist_pending_position(&store, &mut pending);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match receiver.recv() {
                Ok(signal) => signal,
                Err(_) => break,
            }
        };
        match signal {
            PositionSignal::Moved(position) => pending = Some(position),
            PositionSignal::Stop => break,
        }
    }
    persist_pending_position(&store, &mut pending);
}

fn persist_pending_position(store: &Arc<dyn StateStore>, pending: &mut Option<WindowPlacement>) {
    if let Some(position) = pending.take() {
        let _ = store.apply(unix_now(), StateMutation::SetWindow(Some(position)));
    }
}

#[tauri::command]
pub fn get_widget_view(state: tauri::State<'_, ShellState>) -> Result<WidgetView, CommandError> {
    Ok(project_view(&state.coordinator, unix_now()))
}

#[tauri::command]
pub async fn refresh(state: tauri::State<'_, ShellState>) -> Result<WidgetView, CommandError> {
    let coordinator = state.coordinator.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let now = unix_now();
        coordinator.refresh_now(now).map_err(|_| refresh_error())?;
        Ok(project_view(&coordinator, now))
    })
    .await
    .map_err(|_| refresh_error())?
}

#[tauri::command]
pub fn hide_widget(app: AppHandle) -> Result<(), CommandError> {
    app.get_webview_window(MAIN_WINDOW)
        .ok_or_else(window_error)?
        .hide()
        .map_err(|_| window_error())
}

#[tauri::command]
pub fn set_widget_layout(app: AppHandle, layout: Layout) -> Result<(), CommandError> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or_else(window_error)?;
    window
        .set_size(tauri::Size::Logical(tauri::LogicalSize::new(
            WIDGET_WIDTH,
            height_for_layout(layout),
        )))
        .map_err(|_| window_error())
}

pub fn run_gui() -> Result<(), GuiStartError> {
    let now = unix_now();
    let store: Arc<dyn StateStore> = Arc::new(JsonStateStore::new(
        default_state_path().map_err(|_| GuiStartError)?,
    ));
    let user_profile = std::env::var_os("USERPROFILE")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or(GuiStartError)?;
    let roots = resolve_codex_roots(std::env::var_os("CODEX_HOME").as_deref(), &user_profile);
    let coordinator = Arc::new(
        CollectionCoordinator::load(Arc::new(CodexCollector::new(roots)), store.clone(), now)
            .map_err(|_| GuiStartError)?,
    );
    let claude_settings =
        ClaudeSettingsManager::from_environment(store.clone()).map_err(|_| GuiStartError)?;

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
            refresh_in_background(app);
        }))
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_widget_view,
            refresh,
            hide_widget,
            set_widget_layout
        ])
        .setup(move |app| {
            let _ = coordinator.refresh_now(unix_now());
            let persisted = store.load(unix_now()).map_err(|_| GuiStartError)?;
            let supervisor = start_supervisor(coordinator.clone()).map_err(|_| GuiStartError)?;
            let position_saver = PositionSaver::start(store.clone())?;
            let startup: Arc<dyn StartupRegistration> = Arc::new(TauriStartupRegistration {
                app: app.handle().clone(),
            });
            app.manage(ShellState {
                coordinator: coordinator.clone(),
                store: store.clone(),
                claude_settings,
                startup,
                supervisor: Mutex::new(Some(supervisor)),
                position_saver: Mutex::new(Some(position_saver)),
            });

            if let Err(error) = build_tray(app.handle()) {
                show_tray_error(app.handle());
                return Err(error.into());
            }
            configure_main_window(app.handle(), &persisted)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|_| GuiStartError)
}

fn configure_main_window(app: &AppHandle, persisted: &PersistedState) -> tauri::Result<()> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or(tauri::Error::WindowNotFound)?;
    window.set_always_on_top(persisted.always_on_top)?;
    let count = app
        .state::<ShellState>()
        .coordinator
        .current_snapshots(unix_now())
        .len();
    window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
        WIDGET_WIDTH,
        height_for_layout(layout_for_count(count)),
    )))?;
    if let Some(saved) = persisted.window.clone() {
        let monitors = window
            .available_monitors()?
            .into_iter()
            .map(|monitor| {
                let area = monitor.work_area();
                WorkArea::new(
                    area.position.x,
                    area.position.y,
                    area.position
                        .x
                        .saturating_add(i32::try_from(area.size.width).unwrap_or(i32::MAX)),
                    area.position
                        .y
                        .saturating_add(i32::try_from(area.size.height).unwrap_or(i32::MAX)),
                )
            })
            .collect::<Vec<_>>();
        let width = window
            .outer_size()
            .ok()
            .and_then(|size| i32::try_from(size.width).ok())
            .unwrap_or(WIDGET_WIDTH as i32);
        let position = clamp_position(saved, width, TITLE_HEIGHT, &monitors);
        window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
            position.x, position.y,
        )))?;
    }

    let event_window = window.clone();
    let event_app = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = event_window.hide();
        }
        WindowEvent::Moved(position) => {
            event_app
                .state::<ShellState>()
                .note_position(WindowPlacement {
                    x: position.x,
                    y: position.y,
                });
        }
        _ => {}
    });
    window.show()?;
    window.set_focus()?;
    Ok(())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<ShellState>();
    let persisted = state.store.load(unix_now()).unwrap_or_default();
    let current_exe = std::env::current_exe().ok();
    let startup_status = current_exe
        .as_deref()
        .map(|current| startup_integration_status(&state, current, &persisted))
        .unwrap_or(IntegrationStatus::Conflict);
    let claude_status = current_exe
        .as_deref()
        .map(|current| state.claude_settings.status(current, unix_now()))
        .unwrap_or(ClaudeTrackingState::Conflict);

    let show_hide = MenuItem::with_id(app, "show_hide", "Show / Hide", true, None::<&str>)?;
    let refresh_item = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
    let always_on_top = CheckMenuItem::with_id(
        app,
        "always_on_top",
        "Always on top",
        true,
        persisted.always_on_top,
        None::<&str>,
    )?;
    let launch_at_sign_in = CheckMenuItem::with_id(
        app,
        "launch_at_sign_in",
        "Launch at sign in",
        true,
        startup_status == IntegrationStatus::Enabled,
        None::<&str>,
    )?;
    let claude_tracking = MenuItem::with_id(
        app,
        "claude_tracking",
        claude_label(claude_status),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_hide,
            &refresh_item,
            &always_on_top,
            &launch_at_sign_in,
            &claude_tracking,
            &quit,
        ],
    )?;

    let topmost_handle = always_on_top.clone();
    let startup_handle = launch_at_sign_in.clone();
    let claude_handle = claude_tracking.clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Usage Widget")
        .on_menu_event(move |app, event| {
            let result = match event.id().as_ref() {
                "show_hide" => toggle_main_window(app),
                "refresh" => {
                    refresh_in_background(app);
                    Ok(())
                }
                "always_on_top" => toggle_topmost(app, &topmost_handle),
                "launch_at_sign_in" => toggle_startup(app, &startup_handle),
                "claude_tracking" => toggle_claude(app, &claude_handle),
                "quit" => {
                    quit_app(app);
                    Ok(())
                }
                _ => Ok(()),
            };
            if result.is_err() {
                show_tray_error(app);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn toggle_main_window(app: &AppHandle) -> Result<(), ()> {
    let window = app.get_webview_window(MAIN_WINDOW).ok_or(())?;
    if window.is_visible().map_err(|_| ())? {
        window.hide().map_err(|_| ())
    } else {
        show_main_window(app);
        Ok(())
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn toggle_topmost(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) -> Result<(), ()> {
    let window = app.get_webview_window(MAIN_WINDOW).ok_or(())?;
    let enabled = !window.is_always_on_top().map_err(|_| ())?;
    window.set_always_on_top(enabled).map_err(|_| ())?;
    app.state::<ShellState>()
        .store
        .apply(unix_now(), StateMutation::SetAlwaysOnTop(enabled))
        .map_err(|_| ())?;
    item.set_checked(enabled).map_err(|_| ())
}

fn toggle_startup(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) -> Result<(), ()> {
    let state = app.state::<ShellState>();
    let now = unix_now();
    let current = std::env::current_exe().map_err(|_| ())?;
    let persisted = state.store.load(now).map_err(|_| ())?;
    let status = startup_integration_status(&state, &current, &persisted);
    let next = match status {
        IntegrationStatus::Disabled => {
            enable_startup(state.startup.as_ref(), state.store.as_ref(), &current, now)
        }
        IntegrationStatus::Enabled => {
            disable_startup(state.startup.as_ref(), state.store.as_ref(), now)
        }
        IntegrationStatus::NeedsRepair => {
            repair_startup(state.startup.as_ref(), state.store.as_ref(), &current, now)
        }
        IntegrationStatus::Conflict => Err(StartupError::OperationFailed),
    }
    .map_err(|_| ())?;
    item.set_checked(next == IntegrationStatus::Enabled)
        .map_err(|_| ())
}

fn startup_integration_status(
    state: &ShellState,
    current: &Path,
    persisted: &PersistedState,
) -> IntegrationStatus {
    let Ok(registered) = state.startup.is_enabled() else {
        return IntegrationStatus::Conflict;
    };
    if !persisted.launch_at_signin_requested {
        return if registered {
            IntegrationStatus::Conflict
        } else {
            IntegrationStatus::Disabled
        };
    }
    if !registered {
        return IntegrationStatus::NeedsRepair;
    }
    persisted
        .startup_identity
        .as_ref()
        .map(|identity| startup_status(true, &identity.installed_exe, current))
        .unwrap_or(IntegrationStatus::NeedsRepair)
}

fn toggle_claude(app: &AppHandle, item: &MenuItem<tauri::Wry>) -> Result<(), ()> {
    let state = app.state::<ShellState>();
    let now = unix_now();
    let current = std::env::current_exe().map_err(|_| ())?;
    let status = state.claude_settings.status(&current, now);
    let next = match status {
        ClaudeTrackingState::Disabled => state.claude_settings.enable(&current, now),
        ClaudeTrackingState::Enabled => state.claude_settings.disable(now),
        ClaudeTrackingState::NeedsRepair | ClaudeTrackingState::Conflict => {
            state.claude_settings.repair(&current, now)
        }
    }
    .map_err(|_| ())?;
    item.set_text(claude_label(next)).map_err(|_| ())
}

fn claude_label(status: ClaudeTrackingState) -> &'static str {
    match status {
        ClaudeTrackingState::Disabled => "Enable Claude tracking",
        ClaudeTrackingState::Enabled => "Disable Claude tracking",
        ClaudeTrackingState::NeedsRepair | ClaudeTrackingState::Conflict => {
            "Repair Claude tracking"
        }
    }
}

fn refresh_in_background(app: &AppHandle) {
    let Some(state) = app.try_state::<ShellState>() else {
        return;
    };
    let coordinator = state.coordinator.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = coordinator.refresh_now(unix_now());
    });
}

fn quit_app(app: &AppHandle) {
    let state = app.state::<ShellState>();
    state.stop_and_join();
    save_current_state(app, &state);
    drop(app.remove_tray_by_id(TRAY_ID));
    app.exit(0);
}

fn save_current_state(app: &AppHandle, state: &ShellState) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        if let Ok(position) = window.outer_position() {
            let _ = state.store.apply(
                unix_now(),
                StateMutation::SetWindow(Some(WindowPlacement {
                    x: position.x,
                    y: position.y,
                })),
            );
        }
        if let Ok(always_on_top) = window.is_always_on_top() {
            let _ = state
                .store
                .apply(unix_now(), StateMutation::SetAlwaysOnTop(always_on_top));
        }
    }
}

fn show_tray_error(app: &AppHandle) {
    app.dialog()
        .message(TRAY_ERROR_MESSAGE)
        .title("Usage Widget")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

fn project_view(coordinator: &CollectionCoordinator, now: i64) -> WidgetView {
    WidgetView {
        providers: coordinator.current_snapshots(now),
    }
}

fn refresh_error() -> CommandError {
    CommandError {
        code: "refresh_failed",
        message: "Usage data could not be refreshed.",
    }
}

fn window_error() -> CommandError {
    CommandError {
        code: "window_unavailable",
        message: "The widget window is unavailable.",
    }
}

fn layout_for_count(count: usize) -> Layout {
    match count {
        0 => Layout::Empty,
        1 => Layout::Single,
        _ => Layout::Dual,
    }
}

pub fn height_for_layout(layout: Layout) -> f64 {
    match layout {
        Layout::Empty => 102.0,
        Layout::Single => 178.0,
        Layout::Dual => 254.0,
    }
}

pub fn clamp_position(
    saved: WindowPlacement,
    width: i32,
    title_height: i32,
    monitors: &[WorkArea],
) -> WindowPlacement {
    if monitors
        .iter()
        .any(|area| title_intersects(&saved, width, title_height, *area))
    {
        return saved;
    }
    let Some(area) = monitors
        .iter()
        .min_by_key(|area| distance_to_area(&saved, **area))
    else {
        return saved;
    };
    WindowPlacement {
        x: saved
            .x
            .clamp(area.left, area.right.saturating_sub(width).max(area.left)),
        y: saved.y.clamp(
            area.top,
            area.bottom.saturating_sub(title_height).max(area.top),
        ),
    }
}

fn title_intersects(
    position: &WindowPlacement,
    width: i32,
    title_height: i32,
    area: WorkArea,
) -> bool {
    position.x < area.right
        && position.x.saturating_add(width) > area.left
        && position.y < area.bottom
        && position.y.saturating_add(title_height) > area.top
}

fn distance_to_area(position: &WindowPlacement, area: WorkArea) -> i64 {
    let dx = if position.x < area.left {
        i64::from(area.left) - i64::from(position.x)
    } else if position.x > area.right {
        i64::from(position.x) - i64::from(area.right)
    } else {
        0
    };
    let dy = if position.y < area.top {
        i64::from(area.top) - i64::from(position.y)
    } else if position.y > area.bottom {
        i64::from(position.y) - i64::from(area.bottom)
    } else {
        0
    };
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
