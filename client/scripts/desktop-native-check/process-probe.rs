fn main() -> Result<(), String> {
    if std::env::args().any(|arg| arg == "--echo-native-helper") {
        return echo_desktop_native_check::run_helper();
    }
    tokio::runtime::Runtime::new()
        .map_err(|error| error.to_string())?
        .block_on(echo_desktop_native_check::probe_helper_process())
}
