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
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

// Our shared state
struct AppState {
    clients_id_order: Mutex<Vec<u32>>,
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
        clients_id_order,
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
        tracing::debug!("`{}` connected", user_agent.as_str());
    }
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(mut stream: WebSocket, state: Arc<AppState>) {
    if let Some(Ok(Message::Text(msg))) = stream.recv().await {
        if msg == "slave" {
            let client_id = {
                // On utilise ce bloc pour drop le mutex le plus tôt possible
                let mut clients_id_order = state.clients_id_order.lock().unwrap();
                let id = clients_id_order.iter().max().copied().unwrap_or(0) + 1;
                clients_id_order.push(id);
                id // En Rust, faire cela permet de faire "retourner" une valeur à partir du bloc
            };

            tracing::debug!("Slave connected with id = {}", client_id);

            while let Some(Ok(Message::Text(msg))) = stream.recv().await {
                tracing::debug!("Client says: {}", msg)
            }
        }
    } else {
        return;
    }
    while let Some(Ok(Message::Text(msg))) = stream.recv().await {
        tracing::debug!("Client says: {}", msg)
    }
    tracing::debug!("Client disconnected")
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
