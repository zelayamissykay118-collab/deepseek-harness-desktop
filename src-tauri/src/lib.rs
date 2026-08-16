mod bridge;
mod config;
mod core;
mod logger;
mod service;
mod task;

use core::utils::show_window;
use tauri::{
    ipc::Invoke,
    menu::{Menu, MenuEvent, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Manager, Runtime, Wry,
};

#[cfg(windows)]
use std::{os::windows::ffi::OsStrExt, thread, time::Duration};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, BOOL, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM},
    System::Threading::CreateMutexW,
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, SetForegroundWindow, ShowWindowAsync,
        SW_RESTORE,
    },
};

#[cfg(windows)]
const SINGLE_INSTANCE_MUTEX: &str =
    "Local\\io.github.hairyf.deepseek-harness-desktop-single-instance";
#[cfg(windows)]
const MAIN_WINDOW_TITLE: &str = "Deepseek Harness Desktop";

/// Windows fallback for the Tauri single-instance plugin. The plugin's event
/// target window can fail to register on some systems while its mutex remains,
/// which lets later launches continue as full app instances. This guard exits
/// before Tauri starts and restores the existing native window directly.
#[cfg(windows)]
struct WindowsSingleInstance(HANDLE);

#[cfg(windows)]
impl WindowsSingleInstance {
    fn acquire() -> Option<Self> {
        let mutex_name = std::ffi::OsStr::new(SINGLE_INSTANCE_MUTEX)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };

        if handle.is_null() {
            log::error!("failed to create the Windows single-instance mutex");
            return Some(Self(handle));
        }

        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            show_existing_main_window();
            None
        } else {
            Some(Self(handle))
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsSingleInstance {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn show_existing_main_window() {
    for _ in 0..20 {
        let mut found = false;
        unsafe {
            EnumWindows(
                Some(find_and_show_main_window),
                &mut found as *mut bool as LPARAM,
            );
        }
        if found {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
unsafe extern "system" fn find_and_show_main_window(window: HWND, state: LPARAM) -> BOOL {
    let title_length = GetWindowTextLengthW(window);
    if title_length <= 0 {
        return 1;
    }

    let mut title = vec![0_u16; title_length as usize + 1];
    let copied = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
    if copied <= 0 || String::from_utf16_lossy(&title[..copied as usize]) != MAIN_WINDOW_TITLE {
        return 1;
    }

    ShowWindowAsync(window, SW_RESTORE);
    SetForegroundWindow(window);
    *(state as *mut bool) = true;
    0
}

// setup app
fn setup(app_handle: tauri::AppHandle) {
    // 启动进程监控（tick 检测 dsh 服务状态）
    service::scheduler::start(&app_handle);

    // 开机自启动：已安装且开启 auto_start 时拉起服务
    tauri::async_runtime::spawn(async move {
        let setting = config::get_store_dat_setting(&app_handle);
        if !setting.auto_start {
            log::debug!("auto_start disabled, skipping startup");
            return;
        }
        if let Err(e) = service::workflow::start(app_handle).await {
            log::error!("start failed: {}", e);
        }
    });
}

// setup tray
fn tray<R: Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    // 使用默认窗口图标
    let icon = app.default_window_icon().unwrap().clone();

    // 构建菜单
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open", "打开面板", true, None::<&str>)?,
            &MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?,
        ],
    )?;

    fn handle_menu_event<R: Runtime>(app: &tauri::AppHandle<R>, event: &MenuEvent) {
        match event.id().as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    show_window(&window);
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        }
    }

    fn handle_tray_icon_event<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, event: &TrayIconEvent) {
        let app = tray.app_handle();
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } => {
                if let Some(window) = app.get_webview_window("main") {
                    show_window(&window);
                }
            }
            _ => {}
        }
    }

    // 构建托盘图标
    let _ = TrayIconBuilder::new()
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Deepseek Harness Desktop")
        .on_menu_event(move |app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(move |tray, event| handle_tray_icon_event(&tray, &event))
        .build(app)?;

    Ok(())
}

// configure invoke handler
fn handler() -> impl Fn(Invoke<Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        bridge::cmd::install_dependencies,
        bridge::cmd::check_dsh_update,
        bridge::cmd::launch_harness,
        bridge::cmd::shutdown_harness,
        bridge::cmd::restart_harness,
        bridge::cmd::get_dsh_status,
        bridge::cmd::proxy_health_check,
        bridge::cmd::get_runtime_info,
        bridge::cmd::get_app_config,
        bridge::cmd::update_app_config,
        bridge::cmd::open_in_browser,
        bridge::cmd::copy_service_url,
        bridge::cmd::reveal_data_dir,
        bridge::cmd::read_service_logs,
        bridge::cmd::clear_service_logs,
        bridge::cmd::set_language,
        bridge::cmd::toggle_sidebar,
        bridge::cmd::get_dsh_theme,
    ]
}

// configure tauri builder
fn builder() -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        // 必须最先注册：重复启动时复用已有进程，并唤醒主窗口。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                show_window(&window);
            }
        }))
        // 点击关闭按钮时隐藏到托盘而不是退出程序
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        // Opener plugin
        .plugin(tauri_plugin_opener::init())
        // FS plugin
        .plugin(tauri_plugin_fs::init())
        // Simple Store plugin
        .plugin(tauri_plugin_store::Builder::new().build())
        // Clipboard plugin
        .plugin(tauri_plugin_clipboard_manager::init())
}

// run app
pub fn run() {
    // 初始化日志系统
    logger::init();

    #[cfg(windows)]
    let _single_instance = match WindowsSingleInstance::acquire() {
        Some(guard) => guard,
        None => return,
    };

    builder()
        .setup(|app| {
            tray(&app.handle()).unwrap();
            setup(app.handle().clone());
            Ok(())
        })
        .invoke_handler(handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // 退出时回收 Harness 进程：不回收的话，node 进程会在应用退出后
            // 残留并把原生模块 DLL（如 sharp 的 libvips-42.dll）锁在内存，
            // 下次启动重新解压时会失败（Windows os error 32）
            if let tauri::RunEvent::Exit = event {
                let setting = config::get_store_dat_setting(app_handle);
                if setting.installed {
                    service::workflow::stop_on_exit(app_handle.clone(), setting.port);
                }
            }
        });
}
