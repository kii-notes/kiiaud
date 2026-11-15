// SPDX-License-Identifier: MIT OR Apache-2.0

//! Android JNI entrypoints for `kiiaud` (`libkiiaud_android.so`).
//!
//! Hosts the full `kiiaud` playback daemon inside the Android Foreground Service
//! (`org.kiiaud.KiiaudService`) and exposes USB DAC file-descriptor hand-off.

#[cfg(target_os = "android")]
mod android_jni {
    use jni::EnvUnowned;
    use jni::errors::ThrowRuntimeExAndDefault;
    use jni::objects::{JClass, JString};
    use jni::sys::{jboolean, jint};
    use kiiaud_adapter_cli::run_ipc_server;
    use kiiaud_adapter_jellyfin::run_jellyfin_adapter;
    use kiiaud_adapter_lms::{run_lag_adapter, run_lms_adapter};
    use kiiaud_adapter_mpd::run_mpd_server_with_dir;
    use kiiaud_adapter_web::run_web_server;
    use kiiaud_core::KiiaudConfig;
    use kiiaud_playback::PlaybackEngine;
    use parking_lot::Mutex;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use tokio::sync::oneshot;
    use tracing_subscriber::EnvFilter;

    struct DaemonRuntime {
        shutdown_tx: Option<oneshot::Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    static DAEMON: Mutex<Option<DaemonRuntime>> = Mutex::new(None);
    static DAEMON_RUNNING: AtomicBool = AtomicBool::new(false);
    static LOGGING_INIT: OnceLock<()> = OnceLock::new();

    fn init_logging(level: &str) {
        LOGGING_INIT.get_or_init(|| {
            let filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .try_init();
        });
    }

    fn start_daemon_impl(config_path: String, backend_override: String) -> i32 {
        let mut guard = DAEMON.lock();
        if DAEMON_RUNNING.load(Ordering::SeqCst) {
            return 0;
        }

        let cfg_path = PathBuf::from(&config_path);
        let mut config = if cfg_path.is_file() {
            match KiiaudConfig::from_file(&cfg_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(path = %cfg_path.display(), "Failed to load config: {e}");
                    KiiaudConfig::default()
                }
            }
        } else {
            let mut c = KiiaudConfig::default();
            c.output.backend = if backend_override.is_empty() || backend_override == "auto" {
                "android".to_owned()
            } else {
                backend_override.clone()
            };
            let _ = c.save_to_file(&cfg_path);
            c
        };

        if !backend_override.is_empty() && backend_override != "auto" {
            config.output.backend = backend_override;
        } else if config.output.backend == "auto" {
            config.output.backend = "android".to_owned();
        }

        init_logging(&config.daemon.log_level);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        DAEMON_RUNNING.store(true, Ordering::SeqCst);

        let handle = std::thread::Builder::new()
            .name("kiiaud-android-rt".to_owned())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("Failed to create Tokio runtime: {e}");
                        DAEMON_RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                rt.block_on(async move {
                    tracing::info!(
                        name = %config.daemon.name,
                        backend = %config.output.backend,
                        config = %cfg_path.display(),
                        "starting kiiaud Android engine"
                    );

                    let engine = PlaybackEngine::new(&config);
                    let handle = engine.handle();

                    tokio::spawn(async move {
                        engine.run().await;
                    });

                    {
                        let sock_path = config.daemon.socket_path.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_ipc_server(sock_path, h).await {
                                tracing::warn!("CLI IPC server error: {e}");
                            }
                        });
                    }

                    if config.adapter.mpd.enabled {
                        let listen = config.adapter.mpd.listen.clone();
                        let music_dir = config.adapter.mpd.music_directory.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_mpd_server_with_dir(listen, music_dir, h).await {
                                tracing::error!("MPD adapter error: {e}");
                            }
                        });
                    }

                    if config.adapter.jellyfin.enabled {
                        let jf_cfg = config.adapter.jellyfin.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_jellyfin_adapter(jf_cfg, h).await {
                                tracing::error!("Jellyfin adapter error: {e}");
                            }
                        });
                    }

                    {
                        let lms_cfg = config.adapter.lms.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_lms_adapter(lms_cfg, h).await {
                                tracing::error!("LMS LAN adapter error: {e}");
                            }
                        });
                    }

                    {
                        let lag_cfg = config.adapter.lag.clone();
                        let h = handle.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_lag_adapter(lag_cfg, h).await {
                                tracing::error!("Lyrion Anywhere adapter error: {e}");
                            }
                        });
                    }

                    if config.adapter.web.enabled {
                        let listen = config.adapter.web.listen.clone();
                        let h = handle.clone();
                        let cfg_clone = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_web_server(listen, h, cfg_clone, Some(cfg_path)).await
                            {
                                tracing::error!("WebUI adapter error: {e}");
                            }
                        });
                    }

                    let _ = shutdown_rx.await;
                    tracing::info!("kiiaud Android engine shutting down");
                });

                DAEMON_RUNNING.store(false, Ordering::SeqCst);
            });

        match handle {
            Ok(thread) => {
                *guard = Some(DaemonRuntime {
                    shutdown_tx: Some(shutdown_tx),
                    thread: Some(thread),
                });
                0
            }
            Err(e) => {
                DAEMON_RUNNING.store(false, Ordering::SeqCst);
                tracing::error!("Failed to spawn kiiaud daemon thread: {e}");
                -1
            }
        }
    }

    fn stop_daemon_impl() {
        let mut guard = DAEMON.lock();
        if let Some(mut rt) = guard.take() {
            if let Some(tx) = rt.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(thread) = rt.thread.take() {
                drop(guard);
                let _ = thread.join();
            }
        }
        DAEMON_RUNNING.store(false, Ordering::SeqCst);
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_startDaemon(
        mut env: EnvUnowned<'_>,
        _class: JClass<'_>,
        config_path: JString<'_>,
        backend_override: JString<'_>,
    ) -> jint {
        let (cfg, backend) = env
            .with_env(|env| -> jni::errors::Result<(String, String)> {
                let c = config_path.try_to_string(env)?;
                let b = backend_override.try_to_string(env)?;
                Ok((c, b))
            })
            .resolve::<ThrowRuntimeExAndDefault>();
        start_daemon_impl(cfg, backend)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_stopDaemon(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
    ) {
        stop_daemon_impl();
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_isDaemonRunning(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
    ) -> jboolean {
        u8::from(DAEMON_RUNNING.load(Ordering::SeqCst))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_setUsbDeviceFd(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
        fd: jint,
    ) {
        kiiaud_output_android::set_active_usb_fd(fd);
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_clearUsbDevice(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
    ) {
        kiiaud_output_android::clear_active_usb_fd();
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_getActiveUsbFd(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
    ) -> jint {
        kiiaud_output_android::get_active_usb_fd().unwrap_or(-1)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_isUsbDacActive(
        _env: EnvUnowned<'_>,
        _class: JClass<'_>,
    ) -> jboolean {
        u8::from(kiiaud_output_android::get_active_usb_fd().is_some())
    }
}
