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
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast::{self, Sender};
use tracing::{debug, info, warn};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
    broadcast_tx: broadcast::Sender<String>,
}

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "hash_dude=debug")
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

fn execute_dude_msg(msg: String, broadcast_tx: &Sender<String>) {
    let splitted: Vec<&str> = msg.split(' ').collect();
    match splitted.as_slice() {
        ["search", hash, begin, end] => {
            info!(
                "Dude wants to crack {} in range [{}; {}), broadcasting...",
                hash, begin, end
            );
            broadcast_tx.send(msg).expect("Broadcasting failed");
        }
        ["stop"] => {
            info!("Dude wants to stop, broadcasting...");
            broadcast_tx.send(msg).expect("Broadcasting failed");
        }
        ["exit"] => {
            info!("Dude wants to exit, broadcasting...");
            broadcast_tx.send(msg).expect("Broadcasting failed");
        }
        _ => warn!("Unknown request from dude: {}", msg),
    }
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

            let mut transfer_orders_task = tokio::spawn(async move {
                while let Ok(msg) = broadcast_rx.recv().await {
                    // arrêt de la boucle à la moindre erreur
                    if tx.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
            });

            let mut slave_listening_task = tokio::spawn(async move {
                while let Some(Ok(msg)) = rx.next().await {
                    match msg {
                        Message::Text(msg) => {
                            let splitted: Vec<&str> = msg.split(' ').collect();
                            match splitted.as_slice() {
                                ["found", hash, word] => {
                                    info!(
                                        "Slave {} found the word {} behind the hash {}",
                                        slave_id, word, hash
                                    )
                                }
                                _ => warn!("Unknown request from slave {}: {}", slave_id, msg),
                            }
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
            execute_dude_msg(msg, &state.broadcast_tx);

            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    // On est poli, on pong quand on a un ping
                    Message::Ping(payload) => {
                        debug!("Ping received from dude");
                        if tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Text(msg) => execute_dude_msg(msg, &state.broadcast_tx),
                    _ => break,
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
