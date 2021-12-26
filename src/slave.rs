use alphabet::{get_number_from_word, get_word_from_number};
use clap::Parser;
use futures::{stream::SplitSink, SinkExt};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use std::ops::Range;
use tokio::{net::TcpStream, sync::watch, task::JoinHandle};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info, warn, Level};

// Le type du transmetteur qui envoie des messages au master
// je ne l'ai pas déterminé moi-même, pour rassurer le lecteur:
// j'ai juste copié collé la valeur de sortie de ws_stream.split()
type WebSocketSender = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(default_value = "ws://0.0.0.0:3000/ws")]
    ws_address: String,
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

    let connect_addr = Args::parse().ws_address;

    let url = url::Url::parse(&connect_addr).unwrap();

    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");

    let (mut tx_to_master, mut rx) = ws_stream.split();

    tx_to_master
        .send(Message::Text("slave".to_owned()))
        .await
        .unwrap_or_else(|err| {
            error!("Can't connect to master at {}: {}", &connect_addr, err);
            panic!()
        });

    info!("Successfully connected to master at {}", &connect_addr);

    let (tx_stop_search, rx_stop_search) = watch::channel(false);
    // Le but du jeu sera d'échanger l'ownership de ce tx avec la tâche de recherche
    // et le main thread. On va refiler le tx à la tâche puis récupérer l'ownership
    // au retour de la tâche asynchrone. Comme l'illustre l'enum ci dessous,
    // soit on a l'handle de la tâche, soit on a le tx.
    enum TxOrTask {
        Tx(WebSocketSender),
        Task(JoinHandle<WebSocketSender>),
    }
    use TxOrTask::*;
    let mut tx_or_task = Tx(tx_to_master);
    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            Message::Text(msg) => {
                let split: Vec<&str> = msg.split(' ').collect();
                match split.as_slice() {
                    &["search", hash_to_crack, begin, end] => {
                        info!("Search request from master");

                        let hash_hex_bytes = match hex::decode(hash_to_crack) {
                            Ok(bytes) => bytes,
                            Err(err) => {
                                warn!("Problem with hash: {}", err);
                                continue;
                            }
                        };

                        let (begin_num, end_num) =
                            match (get_number_from_word(begin), get_number_from_word(end)) {
                                (Ok(begin_num), Ok(end_num)) => (begin_num, end_num),
                                (Ok(_), Err(err)) => {
                                    warn!("Problem with end word: {}", err);
                                    continue;
                                }
                                (Err(err), Ok(_)) => {
                                    warn!("Problem with begin word: {}", err);
                                    continue;
                                }
                                (Err(err0), Err(err1)) => {
                                    warn!("Problem with both words: {};{}", err0, err1);
                                    continue;
                                }
                            };

                        if begin_num >= end_num {
                            warn!("Begin word greater or equal than end word, no need to continue");
                            continue;
                        }

                        debug!(
                            "{} word(s) in range [{};{})",
                            end_num - begin_num,
                            begin,
                            end
                        );

                        let tx_to_master = match tx_or_task {
                            Tx(tx) => tx,
                            Task(task) => {
                                info!("Recovering from previous search (stop if still running)...");
                                tx_stop_search.send(true).unwrap();
                                task.await.unwrap()
                            }
                        };

                        info!(
                            "Now cracking {} in range [{}; {})...",
                            hash_to_crack, begin, end
                        );

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
                    _ => warn!("Unknown request from master: {}", msg),
                }
            }
            _ => warn!("Non textual message from master: {:?}", msg),
        }
    }
}

async fn crack_hash(
    range: Range<usize>,
    hash_hex_bytes: Vec<u8>,
    hash_to_crack: String,
    mut tx: WebSocketSender,
    rx_stop_search: watch::Receiver<bool>,
) -> WebSocketSender {
    let mut hasher = Md5::new();
    for word in range.map(get_word_from_number) {
        if *rx_stop_search.borrow() {
            return tx;
        }
        hasher.update(&word);
        let hash = hasher.finalize_reset();
        if hash.as_slice() == hash_hex_bytes {
            info!(
                "{} cracked! The word behind it is {}. Notifying master...",
                hash_to_crack, word
            );
            tx.send(Message::Text(format!("found {} {}", hash_to_crack, word)))
                .await
                .unwrap_or_else(|err| {
                    error!("Can't send message via mpsc_websocket_tx: {}", err);
                });
            return tx;
        }
    }

    warn!("No corresponding hash has been found in the provided range");
    tx
}
