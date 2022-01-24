use axum::extract::ws::{self, WebSocket};
use futures::{stream::SplitStream, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{broadcast_message, msg::ToSlaveMessage, AppState};

pub(crate) async fn dude_listening_task(
    mut rx_from_client: SplitStream<WebSocket>,
    tx_to_dude: mpsc::Sender<ws::Message>,
    state: Arc<AppState>,
) {
    while let Some(Ok(msg)) = rx_from_client.next().await {
        match msg {
            // On est poli, on pong quand on a un ping
            ws::Message::Ping(payload) => {
                debug!("Ping received from dude");
                // on utilise le tx_proxy originel non cloné
                if tx_to_dude.send(ws::Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            ws::Message::Text(msg) => match ToSlaveMessage::try_from(msg.as_str()) {
                Ok(message) => {
                    info!("Dude wants to {message:?}");

                    match message {
                        ToSlaveMessage::Search(hash, range) => {
                            let mut request_queue = state.request_queue.lock().unwrap();

                            if request_queue.is_empty() {
                                broadcast_message(
                                    &state.tx_to_slaves,
                                    ToSlaveMessage::Search(hash.clone(), range.clone()),
                                );
                                state.send_to_dudes(
                                    format!("info Cracking {hash}..."),
                                    "Can't tell dudes that hash is cracking",
                                );
                            } else {
                                debug!("Search request pushed to queue");
                                state.send_to_dudes(
                                    "info Request pushed to queue".to_owned(),
                                    "Can't tell dudes that request pushed to queue",
                                );
                            }

                            request_queue.push((hash, range));

                            broadcast_message(&state.tx_to_listeners, request_queue.len())
                        }
                        ToSlaveMessage::Stop => {
                            let mut request_queue = state.request_queue.lock().unwrap();
                            if !request_queue.is_empty() {
                                request_queue.remove(0);

                                state.send_to_dudes(
                                    "info Task stopped succesfully".to_owned(),
                                    "Can't tell dudes that the task stopped",
                                );

                                broadcast_message(&state.tx_to_listeners, request_queue.len());

                                broadcast_message(&state.tx_to_slaves, message);

                                if !request_queue.is_empty() {
                                    info!("Sending queued request: {:?}", request_queue[0]);

                                    let (hash, range) = &request_queue[0];
                                    broadcast_message(
                                        &state.tx_to_slaves,
                                        ToSlaveMessage::Search(hash.clone(), range.clone()),
                                    );

                                    state.send_to_dudes(
                                        format!("info Cracking {hash}..."),
                                        "Can't tell dudes that hash is cracking",
                                    );
                                }
                            } else {
                                state.send_to_dudes(
                                    "info Nothing to stop".to_owned(),
                                    "Can't tell dudes that there is nothing to stop",
                                );
                                warn!("Nothing to stop")
                            }
                        }
                        ToSlaveMessage::Exit => {
                            let mut request_queue = state.request_queue.lock().unwrap();
                            if !request_queue.is_empty() {
                                request_queue.clear();

                                state.send_to_dudes(
                                    "info Tasks stopped succesfully".to_owned(),
                                    "Can't tell dudes that the tasks stopped",
                                );

                                broadcast_message(&state.tx_to_listeners, request_queue.len());
                            }

                            broadcast_message(&state.tx_to_slaves, message);

                            state.send_to_dudes(
                                "info Exit request to slaves".to_owned(),
                                "Can't tell dudes that exit requests",
                            );
                        }
                    }
                }
                Err(err) => warn!("{err:?}"),
            },
            _ => warn!("Non textual message from dude: {msg:?}"),
        }
    }
}
