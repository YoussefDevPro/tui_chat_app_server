use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub sender: u64,
    pub server: u64,
    pub channel: u64,
    pub content: String,
    pub is_replying: bool,
    pub replied_to_msg: u64,
}

type ChannelMap = HashMap<u64, Vec<TcpStream>>;
type ServerMap = HashMap<u64, ChannelMap>;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:6969").unwrap();
    let servers: Arc<Mutex<ServerMap>> = Arc::new(Mutex::new(HashMap::new()));

    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let servers = servers.clone();

        thread::spawn(move || {
            let mut buf = [0; 1024];
            loop {
                let n = match stream.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => n,
                    Err(_) => return,
                };
                if let Ok(msg) = serde_json::from_slice::<Message>(&buf[..n]) {
                    println!("{:?}", msg);
                    let mut servers_lock = servers.lock().unwrap();
                    let server = servers_lock.entry(msg.server).or_default();
                    let channel = server.entry(msg.channel).or_default();

                    if !channel
                        .iter()
                        .any(|s| s.peer_addr().unwrap() == stream.peer_addr().unwrap())
                    {
                        channel.push(stream.try_clone().unwrap());
                    }

                    for peer in channel.iter_mut() {
                        let _ = peer.write_all(&serde_json::to_vec(&msg).unwrap());
                    }
                }
            }
        });
    }
}
