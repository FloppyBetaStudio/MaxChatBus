

use tokio_rustls::{rustls, server::TlsStream, TlsAcceptor};
use std::{
    collections::hash_map::{Entry, HashMap}, io, sync::{Arc, Mutex}
};

use tokio::{io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf}, net::{TcpListener, TcpStream}, sync::mpsc, task};

type Sender<T> = mpsc::Sender<T>;
type Receiver<T> = mpsc::Receiver<T>;

use crate::APP_CONFIG;

#[derive(Clone)]
pub struct ClientInfo {
    name: String,
    group: String,
}

struct Client {
    info: ClientInfo,
    sender: Sender<String>,
}

pub enum Event {
    OnMessage {
        info: ClientInfo,
        content_formatted: String,
    },
    OnBroadcastMessage {
        content_formatted: String,
    },
    OnNewClient {
        info: ClientInfo,
        stream: WriteHalf<TlsStream<TcpStream>>,
    },
    OnDisconnect{
        name: String,
    }
}

async fn broker_loop(mut events: Receiver<Event>, qq_sender: Sender<String>) {
    let mut peers: HashMap<String, Client> = HashMap::new();

    while let Some(event) = events.recv().await {
        match event {
            Event::OnMessage {
                info,
                content_formatted,
            } => {
                println!("Broker OnMessage | Recv: {}", content_formatted);

                //iter
                for (_k, v) in &peers{
                    if v.info.group != info.group { continue;}
                    if v.info.name == info.name {continue;}
                    let _ = v.sender.send(content_formatted.clone()).await;
                }

                //QQ
                let _ = qq_sender.send(content_formatted).await;
            }
            Event::OnBroadcastMessage { content_formatted } =>{
                println!("Broker OnBroadcastMessage | Recv: {}", content_formatted);
                for (_k, v) in &peers{
                    let _ = v.sender.send(content_formatted.clone()).await;
                }
            }
            Event::OnNewClient {
                info,
                stream,
            } => {
                if (info.name.len() < 2) || (info.group.len() < 2) {
                    eprintln!("Illegal client name or group");
                } else {
                    match peers.entry(info.name.clone()) {
                        Entry::Occupied(..) => (),
                        Entry::Vacant(entry) => {
                            let (client_sender, client_receiver) = mpsc::channel(32);
                            entry.insert(Client{info: info, sender: client_sender});
                            tokio::spawn(connection_writer_loop(client_receiver, stream));
                        }
                    }
                }
            }
            Event::OnDisconnect { name } => {
                peers.remove(&name);
            }
        }
    }
}

async fn connection_writer_loop(
    mut client_receiver: Receiver<String>,
    mut stream: WriteHalf<TlsStream<TcpStream>>,
) {
    
    while let Some(s) = client_receiver.recv().await {
        if s.len() == 0 {break;}
        let object = json::object! {
            "type": "text",
            "content": s,
        };
        let s = object.dump() + "\n";
        let _ = stream.write_all(&s.as_bytes()).await;
    }
}

async fn connection_reader_loop(broker_sender: Sender<Event>, reader: &mut BufReader<ReadHalf<TlsStream<TcpStream>>>, client_info: ClientInfo){
    loop {
        let mut buf = String::new();
        let _ = reader.read_line(&mut buf).await;
        match json::parse(&buf){
            Ok(obj) =>{
                match obj["type"].as_str(){
                    Some("text_group") => {
                        //handler
                        let formatted = format!("[{}]<{}>{}", client_info.name, &obj["sender"], &obj["content"]);
                        let _ = broker_sender.send(Event::OnMessage { info: client_info.clone(), content_formatted: formatted }).await;
                    },
                    Some("text_broadcast") => {
                        todo!();
                    },
                    Some(_) => {

                    }
                    None => {
                        eprint!("Connection reader got nothing");
                    },
                }
            },
            Err(e) => {
                eprintln!("Disconnecting due to error in connection reader loop: {}", e);
                let _ = broker_sender.send(Event::OnDisconnect { name: client_info.name }).await;
                return;
            },
        };
    }
}

pub async fn tcp_serer(listen_on: &str, sender: &Mutex<Option<Sender<Event>>>, qq_sender: Sender<String>) -> io::Result<()> {
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(&mut std::fs::File::open("cert.pem")?))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let private_key = rustls_pemfile::private_key(&mut std::io::BufReader::new(&mut std::fs::File::open("key.pem")?))
        .unwrap()
        .unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, private_key)
        .unwrap();

    let listener = TcpListener::bind(listen_on).await;

    match listener {
        Ok(_) => println!("Listening on {}", listen_on),

        // Error binding to the address
        Err(e) => {
            println!("here {}", e);
            return Ok(());
        }
    }

    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = listener.unwrap();

    let (broker_sender, broker_receiver) = mpsc::channel(32);
    let _broker_handle = task::spawn(broker_loop(broker_receiver, qq_sender));

    //Provide sender for QQ
    {let _ = sender.lock().unwrap().insert(broker_sender.clone());}
    

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let broker_sender = broker_sender.clone();

        let fut = async move {
            let mut authorized = false;
            let stream = acceptor.accept(stream).await?;
            {
                
                let (stream_r, stream_w) = split(stream);
                println!("{} | Incoming TCP connection", peer_addr);

                let mut name = String::new();
                let mut group = String::new();
                
                {
                    let mut reader = BufReader::new(stream_r);
                    let mut counter: u8 = 0;

                    while counter < 3 {
                        let mut buf: String = String::new();
                        let _ = reader.read_line(&mut buf).await;

                        if buf.len() < 2 { continue; }
                        
                        println!("{} | Counter: {} | Recv: {}", peer_addr, counter, buf);

                        match json::parse(&buf) {
                            Ok(obj) => {
                                if obj["token"].to_string() != APP_CONFIG.server_token {
                                    eprintln!("{} | Unexpected auth token: Got {}; Expect {}", peer_addr, obj["token"], APP_CONFIG.server_token);
                                    counter += 1;
                                    continue;
                                }

                                if (obj["name"].to_string().len() < 2)
                                    || (obj["group"].to_string().len() < 2)
                                {
                                    eprintln!("{} | Illegal client name or group", peer_addr);
                                    counter += 1;
                                    continue;
                                }

                                name = obj["name"].to_string();
                                group = obj["group"].to_string();

                                break;
                            }
                            Err(e) => eprintln!("{} | Header info parsing failed: {}", peer_addr, e),
                        }

                        counter += 1;
                    }
                    if counter == 3 {
                        eprintln!("{} | Exceeded maximum number of attempts", peer_addr);
                    } else {
                        authorized = true;
                    }

                    if authorized {
                        //Established
                        let a_broker_sender = broker_sender.clone();
                        let info = ClientInfo {name: name, group: group};
                        let a_info = info.clone();
                        task::spawn(async move {
                            connection_reader_loop(a_broker_sender, &mut reader, a_info).await;
                        });
                        let _ = broker_sender.send(Event::OnNewClient { info, stream: stream_w }).await;
                    } 
                }

                
            }

            Ok(()) as io::Result<()>
        };

        task::spawn(async move {
            if let Err(err) = fut.await {
                eprintln!("{:?}", err)
            }
        });
    }
}
