//! "Quit Completely" stops the running Colima profiles, then quits the app.
//! The usual "Quit" quits the app only, and Colima continues to run.

use std::time::Duration;

use gpui::*;

use super::native::QuitCompletely;
use crate::colima;
use crate::runtime::Backend;

/// A `colima stop` that takes longer than this is stuck. The user can then
/// force the stop, or quit the app only.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// True while the app stops Colima before it quits. The window then shows
/// the progress.
#[derive(Clone, Copy, Default)]
pub struct Quitting(pub bool);

impl Global for Quitting {}

pub fn is_quitting(cx: &App) -> bool {
    cx.try_global::<Quitting>().is_some_and(|q| q.0)
}

pub fn init(cx: &mut App) {
    cx.set_global(Quitting(false));
    cx.on_action(|_: &QuitCompletely, cx| quit_completely(cx));
}

/// The result of one stop of all the profiles.
enum Outcome {
    Stopped,
    Failed(String),
    TimedOut,
}

fn quit_completely(cx: &mut App) {
    if is_quitting(cx) {
        return;
    }
    cx.set_global(Quitting(true));
    cx.refresh_windows();
    let backend = Backend::global(cx);
    cx.spawn(async move |cx| {
        let mut force = false;
        loop {
            let rx = backend.spawn(async move {
                let stop = tokio::task::spawn_blocking(move || colima::stop_running(force));
                tokio::time::timeout(STOP_TIMEOUT, stop).await
            });
            let outcome = match rx.await {
                Ok(Ok(Ok(Ok(())))) => Outcome::Stopped,
                Ok(Ok(Ok(Err(e)))) => Outcome::Failed(e.to_string()),
                Ok(Err(_)) => Outcome::TimedOut,
                _ => Outcome::Failed("The stop of Colima was cancelled.".into()),
            };
            let detail = match outcome {
                Outcome::Stopped => break,
                Outcome::Failed(e) => e,
                Outcome::TimedOut => format!(
                    "Colima did not stop in {} seconds.",
                    STOP_TIMEOUT.as_secs()
                ),
            };
            match ask(&detail, cx).await {
                0 => force = true,
                1 => break,
                _ => {
                    // Cancel. The app and Colima continue to run.
                    cx.update(|cx| {
                        cx.set_global(Quitting(false));
                        cx.refresh_windows();
                    });
                    return;
                }
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Asks what to do after a stop that failed. Returns the index of the
/// button: 0 forces the stop, 1 quits the app only, 2 cancels. With no
/// window, the app quits.
async fn ask(detail: &str, cx: &mut AsyncApp) -> usize {
    let Some(window) = cx.update(|cx| cx.windows().into_iter().next()) else {
        return 1;
    };
    let Ok(answer) = window
        .update(cx, |_, window, cx| {
            window.prompt(
                PromptLevel::Warning,
                "Colima did not stop",
                Some(detail),
                &[
                    PromptButton::new("Force Stop"),
                    PromptButton::new("Quit Without Stopping"),
                    PromptButton::Cancel("Cancel".into()),
                ],
                cx,
            )
        }) else {
        return 1;
    };
    answer.await.unwrap_or(1)
}
