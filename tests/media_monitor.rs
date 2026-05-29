use waywallen::media_monitor::{is_mpris_player_name, MediaPlaybackRegistry, PlaybackStatus};

#[test]
fn mpris_bus_name_filter_accepts_only_player_services() {
    assert!(is_mpris_player_name("org.mpris.MediaPlayer2.spotify"));
    assert!(is_mpris_player_name(
        "org.mpris.MediaPlayer2.vlc.instance123"
    ));

    assert!(!is_mpris_player_name("org.freedesktop.ScreenSaver"));
    assert!(!is_mpris_player_name("org.mpris.MediaPlayer2"));
    assert!(!is_mpris_player_name(":1.42"));
}

#[test]
fn registry_reports_any_player_in_playing_state() {
    let mut registry = MediaPlaybackRegistry::default();

    registry.set_player_status("org.mpris.MediaPlayer2.spotify", PlaybackStatus::Paused);
    assert!(!registry.any_playing());

    registry.set_player_status("org.mpris.MediaPlayer2.vlc", PlaybackStatus::Playing);
    assert!(registry.any_playing());

    registry.set_player_status("org.mpris.MediaPlayer2.vlc", PlaybackStatus::Stopped);
    assert!(!registry.any_playing());
}

#[test]
fn registry_clears_removed_players() {
    let mut registry = MediaPlaybackRegistry::default();

    registry.set_player_status("org.mpris.MediaPlayer2.spotify", PlaybackStatus::Playing);
    assert!(registry.any_playing());

    registry.remove_player("org.mpris.MediaPlayer2.spotify");
    assert!(!registry.any_playing());
}
