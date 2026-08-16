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
