// Release builds get no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    downplayed_lib::run();
}
