use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
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
    pub replied_to_msg_id: u64,
}

type ChannelMap = HashMap<u64, Vec<TcpStream>>;
type ServerMap = HashMap<u64, ChannelMap>;

type MessageDB = HashMap<(u64, u64), Vec<Message>>;

fn handle_client(stream: TcpStream, servers: Arc<Mutex<ServerMap>>, db: Arc<Mutex<MessageDB>>) {
    let peer_addr = stream.peer_addr().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut buf = String::new();
        let _n = match reader.read_line(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        if let Ok(msg) = serde_json::from_str::<Message>(&buf) {
            println!("Received from {}: {:?}", peer_addr, msg);
            {
                let mut db_lock = db.lock().unwrap();
                db_lock
                    .entry((msg.server, msg.channel))
                    .or_default()
                    .push(msg.clone());
            }
            let mut servers_lock = servers.lock().unwrap();
            let server = servers_lock.entry(msg.server).or_default();
            let channel = server.entry(msg.channel).or_default();

            if !channel.iter().any(|s| s.peer_addr().unwrap() == peer_addr) {
                channel.push(stream.try_clone().unwrap());
            }

            let data = serde_json::to_string(&msg).unwrap() + "\n";
            channel.retain(|mut peer| peer.write_all(data.as_bytes()).is_ok());
        }
    }
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:6969").unwrap();
    let servers: Arc<Mutex<ServerMap>> = Arc::new(Mutex::new(HashMap::new()));
    let db: Arc<Mutex<MessageDB>> = Arc::new(Mutex::new(HashMap::new()));

    for stream in listener.incoming() {
        let stream = stream.unwrap();
        let servers = servers.clone();
        let db = db.clone();
        thread::spawn(move || handle_client(stream, servers, db));
    }
}
