use std::env;

use futures::SinkExt;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use tokio::task::JoinHandle;
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

    info!("Successfully connected to master at {}", &connect_addr);

    let mut cracking_task: Option<JoinHandle<()>> = None;
    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            Message::Text(msg) => {
                let splitted: Vec<&str> = msg.split(' ').collect();
                match splitted.as_slice() {
                    ["search", hash, begin, end] => {
                        info!("Search request from master");
                        if let Some(task) = cracking_task.as_ref() {
                            info!("Stopping previous search...");
                            task.abort();
                        }
                        info!("Now cracking {} in range [{}; {})...", hash, begin, end);
                        cracking_task = Some(tokio::spawn(async move {
                            loop {
                                println!("prout");
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await
                            }
                        }));
                    }
                    ["stop"] => {
                        if let Some(task) = cracking_task.as_ref() {
                            info!("Stop request from master, aborting...");
                            task.abort();
                            cracking_task = None;
                            info!("Search aborted")
                        } else {
                            info!("No search to stop")
                        }
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
