// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared TLS configuration using bundled Mozilla root certificates (`webpki-roots`).
//!
//! On Android (`target_os = "android"`), `reqwest` defaults to `rustls-platform-verifier`,
//! which panics unless initialized with a live JVM `JNIEnv` and Android `Context`.
//! Preconfiguring `rustls::ClientConfig` with `webpki-roots` avoids that panic in both
//! standalone CLI mode and Android APK/JNI mode.

use std::sync::Arc;

/// Build a `rustls::ClientConfig` backed by `webpki_roots::TLS_SERVER_ROOTS`.
pub fn webpki_rustls_config() -> rustls::ClientConfig {
    let root_store = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("valid default TLS protocol versions")
        .with_root_certificates(root_store)
        .with_no_client_auth()
}
