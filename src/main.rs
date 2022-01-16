mod msg;

use alphabet::get_word_from_number;
use axum::{
    body::{boxed, Full},
    extract::{
        ws::{self, WebSocket, WebSocketUpgrade},
        Extension, TypedHeader,
    },
    http::header,
    response::{Html, IntoResponse, Response},
    routing::get,
    AddExtensionLayer, Router,
};
use clap::Parser;
use futures::{
    sink::SinkExt,
    stream::{SplitSink, SplitStream, StreamExt},
};
use headers::HeaderValue;
use msg::ToSlaveMessage;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn, Level};

// Our shared state
struct AppState {
    slaves_id_order: Mutex<Vec<u32>>,
    request_queue: Mutex<Vec<(String, std::ops::Range<usize>)>>,
    tx_to_slaves: broadcast::Sender<ToSlaveMessage>,
    tx_to_listeners: broadcast::Sender<usize>,
    tx_to_dudes: broadcast::Sender<String>,
}

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long, default_value = "0.0.0.0:3000")]
    address: String,
}

#[tokio::main]
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
        .layer(AddExtensionLayer::new(app_state));

    tracing::info!("Listening on {addr}");
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

            let mut rx = state.tx_to_listeners.subscribe();

            tokio::spawn(async move {
                while let Ok(new_size) = rx.recv().await {
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

            while let Some(Ok(msg)) = rx_from_client.next().await {
                match msg {
                    // On est poli, on pong quand on a un ping
                    ws::Message::Ping(payload) => {
                        debug!("Ping received from dude");
                        // on utilise le tx_proxy originel non cloné
                        if tx_proxy.send(ws::Message::Pong(payload)).await.is_err() {
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
                                        if let Err(err) = state
                                            .tx_to_dudes
                                            .send(format!("info Cracking {hash}..."))
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

                                        broadcast_message(
                                            &state.tx_to_listeners,
                                            request_queue.len(),
                                        );

                                        broadcast_message(&state.tx_to_slaves, message);

                                        if !request_queue.is_empty() {
                                            info!("Sending queued request: {:?}", request_queue[0]);

                                            let (hash, range) = &request_queue[0];
                                            broadcast_message(
                                                &state.tx_to_slaves,
                                                ToSlaveMessage::Search(hash.clone(), range.clone()),
                                            );

                                            if let Err(err) = state
                                                .tx_to_dudes
                                                .send(format!("info Cracking {hash}..."))
                                            {
                                                warn!(
                                                    "Can't tell dudes that hash is cracking: {err}"
                                                );
                                            }
                                        }
                                    } else {
                                        if let Err(err) = state
                                            .tx_to_dudes
                                            .send("info Nothing to stop".to_owned())
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

                                        broadcast_message(
                                            &state.tx_to_listeners,
                                            request_queue.len(),
                                        );
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

            info!("Dude disconnected")
        }
        msg => {
            warn!("Unknown type provided: {msg}")
        }
    }
}

async fn master_to_slave_relay_task(
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
                            "Slave {slave_id} found the word {word} behind the hash {hash}. Now stopping all slaves..."
                        );

                        // on send le message tel quel. Le dude veut aussi un found <hash> <word>
                        if let Err(err) = state.tx_to_dudes.send(msg) {
                            warn!("Can't tell dudes that word was found: {err}");
                        }

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

                            if let Err(err) =
                                state.tx_to_dudes.send(format!("info Cracking {hash}..."))
                            {
                                warn!("Can't tell dudes that hash is cracking: {err}");
                            }
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
