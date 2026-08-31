use std::time::{SystemTime, UNIX_EPOCH};

use usage_widget::{
    providers::claude::{capture_mode_from_args, run_claude_capture},
    state_store::{default_state_path, JsonStateStore},
};

fn main() {
    let args = std::env::args_os().collect::<Vec<_>>();
    if capture_mode_from_args(&args) {
        let Ok(path) = default_state_path() else {
            eprintln!("USAGE: LOCAL STATE ERROR");
            std::process::exit(2);
        };
        let store = JsonStateStore::new(path);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        let exit = run_claude_capture(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            std::io::stderr().lock(),
            &store,
            now,
        );
        std::process::exit(exit);
    }

    eprintln!("Usage Widget GUI is not available in this intermediate build.");
    std::process::exit(1);
}
