//! Probe a custom HTTPS relay using OS CA trust (DotTolerant system verifier).
//!
//! Usage:
//!   cargo run --example probe_custom_relay -- \
//!     https://relay.example.com

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://relay.example.com".to_string());

    println!("probing custom relay: {url}");

    let arg = tunnelmanager_slint::protocol::RelayConfigArg {
        mode: "custom".to_string(),
        urls: vec![url],
        auth_token: None,
        fallback: Some("strict".to_string()),
    };

    match tunnelmanager_slint::protocol::verify_relays(arg).await {
        Ok(res) => {
            println!(
                "OK connected via {:?} latency_ms={}",
                res.url, res.latency_ms
            );
        }
        Err(err) => {
            eprintln!("FAIL: {err}");
            std::process::exit(1);
        }
    }
}
