use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde::Serialize;
use serde_json::Value;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_http::reqwest;

// Хоткей. Формат: https://v2.tauri.app/plugin/global-shortcut/
const HOTKEY: &str = "CmdOrCtrl+Shift+T";
// Языки перевода: sl — исходный (auto = определить), tl — целевой
const SOURCE_LANG: &str = "auto";
const TARGET_LANG: &str = "en";
// GET-запрос ограничен по длине URL — режем длинные тексты
const MAX_CHARS: usize = 4500;
const ERROR_TEXT: &str = "Ошибка перевода";

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

    release_key(&mut enigo, Key::Shift);
    release_key(&mut enigo, modifier);

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| format!("нажатие модификатора: {e}"))?;
    let click = enigo.key(Key::Unicode('c'), Direction::Click);
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

// Запрос к Google Translate; ответ — массив, текст склеивается из result[0][*][0]
async fn translate(text: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = client
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[
            ("client", "gtx"),
            ("sl", SOURCE_LANG),
            ("tl", TARGET_LANG),
            ("dt", "t"),
            ("q", text),
        ])
        .send()
        .await
        .map_err(|e| format!("сеть: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP: {e}"))?
        .text()
        .await
        .map_err(|e| format!("чтение ответа: {e}"))?;

    let json: Value = serde_json::from_str(&body).map_err(|e| format!("JSON: {e}"))?;
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

// Отправляем результат в оверлей и показываем окно
fn show_overlay(app: &AppHandle, payload: TranslationPayload) {
    let Some(window) = app.get_webview_window("overlay") else {
        eprintln!("окно overlay не найдено");
        return;
    };
    if let Err(e) = app.emit_to("overlay", "translation-result", payload) {
        eprintln!("не удалось отправить событие: {e}");
    }
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
            eprintln!("перевод: {e}");
            ERROR_TEXT.to_string()
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
        .tooltip("Переводчик (Ctrl+Shift+T)");
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
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("ошибка запуска приложения: {e}");
    }
}
