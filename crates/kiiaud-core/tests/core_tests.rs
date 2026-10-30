// SPDX-License-Identifier: MIT OR Apache-2.0

use kiiaud_core::{AudioSource, DsdRate, KiiaudConfig, PathPolicy, Queue, strip_uri_auth};
use std::path::PathBuf;

#[test]
fn test_queue_enqueue_and_navigation() {
    let mut queue = Queue::new();
    assert!(queue.is_empty());

    let id1 = queue.enqueue(
        AudioSource::from_uri_or_path("/music/track1.wv"),
        Some("Track 1".to_owned()),
        Some("Artist A".to_owned()),
    );
    let id2 = queue.enqueue(
        AudioSource::from_uri_or_path("/music/track2.flac"),
        Some("Track 2".to_owned()),
        Some("Artist B".to_owned()),
    );
    let id3 = queue.enqueue(
        AudioSource::from_uri_or_path("http://jellyfin.local:8096/Audio/123/stream?static=true"),
        None,
        None,
    );

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(id3, 3);
    assert_eq!(queue.len(), 3);

    // Select first track
    let cur = queue.current_or_first().unwrap();
    assert_eq!(cur.id, id1);
    assert_eq!(cur.position, 0);

    // Peek next without advancing
    let peek = queue.peek_next(false, false).unwrap();
    assert_eq!(peek.id, id2);
    assert_eq!(queue.current().unwrap().id, id1);

    // Advance next
    let next = queue.advance_next(false, false, false).unwrap();
    assert_eq!(next.id, id2);

    // Advance to last
    let next2 = queue.advance_next(false, false, false).unwrap();
    assert_eq!(next2.id, id3);

    // End of queue without repeat returns None
    assert!(queue.advance_next(false, false, false).is_none());

    // With repeat=true wraps around to first track
    let wrapped = queue.advance_next(true, false, false).unwrap();
    assert_eq!(wrapped.id, id1);

    // Step previous with repeat wraps to last track
    let prev = queue.step_previous(true).unwrap();
    assert_eq!(prev.id, id3);
}

#[test]
fn test_queue_consume_and_remove() {
    let mut queue = Queue::new();
    let id1 = queue.enqueue(AudioSource::from_uri_or_path("/music/1.wv"), None, None);
    let id2 = queue.enqueue(AudioSource::from_uri_or_path("/music/2.wv"), None, None);
    let id3 = queue.enqueue(AudioSource::from_uri_or_path("/music/3.wv"), None, None);

    queue.select_id(id1);
    // Advance with consume=true removes id1 and selects id2
    let next = queue.advance_next(false, false, true).unwrap();
    assert_eq!(next.id, id2);
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.items()[0].id, id2);
    assert_eq!(queue.items()[0].position, 0);

    // Remove by id
    let removed = queue.remove_id(id3).unwrap();
    assert_eq!(removed.id, id3);
    assert_eq!(queue.len(), 1);
}

#[test]
fn test_dsd_rate_calculations() {
    assert_eq!(DsdRate::Dsd64.hz(), 2_822_400);
    assert_eq!(DsdRate::Dsd64.dop_pcm_rate_hz(), 176_400);
    assert_eq!(DsdRate::Dsd64.u32_frame_rate_hz(), 88_200);

    assert_eq!(DsdRate::Dsd128.hz(), 5_644_800);
    assert_eq!(DsdRate::Dsd128.dop_pcm_rate_hz(), 352_800);
    assert_eq!(DsdRate::Dsd128.u32_frame_rate_hz(), 176_400);

    assert_eq!(DsdRate::Dsd256.dop_pcm_rate_hz(), 705_600);
    assert_eq!(DsdRate::Dsd512.dop_pcm_rate_hz(), 1_411_200);
}

#[test]
fn test_uri_auth_stripping() {
    assert_eq!(
        strip_uri_auth("http://user:secret@jellyfin.local:8096/Audio/1/stream"),
        "http://jellyfin.local:8096/Audio/1/stream"
    );
    assert_eq!(
        strip_uri_auth("https://example.com/live.flac"),
        "https://example.com/live.flac"
    );
}

#[test]
fn test_security_path_policy() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let policy = PathPolicy::new(vec![manifest_dir.clone()]);

    // Valid file inside allowed_roots
    let cargo_toml = manifest_dir.join("Cargo.toml");
    let validated = policy.validate_local_path(&cargo_toml);
    assert!(validated.is_ok());

    // Traversal attack rejected
    let traversal = manifest_dir.join("../Cargo.toml");
    assert!(policy.validate_local_path(&traversal).is_err());

    // System file outside allowed_roots rejected
    assert!(
        policy
            .validate_local_path(std::path::Path::new("/etc/passwd"))
            .is_err()
    );

    // Directory instead of regular file rejected
    assert!(policy.validate_local_path(&manifest_dir).is_err());
}

#[test]
fn test_config_parsing() {
    let toml_str = r#"
[daemon]
name = "kiiaud-test"

[output]
backend = "alsa"
device = "hw:CARD=Audio,DEV=0"
bit_perfect = true
dsd_mode = "auto"

[security]
allowed_roots = ["/music", "/mnt/audio"]

[adapter.mpd]
enabled = true
listen = "0.0.0.0:6600"
"#;

    let cfg = KiiaudConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.name, "kiiaud-test");
    assert_eq!(cfg.output.device, "hw:CARD=Audio,DEV=0");
    assert!(cfg.output.bit_perfect);
    assert_eq!(cfg.security.allowed_roots.len(), 2);
}

#[test]
fn test_default_config_template_is_valid() {
    let cfg = KiiaudConfig::from_toml_str(kiiaud_core::config::DEFAULT_CONFIG_TEMPLATE)
        .expect("DEFAULT_CONFIG_TEMPLATE must be valid TOML");
    assert_eq!(cfg.daemon.name, "kiiaud-endpoint");
    assert_eq!(cfg.output.backend, "alsa");
    assert!(cfg.output.bit_perfect);
    assert!(cfg.adapter.web.enabled);
    assert!(cfg.adapter.mpd.enabled);
    assert!(!cfg.adapter.jellyfin.enabled);
}

#[test]
fn test_config_merge_overrides_properly() {
    let sys_toml = r#"
[daemon]
name = "system-kiiaud"
log_level = "warn"

[output]
backend = "alsa"
device = "hw:0,0"
bit_perfect = true

[adapter.mpd]
enabled = true
listen = "0.0.0.0:6600"
"#;

    let user_toml = r#"
[daemon]
name = "user-kiiaud"

[output]
device = "hw:CARD=USB,DEV=0"

[adapter.jellyfin]
enabled = true
server_url = "https://my-jellyfin.net"
"#;

    let mut sys_val: toml::Value = toml::from_str(sys_toml).unwrap();
    let user_val: toml::Value = toml::from_str(user_toml).unwrap();

    KiiaudConfig::merge_toml(&mut sys_val, user_val);

    let merged: KiiaudConfig = sys_val.try_into().unwrap();

    // Overridden by user
    assert_eq!(merged.daemon.name, "user-kiiaud");
    assert_eq!(merged.output.device, "hw:CARD=USB,DEV=0");
    assert!(merged.adapter.jellyfin.enabled);
    assert_eq!(merged.adapter.jellyfin.server_url, "https://my-jellyfin.net");

    // Preserved from system
    assert_eq!(merged.daemon.log_level, "warn");
    assert_eq!(merged.output.backend, "alsa");
    assert!(merged.output.bit_perfect);
    assert!(merged.adapter.mpd.enabled);
    assert_eq!(merged.adapter.mpd.listen, "0.0.0.0:6600");
}

#[test]
fn test_queue_move_swap_shuffle_batch() {
    let mut queue = Queue::new();
    let batch = vec![
        (AudioSource::from_uri_or_path("/music/1.flac"), Some("Song 1".into()), None),
        (AudioSource::from_uri_or_path("/music/2.flac"), Some("Song 2".into()), None),
        (AudioSource::from_uri_or_path("/music/3.flac"), Some("Song 3".into()), None),
        (AudioSource::from_uri_or_path("/music/4.flac"), Some("Song 4".into()), None),
    ];
    let ids = queue.enqueue_batch(batch);
    assert_eq!(ids.len(), 4);
    assert_eq!(queue.len(), 4);

    // Current is song 2 (id = 2, position = 1)
    queue.select_pos(1);
    assert_eq!(queue.current().unwrap().id, ids[1]);

    // Move pos 1 (Song 2) to pos 3
    assert!(queue.move_pos(1, 3));
    // Order should now be: Song 1, Song 3, Song 4, Song 2
    assert_eq!(queue.items()[0].id, ids[0]);
    assert_eq!(queue.items()[1].id, ids[2]);
    assert_eq!(queue.items()[2].id, ids[3]);
    assert_eq!(queue.items()[3].id, ids[1]);
    // Current track pointer should follow Song 2 to pos 3
    assert_eq!(queue.current().unwrap().id, ids[1]);
    assert_eq!(queue.current_index(), Some(3));

    // Swap pos 0 and pos 2 -> Song 4 becomes pos 0, Song 1 becomes pos 2
    assert!(queue.swap_pos(0, 2));
    assert_eq!(queue.items()[0].id, ids[3]);
    assert_eq!(queue.items()[2].id, ids[0]);

    // Swap by ID: swap Song 2 and Song 4
    assert!(queue.swap_id(ids[1], ids[3]));
    // Now pos 0 is Song 2, pos 3 is Song 4
    assert_eq!(queue.items()[0].id, ids[1]);
    assert_eq!(queue.items()[3].id, ids[3]);
    assert_eq!(queue.current_index(), Some(0));

    // Shuffle
    queue.shuffle();
    assert_eq!(queue.len(), 4);
    // Song 2 is still current, wherever it was shuffled
    assert_eq!(queue.current().unwrap().id, ids[1]);
}
