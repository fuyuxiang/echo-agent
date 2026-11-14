// Prevent additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--echo-native-helper") {
        if echoagent_lib::run_native_helper().is_err() {
            std::process::exit(1);
        }
        return;
    }
    echoagent_lib::run()
}
