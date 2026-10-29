# Highlight text translator

Фоновый переводчик выделенного текста: выделить → `Ctrl+Shift+T` (`Cmd+Shift+T` на macOS) → оверлей с переводом. Стек: Rust + Tauri v2 + чистый JS (Vite используется только как сборщик).

## Сборка

Нужны Node.js, Rust и [зависимости Tauri](https://v2.tauri.app/start/prerequisites/).

```bash
npm install
cargo install tauri-cli --version "^2"   # если ещё нет; либо используйте npm run tauri ...
cargo tauri icon путь/к/иконка-1024.png  # один раз: генерирует src-tauri/icons/*
cargo tauri dev                          # разработка
cargo tauri build                        # релизная сборка
```

## Как поменять хоткей

В `src-tauri/src/lib.rs` измените константу `HOTKEY`, например `"CmdOrCtrl+Alt+T"`.

## Как поменять язык перевода

В `src-tauri/src/lib.rs` измените `TARGET_LANG` (параметр `tl=` в URL запроса; `"en"`, `"ru"`, `"de"` ...)
и при необходимости `SOURCE_LANG` (`sl=`, `"auto"` — автоопределение).

## Заметки

- **macOS**: нужно выдать приложению доступ в «Конфиденциальность → Универсальный доступ» (иначе эмуляция Cmd+C не сработает). Прозрачное окно требует `macOSPrivateApi` (уже включено; несовместимо с App Store).
- **Linux**: глобальные хоткеи и эмуляция клавиш работают на X11; под Wayland — ограниченно или нет.
- Неофициальный эндпоинт Google Translate может быть ограничен или изменён без предупреждения.
- Автозапуск: плагин подключён, включение — см. TODO в `lib.rs`.
