mod msg;

use alphabet::get_word_from_number;
use axum::{
    extract::{
        ws::{self, WebSocket, WebSocketUpgrade},
        Extension, TypedHeader,
    },
    response::{Html, IntoResponse},
    routing::get,
    AddExtensionLayer, Router,
};
use futures::{
    sink::SinkExt,
    stream::{SplitSink, SplitStream, StreamExt},
};
use msg::Message;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn, Level};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
    request_queue: Mutex<Vec<(String, std::ops::Range<usize>)>>,
    broadcast_tx: broadcast::Sender<Message>,
}

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let addr: SocketAddr = std::env::args()
        .nth(1)
        .expect("This program needs an address (like 127.0.0.1:3000) as first argument!")
        .parse()
        .expect("Can't parse provided address");

    let (broadcast_tx, _) = broadcast::channel(100);

    let app_state = Arc::new(AppState {
        slaves_id_order: Mutex::new(Vec::new()),
        request_queue: Mutex::new(Vec::new()),
        broadcast_tx,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(websocket_handler))
        .layer(AddExtensionLayer::new(app_state));

    tracing::info!("Listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<AppState>>,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
) -> impl IntoResponse {
    if let Some(TypedHeader(user_agent)) = user_agent {
        debug!("`{}` connected", user_agent.as_str());
    }
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut tx, mut rx) = stream.split();

    if let Some(Ok(ws::Message::Text(msg))) = rx.next().await {
        if msg == "slave" {
            let slave_id = {
                // On utilise ce bloc pour drop le mutex le plus tôt possible
                let mut slaves_id_order = state.slaves_id_order.lock().unwrap();
                let id = slaves_id_order.iter().max().copied().unwrap_or(0) + 1;
                slaves_id_order.push(id);
                id // En Rust, faire cela permet de faire "retourner" une valeur à partir du bloc
            };

            info!("Slave connected with id = {}", slave_id);

            tokio::spawn(master_to_slave_relay_task(
                state.broadcast_tx.subscribe(),
                Arc::clone(&state),
                slave_id,
                tx,
            ));

            slave_listening_task(rx, slave_id, &state).await;

            state
                .slaves_id_order
                .lock()
                .unwrap()
                .retain(|&id| id != slave_id);

            info!("Slave {} disconnected", slave_id);

            // Arrêt de la tâche d'écoute de l'esclave associé
            if let Err(err) = state
                .broadcast_tx
                .send(Message::SlaveDisconnected(slave_id))
            {
                warn!("Can't broadcast: {}", err)
            };
        } else {
            info!("Dude connected");

            execute_msg(msg, &state);

            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    // On est poli, on pong quand on a un ping
                    ws::Message::Ping(payload) => {
                        debug!("Ping received from dude");
                        if tx.send(ws::Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    ws::Message::Text(msg) => execute_msg(msg, &state),
                    _ => warn!("Non textual message from dude: {:?}", msg),
                }
            }

            info!("Dude disconnected")
        }
    } else {
        warn!("Error with unknown client");
    }
}

async fn master_to_slave_relay_task(
    mut broadcast_rx: broadcast::Receiver<Message>,
    state_clone: Arc<AppState>,
    slave_id: u32,
    mut tx: SplitSink<WebSocket, ws::Message>,
) {
    while let Ok(msg) = broadcast_rx.recv().await {
        let text = match msg {
            Message::Search(hash, range) => {
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
            Message::Stop => "stop".to_owned(),
            Message::Exit => "exit".to_owned(),
            Message::SlaveDisconnected(id) => {
                if id == slave_id {
                    break;
                } else {
                    continue;
                }
            }
        };
        // arrêt de la boucle à la moindre erreur
        if tx.send(ws::Message::Text(text)).await.is_err() {
            break;
        }
    }
    debug!(
        "Slave {} no longer exists, stopped listening to master",
        slave_id
    )
}

async fn slave_listening_task(
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
                            "Slave {} found the word {} behind the hash {}. Now stopping all slaves...",
                            slave_id, word, hash
                        );

                        if let Err(err) = state.broadcast_tx.send(Message::Stop) {
                            warn!("Can't broadcast: {}", err)
                        }

                        let mut request_queue = state.request_queue.lock().unwrap();
                        request_queue.remove(0);
                        if !request_queue.is_empty() {
                            info!("Sending queued request: {:?}", request_queue[0]);

                            let (hash, range) = &request_queue[0];
                            if let Err(err) = state
                                .broadcast_tx
                                .send(Message::Search(hash.clone(), range.clone()))
                            {
                                warn!("Can't broadcast: {}", err)
                            }
                        }
                    }
                    _ => warn!("Unknown request from slave {}: {}", slave_id, msg),
                }
            }
            ws::Message::Ping(_) => {
                warn!("Slave {} ping'd but pong not implemented", slave_id)
            }
            _ => warn!("Non textual message from slave {}: {:?}", slave_id, msg),
        }
    }
}

fn execute_msg(msg: String, state: &Arc<AppState>) {
    match Message::try_from(msg.as_str()) {
        Ok(message) => {
            info!("Dude wants to {:?}", message);

            if let Message::Search(hash, range) = message {
                let mut request_queue = state.request_queue.lock().unwrap();

                if request_queue.is_empty() {
                    if let Err(err) = state
                        .broadcast_tx
                        .send(Message::Search(hash.clone(), range.clone()))
                    {
                        warn!("Can't broadcast: {}", err)
                    }
                } else {
                    debug!("Search request pushed to queue");
                }

                request_queue.push((hash, range))
            } else if let Err(err) = state.broadcast_tx.send(message) {
                warn!("Can't broadcast: {}", err)
            }
        }
        Err(err) => warn!("{:?}", err),
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("index.html"))
}
