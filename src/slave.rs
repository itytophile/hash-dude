use std::env;

use futures::SinkExt;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, error, info, warn};

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

    let (mut tx, mut rx) = ws_stream.split();

    // Permet d'envoyer des messages au master dans plusieurs threads
    // il y a peut-être moyen de s'en passer en jouant correctement
    // avec l'ownership du tx
    let (mpsc_websocket_tx, mut mpsc_rx) = mpsc::channel::<Message>(100);

    tx.send(Message::Text("slave".to_owned()))
        .await
        .unwrap_or_else(|err| {
            error!("Can't connect to master at {}: {}", &connect_addr, err);
            panic!()
        });

    info!("Successfully connected to master at {}", &connect_addr);

    let mut write_listening_task = tokio::spawn(async move {
        while let Some(msg) = mpsc_rx.recv().await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut master_listening_task = tokio::spawn(async move {
        let mut cracking_task: Option<JoinHandle<()>> = None;
        while let Some(Ok(msg)) = rx.next().await {
            match msg {
                Message::Text(msg) => {
                    let splitted: Vec<&str> = msg.split(' ').collect();
                    match splitted.as_slice() {
                        &["search", hash_to_crack, begin, end] => {
                            info!("Search request from master");
                            if let Some(task) = cracking_task.as_ref() {
                                info!("Stopping previous search...");
                                task.abort();
                            }
                            info!(
                                "Now cracking {} in range [{}; {})...",
                                hash_to_crack, begin, end
                            );

                            match hex::decode(hash_to_crack) {
                                Ok(hash_hex_bytes) => {
                                    if let (Some(begin_num), Some(end_num)) =
                                        (get_number_from_word(begin), get_number_from_word(end))
                                    {
                                        if begin_num >= end_num {
                                            warn!(
                                                "Can't search in range [{};{}), sorry",
                                                begin, end
                                            );
                                            continue;
                                        }
                                        debug!(
                                            "{} word(s) in range [{};{})",
                                            end_num - begin_num,
                                            begin,
                                            end
                                        );
                                        // On clone à chaque fois pour éviter les problèmes d'ownership
                                        let mpsc_websocket_tx = mpsc_websocket_tx.clone();
                                        // On own tout ça sinon au prochain tour de boucle on va les perdre
                                        let hash_to_crack = hash_to_crack.to_owned();
                                        cracking_task = Some(tokio::spawn(async move {
                                            let mut hasher = Md5::new();
                                            for word in
                                                (begin_num..end_num).map(get_word_from_number)
                                            {
                                                hasher.update(&word);
                                                let hash = hasher.finalize_reset();
                                                if hash.as_slice() == hash_hex_bytes {
                                                    info!(
                                                            "{} cracked! The word behind it is {}. Notifying master...",
                                                            hash_to_crack, word
                                                        );
                                                    mpsc_websocket_tx
                                                        .send(Message::Text(format!(
                                                            "found {} {}",
                                                            hash_to_crack, word
                                                        )))
                                                        .await
                                                        .unwrap_or_else(|err| {
                                                            error!(
                                                                "Can't send message via mpsc_websocket_tx: {}",
                                                                err
                                                            );
                                                        });
                                                    return;
                                                }
                                            }
                                            warn!("No corresponding hash was found in the provided range")
                                        }));
                                    } else {
                                        warn!("Unsupported letter(s) in provided range, search aborted");
                                    }
                                }
                                Err(err) => warn!("Problem with hash: {}", err),
                            }
                        }
                        ["stop"] => {
                            if let Some(task) = cracking_task.as_ref() {
                                info!("Stop request from master, aborting...");
                                task.abort();
                                cracking_task = None;
                                info!("Search aborted")
                            } else {
                                info!("No search to stop")
                            }
                        }
                        ["exit"] => {
                            info!("Exit request from master, exiting...");
                            break;
                        }
                        _ => warn!("Unknown request from master: {}", msg),
                    }
                }
                _ => warn!("Non textual message from master: {:?}", msg),
            }
        }
    });

    tokio::select! {
        _ = (&mut write_listening_task) => master_listening_task.abort(),
        _ = (&mut master_listening_task) => write_listening_task.abort(),
    };
}

const BASE: usize = 62;
const ALPHABET: [char; BASE] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L',
    'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9',
];
