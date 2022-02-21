use alphabet::{get_bytes_from_number, get_number_from_word, increment_word};
use clap::Parser;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt,
};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use std::ops::Range;
use tokio::{net::TcpStream, signal::unix, sync::watch, task::JoinHandle};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info, warn, Level};
// Ces macros cfg permettent d'inclure du code en fonction de la plateforme
// pour laquelle on compile. Quand on produit un binaire statique avec glibc,
// le binaire n'embarque pas les biblio nécessaires pour certaines fonctionnalités
// de networking (glibc n'est pas recommandé pour la compilation statique).
// La seule fonctionnalité qui manque à notre binaire est de pouvoir chercher
// un ip à partir d'un nom de domaine. Le crate trust_dns_resolver
// nous permet de faire cela sans l'aide de C.
#[cfg(target_env = "gnu")]
use trust_dns_resolver::AsyncResolver;

// Le type du transmetteur qui envoie des messages au master
// je ne l'ai pas déterminé moi-même, pour rassurer le lecteur:
// j'ai juste copié collé le type de ws_stream.split()
type WebSocketSender = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WebSocketReceiver = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(default_value = "ws://0.0.0.0:3000/ws")]
    ws_address: String,
}

// Le but du jeu sera d'échanger l'ownership de ce tx avec la tâche de recherche
// et le main thread. On va refiler le tx à la tâche puis récupérer l'ownership
// au retour de la tâche asynchrone. Comme l'illustre l'enum ci dessous,
// soit on a l'handle de la tâche, soit on a le tx.
// Cela permet de certifier qu'il n'y a qu'une tâche qui peut utiliser
// le sender, ce qui fait plaisir à Rust.
enum TxOrTask {
    Tx(WebSocketSender),
    Task(JoinHandle<WebSocketSender>),
}
use TxOrTask::*;

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let connect_addr = Args::parse().ws_address;

    #[cfg(not(target_env = "gnu"))]
    let url = url::Url::parse(&connect_addr).unwrap();

    #[cfg(target_env = "gnu")]
    let mut url = url::Url::parse(&connect_addr).unwrap();
    #[cfg(target_env = "gnu")]
    {
        let resolver = AsyncResolver::tokio_from_system_conf().unwrap();
        let ip = resolver
            .lookup_ip(url.host_str().unwrap())
            .await
            .unwrap()
            .iter()
            .next()
            .unwrap();

        url.set_ip_host(ip).unwrap();
    }

    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");

    let (mut tx_to_master, rx) = ws_stream.split();

    tx_to_master
        .send(Message::Text("slave".to_owned()))
        .await
        .unwrap_or_else(|err| {
            error!("Can't connect to master at {connect_addr}: {err}");
            panic!()
        });

    info!("Successfully connected to master at {connect_addr}");

    let (tx_stop_search, rx_stop_search) = watch::channel(false);

    let mut sig_term = unix::signal(unix::SignalKind::terminate()).unwrap();

    tokio::select! {
        _ = sig_term.recv() => {},
        _ = listening_to_master(rx, Tx(tx_to_master), tx_stop_search, rx_stop_search) => {}
    }
}

async fn listening_to_master(
    mut rx: WebSocketReceiver,
    mut tx_or_task: TxOrTask,
    tx_stop_search: watch::Sender<bool>,
    rx_stop_search: watch::Receiver<bool>,
) {
    loop {
        match rx.next().await {
            Some(Ok(msg)) => {
                match msg {
                    Message::Text(msg) => {
                        let split: Vec<&str> = msg.split(' ').collect();
                        match split.as_slice() {
                            &["search", hash_to_crack, begin, end] => {
                                info!("Search request from master");

                                let hash_hex_bytes = match hex::decode(hash_to_crack) {
                                    Ok(bytes) => bytes,
                                    Err(err) => {
                                        warn!("Problem with hash: {err}");
                                        continue;
                                    }
                                };

                                let (begin_num, end_num) = match (
                                    get_number_from_word(begin),
                                    get_number_from_word(end),
                                ) {
                                    (Ok(begin_num), Ok(end_num)) => (begin_num, end_num),
                                    (Ok(_), Err(err)) => {
                                        warn!("Problem with end word: {err}");
                                        continue;
                                    }
                                    (Err(err), Ok(_)) => {
                                        warn!("Problem with begin word: {err}");
                                        continue;
                                    }
                                    (Err(err0), Err(err1)) => {
                                        warn!("Problem with both words: {err0};{err1}");
                                        continue;
                                    }
                                };

                                if begin_num >= end_num {
                                    warn!("Begin word greater or equal than end word, no need to continue");
                                    continue;
                                }

                                info!("{} word(s) in range [{begin};{end})", end_num - begin_num,);

                                let tx_to_master = match tx_or_task {
                                    Tx(tx) => tx,
                                    Task(task) => {
                                        info!("Recovering from previous search (stop if still running)...");
                                        tx_stop_search.send(true).unwrap();
                                        task.await.unwrap()
                                    }
                                };

                                info!("Now cracking {hash_to_crack} in range [{begin}; {end})...");

                                // On met le Stop à false avant chaque lancée
                                tx_stop_search.send(false).unwrap();

                                tx_or_task = Task(tokio::spawn(crack_hash(
                                    begin_num..end_num,
                                    hash_hex_bytes,
                                    hash_to_crack.to_owned(),
                                    tx_to_master,
                                    rx_stop_search.clone(),
                                )));
                            }
                            ["stop"] => {
                                tx_or_task = match tx_or_task {
                                    Tx(tx) => {
                                        info!("No search task to stop");
                                        Tx(tx)
                                    }
                                    Task(task) => {
                                        info!("Stop request from master, aborting...");
                                        tx_stop_search.send(true).unwrap();
                                        let tx = task.await.unwrap();
                                        info!("Aborted");
                                        Tx(tx)
                                    }
                                };
                            }
                            ["exit"] => {
                                info!("Exit request from master, exiting...");
                                tx_stop_search.send(true).unwrap();
                                break;
                            }
                            _ => warn!("Unknown request from master: {msg}"),
                        }
                    }
                    _ => warn!("Non textual message from master: {msg:?}"),
                }
            }
            Some(Err(err)) => {
                error!("{err}");
                break;
            }
            None => {
                info!("Channel closed without error.");
                break;
            }
        }
    }
}

const ITERATIONS_WITHOUT_CHECKING: usize = 100000;

async fn crack_hash(
    range: Range<usize>,
    hash_hex_bytes: Vec<u8>,
    hash_to_crack: String,
    mut tx: WebSocketSender,
    rx_stop_search: watch::Receiver<bool>,
) -> WebSocketSender {
    let mut hasher = Md5::new();
    let mut buffer = [0; 10];
    let mut end_buffer = [0; 10];
    let mut iteration_count = 1;

    let mut slice = get_bytes_from_number(range.start, &mut buffer);
    let end_slice = get_bytes_from_number(range.end, &mut end_buffer);

    while slice != end_slice {
        // Ce délire de compte est une optimisation. Cela permet de moins regarder le
        // channel pour savoir si la tâche doit stopper (ce qui prend du temps). La tâche
        // prend donc un peu plus de temps pour se stopper, mais cela ne se ressent pas.
        if iteration_count % ITERATIONS_WITHOUT_CHECKING == 0 && *rx_stop_search.borrow() {
            return tx;
        }

        hasher.update(slice);

        if hasher.finalize_reset().as_slice() == hash_hex_bytes {
            let word = std::str::from_utf8(slice).unwrap();
            info!("{hash_to_crack} cracked! The word behind it is {word}. Notifying master...");
            tx.send(Message::Text(format!("found {hash_to_crack} {word}")))
                .await
                .unwrap_or_else(|err| {
                    error!("Can't send message via mpsc_websocket_tx: {err}");
                });
            return tx;
        }

        let len = slice.len();

        slice = if let Some(slice) = increment_word(&mut buffer) {
            slice
        } else {
            &buffer[10 - len..]
        };

        iteration_count += 1;
    }

    warn!("No corresponding hash has been found in the provided range");
    tx
}
