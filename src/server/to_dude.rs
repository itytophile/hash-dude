use axum::extract::ws::{self, WebSocket};
use futures::{StreamExt, stream::SplitStream};
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
                                if let Err(err) =
                                    state.tx_to_dudes.send(format!("info Cracking {hash}..."))
                                {
                                    warn!("Can't tell dudes that hash is cracking: {err}");
                                }
                            } else {
                                debug!("Search request pushed to queue");
                                if let Err(err) = state
                                    .tx_to_dudes
                                    .send("info Request pushed to queue".to_owned())
                                {
                                    warn!("Can't tell dudes that request pushed to queue: {err}");
                                }
                            }

                            request_queue.push((hash, range));

                            broadcast_message(&state.tx_to_listeners, request_queue.len())
                        }
                        ToSlaveMessage::Stop => {
                            let mut request_queue = state.request_queue.lock().unwrap();
                            if !request_queue.is_empty() {
                                request_queue.remove(0);

                                if let Err(err) = state
                                    .tx_to_dudes
                                    .send("info Task stopped succesfully".to_owned())
                                {
                                    warn!("Can't tell dudes that the task stopped: {err}");
                                }

                                broadcast_message(&state.tx_to_listeners, request_queue.len());

                                broadcast_message(&state.tx_to_slaves, message);

                                if !request_queue.is_empty() {
                                    info!("Sending queued request: {:?}", request_queue[0]);

                                    let (hash, range) = &request_queue[0];
                                    broadcast_message(
                                        &state.tx_to_slaves,
                                        ToSlaveMessage::Search(hash.clone(), range.clone()),
                                    );

                                    if let Err(err) =
                                        state.tx_to_dudes.send(format!("info Cracking {hash}..."))
                                    {
                                        warn!("Can't tell dudes that hash is cracking: {err}");
                                    }
                                }
                            } else {
                                if let Err(err) =
                                    state.tx_to_dudes.send("info Nothing to stop".to_owned())
                                {
                                    warn!("Can't tell dudes that there is nothing to stop: {err}");
                                }
                                warn!("Nothing to stop")
                            }
                        }
                        ToSlaveMessage::Exit => {
                            let mut request_queue = state.request_queue.lock().unwrap();
                            if !request_queue.is_empty() {
                                request_queue.clear();

                                if let Err(err) = state
                                    .tx_to_dudes
                                    .send("info Tasks stopped succesfully".to_owned())
                                {
                                    warn!("Can't tell dudes that the tasks stopped: {err}");
                                }

                                broadcast_message(&state.tx_to_listeners, request_queue.len());
                            }
                            broadcast_message(&state.tx_to_slaves, message);
                            if let Err(err) = state
                                .tx_to_dudes
                                .send("info Exit request to slaves".to_owned())
                            {
                                warn!("Can't tell dudes that exit requests: {err}");
                            }
                        }
                    }
                }
                Err(err) => warn!("{err:?}"),
            },
            _ => warn!("Non textual message from dude: {msg:?}"),
        }
    }
}
