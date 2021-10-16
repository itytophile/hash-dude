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
    let splitted: Vec<&str> = msg.trim().split(' ').collect();
    if splitted.len() != 4 || splitted[0] != "search" {
        warn!("Unknown message from dude: {}", msg)
    } else {
        info!("Request '{}' received from dude, broadcasting...", msg);
        broadcast_tx.send(msg).expect("Broadcasting failed");
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
                    if let Message::Text(msg) = msg {
                        let splitted: Vec<&str> = msg.trim().split(' ').collect();
                        if splitted.len() != 3 || splitted[0] != "found" {
                            warn!("Unknown message from slave {}: {}", slave_id, msg)
                        } else {
                            info!(
                                "Slave {} found the word {} behind the hash {}",
                                slave_id, splitted[1], splitted[2]
                            )
                        }
                    } else {
                        warn!("Non textual message from slave {}: {:?}", slave_id, msg)
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
            while let Some(Ok(Message::Text(msg))) = rx.next().await {
                execute_dude_msg(msg, &state.broadcast_tx);
            }
            info!("Dude disconnected")
        }
    } else {
        warn!("Error with unknown client");
    }
    /*
        // By splitting we can send and receive at the same time.
        let (mut client_tx, mut client_rx) = stream.split();

        // Username gets set in the receive loop, if its valid
        let mut username = String::new();

        // Loop until a text message is found.
        if let Some(Ok(Message::Text(name))) = client_rx.next().await {
            // If username that is sent by client is not taken, fill username string.
            add_username_if_new(&state, &mut username, &name);

            // If not empty we want to quit the loop else we want to quit function.
            if username.is_empty() {
                // Only send our client that username is taken.
                let _ = client_tx
                    .send(Message::Text(String::from("Username already taken.")))
                    .await;

                return;
            }
        } else {
            return;
        }

        // Subscribe before sending joined message.
        let mut broadcast_rx = state.broadcast_tx.subscribe();

        // Send joined message to all subscribers.
        let msg = format!("{} joined.", username);
        let _ = state.broadcast_tx.send(msg);

        // This task will receive broadcast messages and send text message to our client.
        let mut send_task = tokio::spawn(async move {
            while let Ok(msg) = broadcast_rx.recv().await {
                // In any websocket error, break loop.
                if client_tx.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
        });

        // Clone things we want to pass to the receiving task.
        let tx = state.broadcast_tx.clone();
        let name = username.clone();

        // This task will receive messages from client and send them to broadcast subscribers.
        let mut recv_task = tokio::spawn(async move {
            while let Some(Ok(Message::Text(text))) = client_rx.next().await {
                // Add username before message.
                let _ = tx.send(format!("{}: {}", name, text));
            }
        });

        // If any one of the tasks exit, abort the other.
        tokio::select! {
            _ = (&mut send_task) => recv_task.abort(),
            _ = (&mut recv_task) => send_task.abort(),
        };

        // Send user left message.
        let msg = format!("{} left.", username);
        let _ = state.broadcast_tx.send(msg);

        // Remove username from map so new clients can take it.
        state.user_set.lock().unwrap().remove(&username);
    }*/
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("index.html"))
}
