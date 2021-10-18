use futures::{stream::SplitSink, SinkExt};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use std::{env, ops::Range};
use tokio::{net::TcpStream, sync::watch, task::JoinHandle};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info, warn};

type WebSocketSender = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

// algo trouvé par chance
fn get_word_from_number(mut num: usize) -> String {
    let mut word = String::new();
    loop {
        word.insert(0, ALPHABET[num % BASE]);
        num /= BASE;
        if num == 0 {
            break word;
        }
        num -= 1; // la petite douille de la chance
    }
}
// algo trouvé aussi par chance
fn get_number_from_word(word: &str) -> Option<usize> {
    let mut num = 0;
    for (index, c) in word.chars().rev().enumerate() {
        let letter_index = ALPHABET.iter().position(|&a| a == c)?;

        num += (letter_index + 1) * 62_u64.pow(index as u32) as usize;
    }
    Some(num - 1)
}

#[tokio::main]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "slave=debug")
    }

    tracing_subscriber::fmt::init();
    let connect_addr = env::args()
        .nth(1)
        .expect("This program needs a WebSocket URL as first argument!");

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
                let splitted: Vec<&str> = msg.split(' ').collect();
                match splitted.as_slice() {
                    &["search", hash_to_crack, begin, end] => {
                        info!("Search request from master");

                        match hex::decode(hash_to_crack) {
                            Ok(hash_hex_bytes) => {
                                if let (Some(begin_num), Some(end_num)) =
                                    (get_number_from_word(begin), get_number_from_word(end))
                                {
                                    if begin_num >= end_num {
                                        warn!("Can't search in range [{};{}), sorry", begin, end);
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
                                } else {
                                    warn!(
                                        "Unsupported letter(s) in provided range, search aborted"
                                    );
                                }
                            }
                            Err(err) => warn!("Problem with hash: {}", err),
                        }
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

    warn!("No corresponding hash was found in the provided range");
    tx
}

const BASE: usize = 62;
const ALPHABET: [char; BASE] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L',
    'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9',
];
