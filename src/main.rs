mod msg;
mod server;
use crate::server::{
    to_dude::dude_listening_task,
    to_slave::{master_to_slave_relay_task, slave_listening_task},
};
use axum::{
    body::{boxed, Full},
    extract::{
        ws::{self, WebSocket, WebSocketUpgrade},
        Extension, TypedHeader,
    },
    http::header,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use futures::{sink::SinkExt, stream::StreamExt};
use headers::HeaderValue;
use msg::ToSlaveMessage;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::{
    signal::unix,
    sync::{broadcast, mpsc},
};
use tracing::{debug, info, warn, Level};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
    request_queue: Mutex<Vec<(String, std::ops::Range<usize>)>>,
    tx_to_slaves: broadcast::Sender<ToSlaveMessage>,
    tx_to_listeners: broadcast::Sender<usize>,
    tx_to_dudes: broadcast::Sender<String>,
}

impl AppState {
    fn send_to_dudes(&self, msg: String, err_msg: &'static str) {
        if let Err(err) = self.tx_to_dudes.send(msg) {
            warn!("{err_msg}: {err}");
        }
    }
}

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long, default_value = "0.0.0.0:3000")]
    address: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    if std::env::var_os("RUST_LOG").is_none() {
        tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let addr: SocketAddr = args.address.parse().expect("Can't parse provided address");

    let (tx_to_slaves, _) = broadcast::channel(100);
    let (tx_to_listeners, _) = broadcast::channel(100);
    let (tx_to_dudes, _) = broadcast::channel(100);

    let app_state = Arc::new(AppState {
        slaves_id_order: Mutex::new(Vec::new()),
        request_queue: Mutex::new(Vec::new()),
        tx_to_slaves,
        tx_to_listeners,
        tx_to_dudes,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/css", get(css))
        .route("/ws", get(websocket_handler))
        .layer(Extension(app_state));

    tracing::info!("Listening on {addr}");
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(async {
            unix::signal(unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
        })
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
    let (mut tx_to_client, mut rx_from_client) = stream.split();

    let msg = if let Some(Ok(ws::Message::Text(msg))) = rx_from_client.next().await {
        msg
    } else {
        warn!("Error with unknown client");
        return;
    };

    match msg.as_str() {
        "slave" => {
            let slave_id = {
                // On utilise ce bloc pour drop le mutex le plus tôt possible
                let mut slaves_id_order = state.slaves_id_order.lock().unwrap();
                let id = slaves_id_order.iter().max().copied().unwrap_or(0) + 1;
                slaves_id_order.push(id);
                id // En Rust, faire cela permet de faire "retourner" une valeur à partir du bloc
            };

            info!("Slave connected with id = {slave_id}, restarting task...");

            // on stoppe tout le monde pour repartager la tâche avec le nouveau
            broadcast_message(&state.tx_to_slaves, ToSlaveMessage::Stop);

            // on crée le receveur avant d'envoyer un potentiel message de recherche
            let rx_from_master = state.tx_to_slaves.subscribe();

            {
                // On utilise ce bloc pour drop le mutex le plus tôt possible
                let request_queue = state.request_queue.lock().unwrap();
                if !request_queue.is_empty() {
                    let (hash, range) = &request_queue[0];
                    broadcast_message(
                        &state.tx_to_slaves,
                        ToSlaveMessage::Search(hash.clone(), range.clone()),
                    )
                }
            }

            tokio::spawn(master_to_slave_relay_task(
                rx_from_master,
                Arc::clone(&state),
                slave_id,
                tx_to_client,
            ));

            slave_listening_task(rx_from_client, slave_id, &state).await;

            state
                .slaves_id_order
                .lock()
                .unwrap()
                .retain(|&id| id != slave_id);

            // on stoppe tout le monde pour repartager la tâche avec les esclaves restants
            broadcast_message(&state.tx_to_slaves, ToSlaveMessage::Stop);

            info!("Slave {slave_id} disconnected");

            let request_queue = state.request_queue.lock().unwrap();
            if !request_queue.is_empty() {
                let (hash, range) = &request_queue[0];
                broadcast_message(
                    &state.tx_to_slaves,
                    ToSlaveMessage::Search(hash.clone(), range.clone()),
                )
            }
        }
        "queue length" => {
            info!("Listener connected");

            let mut rx_from_master = state.tx_to_listeners.subscribe();

            tokio::spawn(async move {
                while let Ok(new_size) = rx_from_master.recv().await {
                    if let Err(err) = tx_to_client
                        .send(ws::Message::Binary(new_size.to_be_bytes().to_vec()))
                        .await
                    {
                        // Permet de jeter cette tâche qui ne sert plus
                        // car le listener n'est plus connecté
                        warn!("Can't send to listener: {err}");
                        break;
                    }
                }
            });

            while let Some(Ok(msg)) = rx_from_client.next().await {
                match msg {
                    ws::Message::Close(_) => break,
                    msg => warn!("Unknown message from listener: {msg:?}"),
                }
            }

            info!("Listener disconnected");
        }
        "dude" => {
            info!("Dude connected");

            let mut rx = state.tx_to_dudes.subscribe();

            // On ne peut pas utiliser le tx_to_client dans plusieurs tâches
            // nous devons "emballer" ce tx dans un mpsc (multi producer single consumer)
            // pour multiplier des tx (tx_proxy dans notre cas)
            let (tx_proxy, mut rx_proxy) = mpsc::channel::<ws::Message>(100);

            // La tâche du proxy
            tokio::spawn(async move {
                while let Some(message_to_dude) = rx_proxy.recv().await {
                    if let Err(err) = tx_to_client.send(message_to_dude).await {
                        warn!("Can't send to dude: {err}");
                        break;
                    }
                }
            });

            // La tâche qui écoute les messages pour tous les dudes
            {
                // On ouvre des accolades pour shadow tranquillement tx_proxy
                // C'est pour le style, pour éviter de marquer let tx_proxy_clone = tx_proxy.clone()
                // à la sortie des accolades on va retrouver le tx_proxy originel non cloné
                let tx_proxy = tx_proxy.clone();
                tokio::spawn(async move {
                    while let Ok(message_to_dude) = rx.recv().await {
                        if let Err(err) = tx_proxy.send(ws::Message::Text(message_to_dude)).await {
                            // Permet de jeter cette tâche qui ne sert plus
                            // car le listener n'est plus connecté
                            warn!("Can't send to proxy: {err}");
                            break;
                        }
                    }
                });
            }

            dude_listening_task(rx_from_client, tx_proxy, state).await;

            info!("Dude disconnected")
        }
        msg => {
            warn!("Unknown type provided: {msg}")
        }
    }
}

fn broadcast_message<T>(tx: &broadcast::Sender<T>, message: T) {
    if let Err(err) = tx.send(message) {
        warn!("Can't broadcast: {err}")
    }
}

async fn index() -> Html<&'static str> {
    // Le include_str! est une macro qui permet de charger un fichier
    // à la compilation (le texte sera directement dans le binaire)
    Html(include_str!("index.html"))
}

async fn css() -> Response {
    // Obligé de faire ça pour bien dire que c'est du css
    // au lieu d'envoyer direct du text/plain
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static(mime::TEXT_CSS_UTF_8.as_ref()),
        )
        .body(boxed(Full::from(include_str!("bulma.min.css"))))
        .unwrap()
}
