use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde::Serialize;
use serde_json::Value;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_http::reqwest;

// Хоткей. Формат: https://v2.tauri.app/plugin/global-shortcut/
const HOTKEY: &str = "CmdOrCtrl+Shift+Q";
// Языки перевода: sl — исходный (auto = определить), tl — целевой
const SOURCE_LANG: &str = "auto";
const TARGET_LANG: &str = "en";
// GET-запрос ограничен по длине URL — режем длинные тексты
const MAX_CHARS: usize = 4500;
const ERROR_TEXT: &str = "Ошибка перевода";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

// Защита от повторного нажатия, пока предыдущий перевод не закончился
static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct TranslationPayload {
    source: String,
    translated: String,
}

// Отпускаем модификаторы хоткея (Shift и т.д.), иначе вместо Ctrl+C уйдёт Ctrl+Shift+C
fn release_key(enigo: &mut Enigo, key: Key) {
    if let Err(e) = enigo.key(key, Direction::Release) {
        eprintln!("не удалось отпустить клавишу: {e}");
    }
}

// Эмуляция Ctrl+C (Cmd+C на macOS)
fn press_copy() -> Result<(), String> {
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| format!("инициализация enigo: {e}"))?;

    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    // Код клавиши C не зависит от раскладки (на Windows это VK_C = 0x43)
    #[cfg(target_os = "windows")]
    let copy_key = Key::Other(0x43);
    #[cfg(not(target_os = "windows"))]
    let copy_key = Key::Unicode('c');

    release_key(&mut enigo, Key::Shift);
    release_key(&mut enigo, modifier);

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| format!("нажатие модификатора: {e}"))?;
    let click = enigo.key(copy_key, Direction::Click);
    // Модификатор отпускаем в любом случае, чтобы он не «залип»
    release_key(&mut enigo, modifier);
    click.map_err(|e| format!("нажатие C: {e}"))
}

// Копирует выделенное и возвращает текст; прежний буфер восстанавливается.
// Блокирующая функция — вызывать через spawn_blocking.
fn copy_selection(app: &AppHandle) -> Result<String, String> {
    let clipboard = app.clipboard();
    let previous = clipboard.read_text().ok();

    // Очищаем буфер, чтобы по пустому результату понять, что ничего не выделено
    clipboard
        .write_text("")
        .map_err(|e| format!("очистка буфера: {e}"))?;
    std::thread::sleep(Duration::from_millis(50));

    press_copy()?;
    std::thread::sleep(Duration::from_millis(150)); // ждём, пока буфер обновится

    let text = match clipboard.read_text() {
        Ok(t) => t,
        Err(_) => String::new(), // пустой буфер — штатная ситуация
    };

    if let Some(prev) = previous {
        if let Err(e) = clipboard.write_text(prev) {
            eprintln!("не удалось восстановить буфер: {e}");
        }
    }
    Ok(text)
}

// Разбор ответа Google: текст склеивается из result[0][*][0]
fn parse_google(body: &str) -> Result<String, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("JSON: {e}"))?;
    let parts = json
        .get(0)
        .and_then(Value::as_array)
        .ok_or("неожиданный формат ответа")?;
    let translated: String = parts
        .iter()
        .filter_map(|p| p.get(0).and_then(Value::as_str))
        .collect();
    if translated.is_empty() {
        return Err("пустой перевод".into());
    }
    Ok(translated)
}

// Основной путь: запрос к Google Translate через reqwest
async fn translate(text: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("создание клиента: {e}"))?;

    let resp = client
        .get("https://translate.googleapis.com/translate_a/single")
        .header("Accept", "*/*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .query(&[
            ("client", "gtx"),
            ("sl", SOURCE_LANG),
            ("tl", TARGET_LANG),
            ("dt", "t"),
            ("q", text),
        ])
        .send()
        .await
        .map_err(|e| format!("сеть: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("чтение ответа: {e}"))?;

    // При ошибке показываем начало ответа: Google обычно пишет там причину
    if !status.is_success() {
        let preview: String = body.chars().take(300).collect();
        return Err(format!("HTTP {status}: {preview}"));
    }
    parse_google(&body)
}

// Запасной путь: системный curl (у него другой сетевой «отпечаток», чем у reqwest).
// Блокирующая функция — вызывать через spawn_blocking.
fn translate_via_curl(text: &str) -> Result<String, String> {
    let q = format!("q={text}");
    let sl = format!("sl={SOURCE_LANG}");
    let tl = format!("tl={TARGET_LANG}");

    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "--fail", "--max-time", "10", "-G"])
        .arg("https://translate.googleapis.com/translate_a/single")
        .args(["-d", "client=gtx", "-d", "dt=t"])
        .args(["-d", sl.as_str(), "-d", tl.as_str()])
        .args(["--data-urlencode", q.as_str()]);

    // Не показывать консольное окно на Windows
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let output = cmd.output().map_err(|e| format!("запуск curl: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl: {}", err.trim()));
    }
    parse_google(&String::from_utf8_lossy(&output.stdout))
}

fn center_fallback(window: &WebviewWindow) {
    if let Err(e) = window.center() {
        eprintln!("не удалось центрировать окно: {e}");
    }
}

// Центрирует окно на мониторе, где сейчас курсор
fn place_on_cursor_monitor(window: &WebviewWindow) {
    let cursor = match window.cursor_position() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("позиция курсора: {e}");
            return center_fallback(window);
        }
    };
    let monitors = match window.available_monitors() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("список мониторов: {e}");
            return center_fallback(window);
        }
    };
    let target = monitors.iter().find(|m| {
        let p = m.position();
        let s = m.size();
        cursor.x >= p.x as f64
            && cursor.x < p.x as f64 + s.width as f64
            && cursor.y >= p.y as f64
            && cursor.y < p.y as f64 + s.height as f64
    });
    let Some(monitor) = target else {
        return center_fallback(window);
    };
    let size = match window.outer_size() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("размер окна: {e}");
            return center_fallback(window);
        }
    };

    let x = monitor.position().x + (monitor.size().width as i32 - size.width as i32) / 2;
    let y = monitor.position().y + (monitor.size().height as i32 - size.height as i32) / 2;
    if let Err(e) = window.set_position(PhysicalPosition::new(x, y)) {
        eprintln!("не удалось переместить окно: {e}");
    }
}

// Отправляем результат в оверлей и показываем окно
fn show_overlay(app: &AppHandle, payload: TranslationPayload) {
    let Some(window) = app.get_webview_window("overlay") else {
        eprintln!("окно overlay не найдено");
        return;
    };
    if let Err(e) = app.emit_to("overlay", "translation-result", payload) {
        eprintln!("не удалось отправить событие: {e}");
    }
    place_on_cursor_monitor(&window);
    if let Err(e) = window.show() {
        eprintln!("не удалось показать окно: {e}");
    }
    if let Err(e) = window.set_focus() {
        eprintln!("не удалось передать фокус: {e}");
    }
}

async fn process_hotkey(app: &AppHandle) {
    // Эмуляция клавиш и sleep — в blocking-пуле, чтобы не занимать async-потоки
    let app_for_copy = app.clone();
    let source = match tauri::async_runtime::spawn_blocking(move || copy_selection(&app_for_copy))
        .await
    {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            eprintln!("копирование выделенного: {e}");
            return;
        }
        Err(e) => {
            eprintln!("поток копирования завершился с ошибкой: {e}");
            return;
        }
    };

    let source = source.trim().to_string();
    if source.is_empty() {
        return; // ничего не выделено — окно не открываем
    }

    let query: String = source.chars().take(MAX_CHARS).collect();
    let translated = match translate(&query).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("перевод (reqwest): {e}");
            let q = query.clone();
            match tauri::async_runtime::spawn_blocking(move || translate_via_curl(&q)).await {
                Ok(Ok(t)) => t,
                Ok(Err(e2)) => {
                    eprintln!("перевод (curl): {e2}");
                    ERROR_TEXT.to_string()
                }
                Err(e2) => {
                    eprintln!("поток curl завершился с ошибкой: {e2}");
                    ERROR_TEXT.to_string()
                }
            }
        }
    };

    show_overlay(app, TranslationPayload { source, translated });
}

fn handle_hotkey(app: AppHandle) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    // tauri::async_runtime::spawn вместо tokio::spawn: обработчик вызывается вне
    // контекста tokio, а tokio::spawn там паникует. Runtime тот же (tokio внутри Tauri).
    tauri::async_runtime::spawn(async move {
        process_hotkey(&app).await;
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit])?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("Переводчик (Ctrl+Shift+Q)"); // можете изменять под себя
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "quit" {
                app.exit(0);
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // TODO: автозапуск включается вызовом app.autolaunch().enable() (трейт ManagerExt);
        // здесь плагин только подключён.
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        handle_hotkey(app.clone());
                    }
                })
                .build(),
        )
        .setup(|app| {
            // macOS: без иконки в Dock
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            if let Err(e) = build_tray(app) {
                eprintln!("не удалось создать иконку в трее: {e}");
            }
            if let Err(e) = app.global_shortcut().register(HOTKEY) {
                eprintln!("не удалось зарегистрировать хоткей {HOTKEY}: {e}");
            }
            // Автозапуск только в релизной сборке: в dev он зарегистрировал бы
            // debug-exe, который не работает без Vite и пропадёт после очистки target
            #[cfg(not(debug_assertions))]
            {
                let autolaunch = app.autolaunch();
                match autolaunch.is_enabled() {
                    Ok(true) => {}
                    Ok(false) => {
                        if let Err(e) = autolaunch.enable() {
                            eprintln!("не удалось включить автозапуск: {e}");
                        }
                    }
                    Err(e) => eprintln!("не удалось проверить автозапуск: {e}"),
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("ошибка запуска приложения: {e}");
    }
}
