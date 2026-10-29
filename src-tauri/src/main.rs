// Без этого на Windows в релизе появится консольное окно
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    translator_lib::run()
}
