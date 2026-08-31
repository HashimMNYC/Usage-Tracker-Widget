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
use winreg::{
    enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ},
    RegKey, RegValue,
};

use crate::{
    claude_settings::{ClaudeSettingsManager, ClaudeTrackingState},
    coordinator::{start_supervisor, CollectionCoordinator, CollectorSupervisor},
    model::ProviderSnapshot,
    paths::resolve_codex_roots,
    providers::codex::CodexCollector,
    startup::{
        disable_startup, enable_startup, repair_startup, startup_status, StartupError,
        StartupRegistration, StartupRegistrationSnapshot,
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
pub const STARTUP_MANUAL_REVIEW_MESSAGE: &str =
    "Launch at Sign-in needs manual review in Windows Startup Apps.";
const CLAUDE_MANUAL_REVIEW_MESSAGE: &str =
    "Claude Tracking needs manual review in Claude settings.";
pub const SHOW_HIDE_LABEL: &str = "Show/Hide";
pub const REFRESH_LABEL: &str = "Refresh";
pub const ALWAYS_ON_TOP_LABEL: &str = "Always on Top";
const STARTUP_ENTRY_NAME: &str = "Usage Widget";
const STARTUP_RUN_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
const STARTUP_APPROVED_KEY: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

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
pub struct TrayActionState {
    pub label: &'static str,
    pub checked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIntegrationAction {
    Enable,
    Disable,
    Repair,
    ManualReview,
}

enum TrayActionError {
    General,
    StartupManualReview,
    ClaudeManualReview,
}

impl TrayActionError {
    const fn message(&self) -> &'static str {
        match self {
            Self::General => TRAY_ERROR_MESSAGE,
            Self::StartupManualReview => STARTUP_MANUAL_REVIEW_MESSAGE,
            Self::ClaudeManualReview => CLAUDE_MANUAL_REVIEW_MESSAGE,
        }
    }
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
    fn snapshot(&self) -> Result<StartupRegistrationSnapshot, StartupError> {
        Ok(StartupRegistrationSnapshot {
            run_value: read_startup_value(STARTUP_RUN_KEY, REG_SZ)?,
            startup_approved_value: read_startup_value(STARTUP_APPROVED_KEY, REG_BINARY)?,
        })
    }

    fn restore(&self, snapshot: &StartupRegistrationSnapshot) -> Result<(), StartupError> {
        restore_startup_snapshot_with(snapshot, restore_startup_value)
    }

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

fn restore_startup_snapshot_with(
    snapshot: &StartupRegistrationSnapshot,
    mut restore: impl FnMut(&str, RegType, Option<&[u8]>) -> Result<(), StartupError>,
) -> Result<(), StartupError> {
    let run_result = restore(STARTUP_RUN_KEY, REG_SZ, snapshot.run_value.as_deref());
    let approved_result = restore(
        STARTUP_APPROVED_KEY,
        REG_BINARY,
        snapshot.startup_approved_value.as_deref(),
    );
    if run_result.is_err() || approved_result.is_err() {
        Err(StartupError::OperationFailed)
    } else {
        Ok(())
    }
}

fn read_startup_value(key: &str, expected_type: RegType) -> Result<Option<Vec<u8>>, StartupError> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let registry = match hkcu.open_subkey_with_flags(key, KEY_READ) {
        Ok(registry) => registry,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StartupError::Unavailable),
    };
    match registry.get_raw_value(STARTUP_ENTRY_NAME) {
        Ok(value) if value.vtype == expected_type => Ok(Some(value.bytes)),
        Ok(_) => Err(StartupError::Unavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(StartupError::Unavailable),
    }
}

fn restore_startup_value(
    key: &str,
    value_type: RegType,
    bytes: Option<&[u8]>,
) -> Result<(), StartupError> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let registry = match hkcu.open_subkey_with_flags(key, KEY_SET_VALUE) {
        Ok(registry) => registry,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && bytes.is_none() => {
            return Ok(())
        }
        Err(_) => return Err(StartupError::OperationFailed),
    };
    match bytes {
        Some(bytes) => registry
            .set_raw_value(
                STARTUP_ENTRY_NAME,
                &RegValue {
                    bytes: bytes.to_vec(),
                    vtype: value_type,
                },
            )
            .map_err(|_| StartupError::OperationFailed),
        None => match registry.delete_value(STARTUP_ENTRY_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StartupError::OperationFailed),
        },
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
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name(STARTUP_ENTRY_NAME)
                .build(),
        )
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
                show_tray_error(app.handle(), TRAY_ERROR_MESSAGE);
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

    let startup_action = startup_tray_action_state(startup_status);
    let show_hide = MenuItem::with_id(app, "show_hide", SHOW_HIDE_LABEL, true, None::<&str>)?;
    let refresh_item = MenuItem::with_id(app, "refresh", REFRESH_LABEL, true, None::<&str>)?;
    let always_on_top = CheckMenuItem::with_id(
        app,
        "always_on_top",
        ALWAYS_ON_TOP_LABEL,
        true,
        persisted.always_on_top,
        None::<&str>,
    )?;
    let launch_at_sign_in = CheckMenuItem::with_id(
        app,
        "launch_at_sign_in",
        startup_action.label,
        true,
        startup_action.checked,
        None::<&str>,
    )?;
    let claude_tracking = MenuItem::with_id(
        app,
        "claude_tracking",
        claude_tray_label(claude_status),
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
    let startup_click_handle = launch_at_sign_in.clone();
    let claude_click_handle = claude_tracking.clone();
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
            if let Err(error) = result {
                show_tray_error(app, error.message());
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                if let Err(error) = sync_integration_items(
                    tray.app_handle(),
                    &startup_click_handle,
                    &claude_click_handle,
                ) {
                    show_tray_error(tray.app_handle(), error.message());
                }
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn toggle_main_window(app: &AppHandle) -> Result<(), TrayActionError> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or(TrayActionError::General)?;
    if window.is_visible().map_err(|_| TrayActionError::General)? {
        window.hide().map_err(|_| TrayActionError::General)
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

fn toggle_topmost(
    app: &AppHandle,
    item: &CheckMenuItem<tauri::Wry>,
) -> Result<(), TrayActionError> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or(TrayActionError::General)?;
    let enabled = !window
        .is_always_on_top()
        .map_err(|_| TrayActionError::General)?;
    window
        .set_always_on_top(enabled)
        .map_err(|_| TrayActionError::General)?;
    app.state::<ShellState>()
        .store
        .apply(unix_now(), StateMutation::SetAlwaysOnTop(enabled))
        .map_err(|_| TrayActionError::General)?;
    item.set_checked(enabled)
        .map_err(|_| TrayActionError::General)
}

fn toggle_startup(
    app: &AppHandle,
    item: &CheckMenuItem<tauri::Wry>,
) -> Result<(), TrayActionError> {
    let state = app.state::<ShellState>();
    let now = unix_now();
    let current = std::env::current_exe().map_err(|_| TrayActionError::General)?;
    let persisted = state
        .store
        .load(now)
        .map_err(|_| TrayActionError::General)?;
    let status = startup_integration_status(&state, &current, &persisted);
    let result = match startup_tray_action(status) {
        TrayIntegrationAction::Enable => {
            enable_startup(state.startup.as_ref(), state.store.as_ref(), &current, now)
        }
        TrayIntegrationAction::Disable => {
            disable_startup(state.startup.as_ref(), state.store.as_ref(), now)
        }
        TrayIntegrationAction::Repair => {
            repair_startup(state.startup.as_ref(), state.store.as_ref(), &current, now)
        }
        TrayIntegrationAction::ManualReview => {
            sync_startup_item(item, status)?;
            return Err(TrayActionError::StartupManualReview);
        }
    };
    match result {
        Ok(next) => sync_startup_item(item, next),
        Err(_) => {
            let after = state
                .store
                .load(now)
                .ok()
                .map(|persisted| startup_integration_status(&state, &current, &persisted))
                .unwrap_or(IntegrationStatus::Conflict);
            sync_startup_item(item, after)?;
            Err(TrayActionError::General)
        }
    }
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

fn toggle_claude(app: &AppHandle, item: &MenuItem<tauri::Wry>) -> Result<(), TrayActionError> {
    let state = app.state::<ShellState>();
    let now = unix_now();
    let current = std::env::current_exe().map_err(|_| TrayActionError::General)?;
    let status = state.claude_settings.status(&current, now);
    let result = match claude_tray_action(status) {
        TrayIntegrationAction::Enable => state.claude_settings.enable(&current, now),
        TrayIntegrationAction::Disable => state.claude_settings.disable(now),
        TrayIntegrationAction::Repair => state.claude_settings.repair(&current, now),
        TrayIntegrationAction::ManualReview => {
            item.set_text(claude_tray_label(status))
                .map_err(|_| TrayActionError::General)?;
            return Err(TrayActionError::ClaudeManualReview);
        }
    };
    match result {
        Ok(next) => item
            .set_text(claude_tray_label(next))
            .map_err(|_| TrayActionError::General),
        Err(_) => {
            let after = state.claude_settings.status(&current, now);
            item.set_text(claude_tray_label(after))
                .map_err(|_| TrayActionError::General)?;
            Err(TrayActionError::General)
        }
    }
}

pub fn startup_tray_action_state(status: IntegrationStatus) -> TrayActionState {
    match status {
        IntegrationStatus::Disabled | IntegrationStatus::Conflict => TrayActionState {
            label: "Launch at Sign-in",
            checked: false,
        },
        IntegrationStatus::Enabled => TrayActionState {
            label: "Launch at Sign-in",
            checked: true,
        },
        IntegrationStatus::NeedsRepair => TrayActionState {
            label: "Repair Launch at Sign-in",
            checked: false,
        },
    }
}

pub fn startup_tray_action(status: IntegrationStatus) -> TrayIntegrationAction {
    match status {
        IntegrationStatus::Disabled => TrayIntegrationAction::Enable,
        IntegrationStatus::Enabled => TrayIntegrationAction::Disable,
        IntegrationStatus::NeedsRepair => TrayIntegrationAction::Repair,
        IntegrationStatus::Conflict => TrayIntegrationAction::ManualReview,
    }
}

pub fn claude_tray_action(status: ClaudeTrackingState) -> TrayIntegrationAction {
    match status {
        ClaudeTrackingState::Disabled => TrayIntegrationAction::Enable,
        ClaudeTrackingState::Enabled => TrayIntegrationAction::Disable,
        ClaudeTrackingState::NeedsRepair => TrayIntegrationAction::Repair,
        ClaudeTrackingState::Conflict => TrayIntegrationAction::ManualReview,
    }
}

pub fn claude_tray_label(status: ClaudeTrackingState) -> &'static str {
    match status {
        ClaudeTrackingState::Disabled => "Enable Claude Tracking",
        ClaudeTrackingState::Enabled => "Disable Claude Tracking",
        ClaudeTrackingState::NeedsRepair | ClaudeTrackingState::Conflict => {
            "Repair Claude Tracking"
        }
    }
}

fn sync_startup_item(
    item: &CheckMenuItem<tauri::Wry>,
    status: IntegrationStatus,
) -> Result<(), TrayActionError> {
    let action = startup_tray_action_state(status);
    item.set_text(action.label)
        .map_err(|_| TrayActionError::General)?;
    item.set_checked(action.checked)
        .map_err(|_| TrayActionError::General)
}

fn sync_integration_items(
    app: &AppHandle,
    startup_item: &CheckMenuItem<tauri::Wry>,
    claude_item: &MenuItem<tauri::Wry>,
) -> Result<(), TrayActionError> {
    let state = app.state::<ShellState>();
    let now = unix_now();
    let current = std::env::current_exe().map_err(|_| TrayActionError::General)?;
    let persisted = state
        .store
        .load(now)
        .map_err(|_| TrayActionError::General)?;
    sync_startup_item(
        startup_item,
        startup_integration_status(&state, &current, &persisted),
    )?;
    claude_item
        .set_text(claude_tray_label(
            state.claude_settings.status(&current, now),
        ))
        .map_err(|_| TrayActionError::General)
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

fn show_tray_error(app: &AppHandle, message: &'static str) {
    app.dialog()
        .message(message)
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

#[cfg(test)]
mod startup_restore_tests {
    use super::*;

    #[test]
    fn restore_attempts_startup_approved_after_run_restoration_fails() {
        let snapshot = StartupRegistrationSnapshot {
            run_value: Some(vec![1, 2, 3]),
            startup_approved_value: Some(vec![4, 5, 6]),
        };
        let mut attempted = Vec::new();

        let result = restore_startup_snapshot_with(&snapshot, |key, _value_type, _bytes| {
            attempted.push(key.to_owned());
            if key == STARTUP_RUN_KEY {
                Err(StartupError::OperationFailed)
            } else {
                Ok(())
            }
        });

        assert_eq!(result, Err(StartupError::OperationFailed));
        assert_eq!(attempted, vec![STARTUP_RUN_KEY, STARTUP_APPROVED_KEY]);
    }
}
