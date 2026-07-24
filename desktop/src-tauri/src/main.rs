use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tauri::{Emitter, Listener, Manager};

mod startup;
use startup::{
    apply_setup, copy_diagnostics, open_config, preview_setup, retry_startup, startup_status,
    StartupStore,
};

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

fn main() {
    tauri::Builder::default()
        .manage(StartupStore::from_environment())
        .invoke_handler(tauri::generate_handler![
            startup_status,
            preview_setup,
            apply_setup,
            retry_startup,
            open_config,
            copy_diagnostics
        ])
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
            let startup_handle = app.handle().clone();

            tauri::async_runtime::spawn(async move {
                let state = startup_handle.state::<StartupStore>();
                if startup::run_startup(window, &state).await.is_err() {
                    eprintln!("xv desktop startup entered recovery");
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
