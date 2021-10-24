use alphabet::{get_number_from_word, get_word_from_number};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, TypedHeader,
    },
    handler::get,
    response::{Html, IntoResponse},
    AddExtensionLayer, Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use std::{
    net::SocketAddr,
    ops::Range,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast::{self, error::SendError, Sender};
use tracing::{debug, info, warn};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
    broadcast_tx: broadcast::Sender<CustomMessage>,
}

#[derive(Debug, Clone)]
enum CustomMessage {
    Search(String, Range<usize>),
    Stop,
    Exit,
}

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "server=debug")
    }

    tracing_subscriber::fmt::init();

    let clients_id_order = Mutex::new(Vec::new());
    let (broadcast_tx, _) = broadcast::channel(100);

    let app_state = Arc::new(AppState {
        slaves_id_order: clients_id_order,
        broadcast_tx,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(websocket_handler))
        .layer(AddExtensionLayer::new(app_state));

    let addr = SocketAddr::from(([172, 17, 0, 1], 3000));
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
    BroadcastError(SendError<CustomMessage>),
    UnknownRequest(String),
    WordProblem(String),
}

impl From<SendError<CustomMessage>> for ExecuteMsgError {
    fn from(err: SendError<CustomMessage>) -> Self {
        ExecuteMsgError::BroadcastError(err)
    }
}

fn execute_dude_msg(
    msg: String,
    broadcast_tx: &Sender<CustomMessage>,
) -> Result<(), ExecuteMsgError> {
    let splitted: Vec<&str> = msg.split(' ').collect();
    match splitted.as_slice() {
        &["search", hash, begin, end] => {
            info!(
                "Dude wants to crack {} in range [{}; {}), broadcasting...",
                hash, begin, end
            );
            let (begin_num, end_num) =
                match (get_number_from_word(begin), get_number_from_word(end)) {
                    (Ok(begin_num), Ok(end_num)) => (begin_num, end_num),
                    (Ok(_), Err(err)) => {
                        return Err(ExecuteMsgError::WordProblem(format!(
                            "Problem with end word: {}",
                            err
                        )));
                    }
                    (Err(err), Ok(_)) => {
                        return Err(ExecuteMsgError::WordProblem(format!(
                            "Problem with begin word: {}",
                            err
                        )));
                    }
                    (Err(err0), Err(err1)) => {
                        return Err(ExecuteMsgError::WordProblem(format!(
                            "Problem with both words: {};{}",
                            err0, err1
                        )));
                    }
                };
            broadcast_tx.send(CustomMessage::Search(hash.to_owned(), begin_num..end_num))?;
        }
        ["stop"] => {
            info!("Dude wants to stop, broadcasting...");
            broadcast_tx.send(CustomMessage::Stop)?;
        }
        ["exit"] => {
            info!("Dude wants to exit, broadcasting...");
            broadcast_tx.send(CustomMessage::Exit)?;
        }
        _ => {
            return Err(ExecuteMsgError::UnknownRequest(format!(
                "Unknown request from dude: {}",
                msg
            )))
        }
    };

    Ok(())
}

async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut tx, mut rx) = stream.split();

    if let Some(Ok(Message::Text(msg))) = rx.next().await {
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
                        CustomMessage::Search(hash, range) => {
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
                            let slave_begin = gap * slave_pos
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
                        CustomMessage::Stop => "stop".to_owned(),
                        CustomMessage::Exit => "exit".to_owned(),
                    };
                    // arrêt de la boucle à la moindre erreur
                    if tx.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
            });

            let broadcast_tx = state.broadcast_tx.clone();

            let mut slave_listening_task = tokio::spawn(async move {
                while let Some(Ok(msg)) = rx.next().await {
                    match msg {
                        Message::Text(msg) => {
                            let splitted: Vec<&str> = msg.split(' ').collect();
                            match splitted.as_slice() {
                                ["found", hash, word] => {
                                    info!(
                                        "Slave {} found the word {} behind the hash {}. Now stopping all slaves...",
                                        slave_id, word, hash
                                    );
                                    if let Err(err) = broadcast_tx.send(CustomMessage::Stop) {
                                        warn!("Can't broadcast: {}", err)
                                    }
                                }
                                _ => warn!("Unknown request from slave {}: {}", slave_id, msg),
                            }
                        }
                        Message::Ping(_) => {
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
                    ExecuteMsgError::BroadcastError(err) => {
                        warn!("Can't broadcast: {}", err)
                    }
                    ExecuteMsgError::UnknownRequest(err) => warn!("{}", err),
                    ExecuteMsgError::WordProblem(err) => warn!("{}", err),
                }
            }

            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    // On est poli, on pong quand on a un ping
                    Message::Ping(payload) => {
                        debug!("Ping received from dude");
                        if tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Text(msg) => {
                        if let Err(err) = execute_dude_msg(msg, &state.broadcast_tx) {
                            match err {
                                ExecuteMsgError::BroadcastError(err) => {
                                    warn!("Can't broadcast: {}", err)
                                }
                                ExecuteMsgError::UnknownRequest(err) => warn!("{}", err),
                                ExecuteMsgError::WordProblem(err) => warn!("{}", err),
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
