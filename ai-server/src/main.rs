use std::{os::unix::ffi::OsStrExt, path::PathBuf, process::Stdio, sync::Arc};

use color_eyre::eyre::Result;
use futures::{SinkExt, StreamExt};
use scopeguard::defer;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tracing::info;
use uuid::Uuid;

struct ChildIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let listener = TcpListener::bind(":::3001").await?;
    info!("Listening on {}", listener.local_addr().unwrap());
    let child = Command::new("./model.py")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.unwrap();
    let stdout = BufReader::new(child.stdout.unwrap());

    let io = Arc::new(Mutex::new(ChildIo { stdin, stdout }));

    while let Ok((stream, _)) = listener.accept().await {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let io = io.clone();
        tokio::spawn(async move {
            _ = handle_ws(ws, io).await;
        });
    }

    Ok(())
}

async fn handle_ws(mut ws: WebSocketStream<TcpStream>, io: Arc<Mutex<ChildIo>>) -> Result<()> {
    info!("Got connection");

    // aggregate output
    let mut output = String::new();

    while let Some(res) = ws.next().await {
        let msg = res?;
        match msg {
            Message::Binary(data) => {
                if data.is_empty() {
                    info!("Replying to connection: {output}");
                    // done, just reply
                    ws.send(Message::Text(output)).await?;
                    break;
                }

                let mut io = io.lock().await;
                let filename =
                    PathBuf::from(Uuid::new_v4().to_string()).with_extension("webm");
                info!("Processing file {filename:?}");
                tokio::fs::write(&filename, &data).await?;
                // we'll just do synchronous cleanup...
                defer!({
                    info!("Cleaning up file {filename:?}");
                    _ = std::fs::remove_file(&filename);
                });
                io.stdin.write_all(filename.as_os_str().as_bytes()).await?;
                io.stdin.write_all(b"\n").await?;
                io.stdout.read_line(&mut output).await?;
            }
            Message::Ping(_) => {
                ws.send(Message::Pong(vec![])).await?;
            }
            Message::Close(_) => {
                // drop everything on the floor
                break;
            }
            _ => {}
        }
    }

    info!("Done handling connection");

    Ok(())
}