use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tauri::{Emitter, Listener, Manager};

#[derive(Debug, PartialEq, Eq)]
enum CloseDecision {
    Allow,
    AskPage,
    DenyWhileSaving,
}

#[derive(Default)]
struct DesktopSavePending(AtomicBool);

impl DesktopSavePending {
    fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn set_from_payload(&self, payload: &str) -> Result<(), serde_json::Error> {
        self.0
            .store(serde_json::from_str(payload)?, Ordering::Release);
        Ok(())
    }
}

fn close_decision(save_pending: bool, approved: bool) -> CloseDecision {
    if save_pending {
        CloseDecision::DenyWhileSaving
    } else if approved {
        CloseDecision::Allow
    } else {
        CloseDecision::AskPage
    }
}

fn project_directory() -> Result<Option<PathBuf>, String> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--project" {
            return args
                .next()
                .map(PathBuf::from)
                .map(Some)
                .ok_or_else(|| "--project requires a directory path".to_string());
        }
    }

    Ok(std::env::var_os("XV_DESKTOP_PROJECT").map(PathBuf::from))
}

fn show_startup_error(window: &tauri::WebviewWindow, error: &str) {
    let message = serde_json::to_string(error).unwrap_or_else(|_| "\"startup failed\"".into());
    let _ = window.eval(format!("window.showStartupError({message})"));
}

async fn start_server(window: tauri::WebviewWindow) -> Result<(), String> {
    if let Some(project) = project_directory()? {
        std::env::set_current_dir(&project).map_err(|e| {
            format!(
                "could not use project directory '{}': {e}",
                project.display()
            )
        })?;
    }

    let config = crosstache::config::load_config_no_validation()
        .await
        .map_err(|e| e.to_string())?;
    let server = crosstache::web::prepare_web(config, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let url = server
        .url()
        .parse()
        .map_err(|e| format!("invalid embedded UI URL: {e}"))?;

    #[cfg(debug_assertions)]
    println!("xv desktop embedded UI: {}", server.url());

    window
        .navigate(url)
        .map_err(|e| format!("could not open the embedded UI: {e}"))?;

    server.serve().await.map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .ok_or("main window was not created")?;
            let close_approved = Arc::new(AtomicBool::new(false));
            let save_pending = Arc::new(DesktopSavePending::default());
            let pending_state = save_pending.clone();
            window.listen("xv://save-pending-changed", move |event| {
                let _ = pending_state.set_from_payload(event.payload());
            });
            let approval_window = window.clone();
            let approval_flag = close_approved.clone();
            window.listen("xv://window-close-approved", move |_| {
                approval_flag.store(true, Ordering::Release);
                let _ = approval_window.close();
            });
            let close_window = window.clone();
            let close_pending = save_pending.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let approved = close_approved.swap(false, Ordering::AcqRel);
                    match close_decision(close_pending.get(), approved) {
                        CloseDecision::Allow => {}
                        CloseDecision::AskPage => {
                            api.prevent_close();
                            let _ = close_window.emit("xv://window-close-requested", ());
                        }
                        CloseDecision::DenyWhileSaving => api.prevent_close(),
                    }
                }
            });
            let error_window = window.clone();

            tauri::async_runtime::spawn(async move {
                if let Err(error) = start_server(window).await {
                    eprintln!("xv desktop startup failed: {error}");
                    show_startup_error(&error_window, &error);
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running xv desktop");
}

#[cfg(test)]
mod tests {
    use super::{close_decision, CloseDecision, DesktopSavePending};

    #[test]
    fn close_decision_allows_an_approved_close_and_asks_the_page_otherwise() {
        assert_eq!(close_decision(false, true), CloseDecision::Allow);
        assert_eq!(close_decision(false, false), CloseDecision::AskPage);
        assert_eq!(close_decision(true, false), CloseDecision::DenyWhileSaving);
    }

    #[test]
    fn close_decision_denies_while_page_save_is_pending() {
        let state = DesktopSavePending::default();
        assert_eq!(close_decision(state.get(), false), CloseDecision::AskPage);
        state.set_from_payload("true").unwrap();
        assert_eq!(
            close_decision(state.get(), true),
            CloseDecision::DenyWhileSaving
        );
        state.set_from_payload("false").unwrap();
        assert_eq!(close_decision(state.get(), true), CloseDecision::Allow);
    }
}
