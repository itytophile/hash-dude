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
use futures::{sink::SinkExt, stream::StreamExt};
use msg::{ConversionError, Message};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast::{self, error::SendError, Sender};
use tracing::{debug, info, warn, Level};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
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

enum ExecuteMsgError {
    Broadcast(SendError<Message>),
    Conversion(ConversionError),
}

fn execute_dude_msg(msg: String, broadcast_tx: &Sender<Message>) -> Result<usize, ExecuteMsgError> {
    let message: Message = Message::try_from(msg.as_str()).map_err(ExecuteMsgError::Conversion)?;
    info!("Dude wants to {:?}", message);
    broadcast_tx
        .send(message)
        .map_err(ExecuteMsgError::Broadcast)
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

            let mut broadcast_rx = state.broadcast_tx.subscribe();

            let state_clone = Arc::clone(&state);

            let mut transfer_orders_task = tokio::spawn(async move {
                while let Ok(msg) = broadcast_rx.recv().await {
                    let text = match msg {
                        Message::Search(hash, range) => {
                            let (begin, end) = (range.start, range.end);
                            let slaves_id_order = state_clone.slaves_id_order.lock().unwrap();
                            let slaves_count = slaves_id_order.len();
                            let slave_pos = slaves_id_order
                                .iter()
                                .position(|&id| id == slave_id)
                                .unwrap();
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
                            let slave_end =
                                slave_begin + gap + if slave_pos < remainder { 1 } else { 0 };
                            format!(
                                "search {} {} {}",
                                hash,
                                get_word_from_number(slave_begin),
                                get_word_from_number(slave_end)
                            )
                        }
                        Message::Stop => "stop".to_owned(),
                        Message::Exit => "exit".to_owned(),
                    };
                    // arrêt de la boucle à la moindre erreur
                    if tx.send(ws::Message::Text(text)).await.is_err() {
                        break;
                    }
                }
            });

            let broadcast_tx = state.broadcast_tx.clone();

            let mut slave_listening_task = tokio::spawn(async move {
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
                                    if let Err(err) = broadcast_tx.send(Message::Stop) {
                                        warn!("Can't broadcast: {}", err)
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
            });

            // Si l'une des deux tâches s'arrêtent, plus la peine de continuer.
            tokio::select! {
                _ = (&mut transfer_orders_task) => slave_listening_task.abort(),
                _ = (&mut slave_listening_task) => transfer_orders_task.abort(),
            };

            state
                .slaves_id_order
                .lock()
                .unwrap()
                .retain(|&id| id != slave_id);

            info!("Slave {} disconnected", slave_id)
        } else {
            info!("Dude connected");
            if let Err(err) = execute_dude_msg(msg, &state.broadcast_tx) {
                match err {
                    ExecuteMsgError::Broadcast(err) => {
                        warn!("Can't broadcast: {}", err)
                    }
                    ExecuteMsgError::Conversion(err) => {
                        warn!("{:?}", err)
                    }
                }
            }

            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    // On est poli, on pong quand on a un ping
                    ws::Message::Ping(payload) => {
                        debug!("Ping received from dude");
                        if tx.send(ws::Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    ws::Message::Text(msg) => {
                        if let Err(err) = execute_dude_msg(msg, &state.broadcast_tx) {
                            match err {
                                ExecuteMsgError::Broadcast(err) => {
                                    warn!("Can't broadcast: {}", err)
                                }
                                ExecuteMsgError::Conversion(err) => {
                                    warn!("{:?}", err)
                                }
                            }
                        }
                    }
                    _ => warn!("Non textual message from dude: {:?}", msg),
                }
            }

            info!("Dude disconnected")
        }
    } else {
        warn!("Error with unknown client");
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("index.html"))
}
