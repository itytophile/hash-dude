use std::env;

use futures::SinkExt;
use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "slave=debug")
    }

    tracing_subscriber::fmt::init();
    let connect_addr = env::args()
        .nth(1)
        .expect("This program needs a WebSocket URL as first argument!");

    let url = url::Url::parse(&connect_addr).unwrap();

    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");

    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text("slave".to_owned()))
        .await
        .expect("issou");

    info!("Successfully connected to master");

    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            Message::Text(msg) => {
                let splitted: Vec<&str> = msg.split(' ').collect();
                match splitted.as_slice() {
                    ["search", hash, begin, end] => {
                        info!(
                            "Search request from master, cracking {} in range [{}; {})...",
                            hash, begin, end
                        );
                    }
                    ["stop"] => {
                        info!("Stop request from master, stopping...");
                    }
                    ["exit"] => {
                        info!("Exit request from master, exitting...");
                        break;
                    }
                    _ => warn!("Unknown request from master: {}", msg),
                }
            }
            _ => warn!("Non textual message from master: {:?}", msg),
        }
    }
}
