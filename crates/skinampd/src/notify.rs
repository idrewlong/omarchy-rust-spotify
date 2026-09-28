//! Track-change notifications, straight over D-Bus with the hints Omarchy's
//! notification daemon understands (the same ones `omarchy-notification-send`
//! builds). No process is spawned per track.

use std::collections::HashMap;
use std::sync::Arc;

use mpris_server::zbus::{self, zvariant::Value};
use skinamp_proto::{PlayerState, Status};
use tokio::sync::{broadcast, watch};

use crate::config::{Config, Notifications};
use crate::state::Update;

const GLYPH: &str = "󰓇";

/// The command that opens the player, as the JSON argv Omarchy's
/// notification daemon runs on click.
fn open_player_argv() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    serde_json::json!([
        "omarchy-launch-or-focus-tui",
        "--app-id=org.omarchy.skinamp",
        format!("{home}/.local/bin/skinamp"),
        "tui"
    ])
    .to_string()
}

struct Notifier {
    conn: zbus::Connection,
    /// Reused so skips update one notification instead of stacking.
    last_id: u32,
}

impl Notifier {
    async fn show(&mut self, summary: &str, body: &str, image: Option<&str>) -> zbus::Result<()> {
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(0));
        hints.insert("omarchy-glyph", Value::from(GLYPH));
        if let Some(path) = image {
            hints.insert("image-path", Value::from(path));
        }
        // Clicking the notification opens (or focuses) the player, the way
        // omarchy-notification-send --exec does.
        hints.insert("omarchy-exec-argv", Value::from(open_player_argv()));
        let reply = self
            .conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Skinamp",
                    self.last_id,
                    // The app icon, shown when there's no cover yet.
                    "skinamp",
                    summary,
                    body,
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            )
            .await?;
        self.last_id = reply.body().deserialize::<u32>()?;
        Ok(())
    }
}

/// What, if anything, this update should notify about: a new track while
/// playing, or the cover arriving for the track just announced.
fn wants_notification(old: &PlayerState, new: &PlayerState) -> bool {
    let Some(track) = &new.track else {
        return false;
    };
    if !matches!(new.status, Status::Playing | Status::Loading) {
        return false;
    }
    match &old.track {
        Some(prev) if prev.uri == track.uri => {
            prev.cover_path.is_none() && track.cover_path.is_some()
        }
        _ => true,
    }
}

pub async fn run(mut updates: broadcast::Receiver<Arc<Update>>, config: watch::Receiver<Config>) {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("notifications disabled: no session bus ({e})");
            return;
        }
    };
    let mut notifier = Notifier { conn, last_id: 0 };
    let mut last = PlayerState::default();
    // The track we last announced: a cover arriving later only updates it.
    let mut announced: Option<String> = None;
    loop {
        let update = match updates.recv().await {
            Ok(u) => u,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let new = &update.state;
        let enabled = config.borrow().notifications != Notifications::Off;
        if enabled && wants_notification(&last, new) {
            let t = new.track.as_ref().expect("checked");
            let cover_only = announced.as_deref() == Some(t.uri.as_str());
            if !cover_only || t.cover_path.is_some() {
                let artists = t.artists.join(", ");
                if let Err(e) = notifier
                    .show(&t.name, &artists, t.cover_path.as_deref())
                    .await
                {
                    tracing::warn!("notification failed: {e}");
                }
                announced = Some(t.uri.clone());
            }
        }
        last = new.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skinamp_proto::Track;

    fn playing(uri: &str, cover: Option<&str>) -> PlayerState {
        PlayerState {
            status: Status::Playing,
            track: Some(Track {
                uri: uri.into(),
                cover_path: cover.map(Into::into),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn new_track_while_playing_notifies() {
        assert!(wants_notification(
            &PlayerState::default(),
            &playing("a", None)
        ));
        assert!(wants_notification(&playing("a", None), &playing("b", None)));
    }

    #[test]
    fn cover_arriving_updates_once() {
        assert!(wants_notification(
            &playing("a", None),
            &playing("a", Some("/c.jpg"))
        ));
        assert!(!wants_notification(
            &playing("a", Some("/c.jpg")),
            &playing("a", Some("/c.jpg"))
        ));
    }

    #[test]
    fn paused_or_same_track_does_not_notify() {
        let mut paused = playing("b", None);
        paused.status = Status::Paused;
        assert!(!wants_notification(&playing("a", None), &paused));
        assert!(!wants_notification(
            &playing("a", None),
            &playing("a", None)
        ));
    }
}
