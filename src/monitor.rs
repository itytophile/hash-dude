use clap::Parser;
use futures::{stream::SplitStream, SinkExt};
use futures_util::StreamExt;
use tokio::{net::TcpStream, signal::unix};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info, warn, Level};

// Le type du transmetteur qui envoie des messages au master
// je ne l'ai pas déterminé moi-même, pour rassurer le lecteur:
// j'ai juste copié collé le type de sortie de ws_stream.split()
type WebSocketReceiver = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    #[clap(short, long, default_value = "ws://0.0.0.0:3000/ws")]
    address: String,
    #[clap(short, long, default_value = "stackhash_slave")]
    service: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var_os("RUST_LOG").is_none() {
        tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let Args { address, service } = Args::parse();

    let url = url::Url::parse(&address).unwrap();

    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");

    let (mut tx_to_master, rx) = ws_stream.split();

    tx_to_master
        .send(Message::Text("queue length".to_owned()))
        .await
        .unwrap_or_else(|err| {
            error!("Can't connect to master at {address}: {err}");
            panic!()
        });

    info!("Successfully connected to master at {address}");

    let mut sig_term = unix::signal(unix::SignalKind::terminate()).unwrap();

    tokio::select! {
        _ = sig_term.recv() => {},
        _ = listening_to_master(rx, &service) => {}
    }
}

const REQUIRED_REPLICAS_COUNT_FOR_LENGTH: [(usize, usize); 4] =
    [(2, 1), (4, 2), (8, 4), (usize::MAX, 8)];

async fn listening_to_master(mut rx: WebSocketReceiver, slave_service: &str) {
    let mut replicas_count = 1;

    loop {
        match rx.next().await {
            Some(Ok(msg)) => match msg {
                Message::Binary(bytes) => {
                    let mut buffer = [0; 8];
                    buffer.copy_from_slice(bytes.as_slice());
                    let queue_length = usize::from_be_bytes(buffer);
                    info!("{queue_length} request(s) in queue");
                    for (length, count) in REQUIRED_REPLICAS_COUNT_FOR_LENGTH {
                        if queue_length <= length {
                            if replicas_count != count {
                                info!("Updating slaves: {replicas_count} -> {count}");
                                info!(
                                    "{:?}",
                                    std::process::Command::new("docker")
                                        .args(["service", "scale"])
                                        .arg(format!("{slave_service}={count}"))
                                        .output()
                                        .unwrap()
                                        .status
                                );
                                replicas_count = count;
                            }
                            break;
                        }
                    }
                }
                message => warn!("Unknown message received: {message}"),
            },
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
