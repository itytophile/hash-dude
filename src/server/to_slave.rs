use crate::{broadcast_message, msg::ToSlaveMessage, AppState};
use alphabet::get_word_from_number;
use axum::extract::ws::{self, WebSocket};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

pub(crate) async fn master_to_slave_relay_task(
    mut rx_from_master: broadcast::Receiver<ToSlaveMessage>,
    state_clone: Arc<AppState>,
    slave_id: u32,
    mut tx_to_slave: SplitSink<WebSocket, ws::Message>,
) {
    while let Ok(msg) = rx_from_master.recv().await {
        let text = match msg {
            ToSlaveMessage::Search(hash, range) => {
                let (begin, end) = (range.start, range.end);
                let slaves_id_order = state_clone.slaves_id_order.lock().unwrap();
                let slaves_count = slaves_id_order.len();
                // Peut échouer si l'esclave s'est déconnecté
                let slave_pos =
                    if let Some(pos) = slaves_id_order.iter().position(|&id| id == slave_id) {
                        pos
                    } else {
                        // On peut casser la boucle si l'esclave n'est plus là
                        break;
                    };
                let gap = (end - begin) / slaves_count;
                let remainder = (end - begin) % slaves_count;
                // On va tout décaler pour distribuer équitablement le reste
                // donc 1 chacun pour les concernés (slave_pos < remainder)
                let slave_begin = begin
                    + gap * slave_pos
                    + if slave_pos < remainder {
                        slave_pos
                    } else {
                        remainder
                    };
                let slave_end = slave_begin + gap + if slave_pos < remainder { 1 } else { 0 };
                format!(
                    "search {} {} {}",
                    hash,
                    get_word_from_number(slave_begin),
                    get_word_from_number(slave_end)
                )
            }
            ToSlaveMessage::Stop => "stop".to_owned(),
            ToSlaveMessage::Exit => "exit".to_owned(),
        };
        // arrêt de la boucle à la moindre erreur
        if let Err(err) = tx_to_slave.send(ws::Message::Text(text)).await {
            warn!("Can't send to slave: {err}");
            break;
        }
    }
    debug!("Slave {slave_id} no longer exists, stopped listening to master",)
}

pub(crate) async fn slave_listening_task(
    mut rx: SplitStream<WebSocket>,
    slave_id: u32,
    state: &Arc<AppState>,
) {
    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            ws::Message::Text(msg) => {
                let split: Vec<&str> = msg.split(' ').collect();
                match split.as_slice() {
                    ["found", hash, word] => {
                        info!(
                            "Slave {slave_id} found the word {word} behind the hash {hash}. Now stopping all slaves..."
                        );

                        // on send le message tel quel. Le dude veut aussi un found <hash> <word>
                        state.send_to_dudes(msg, "Can't tell dudes that word was found");

                        broadcast_message(&state.tx_to_slaves, ToSlaveMessage::Stop);

                        let mut request_queue = state.request_queue.lock().unwrap();

                        request_queue.remove(0);

                        broadcast_message(&state.tx_to_listeners, request_queue.len());

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
                            )
                        }
                    }
                    _ => warn!("Unknown request from slave {slave_id}: {msg}"),
                }
            }
            ws::Message::Ping(_) => {
                warn!("Slave {slave_id} ping'd but pong not implemented")
            }
            _ => warn!("Non textual message from slave {slave_id}: {msg:?}"),
        }
    }
}
