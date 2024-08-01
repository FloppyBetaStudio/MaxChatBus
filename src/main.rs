use json::object;
use std::sync::Mutex;

use futures_util::{SinkExt, StreamExt};
use server::Event;
use tokio::sync::mpsc;

use tokio_tungstenite::{
    connect_async,
    tungstenite::{http::Uri, protocol::Message, ClientRequestBuilder},
};

use lazy_static::lazy_static;

mod server;

type Sender<T> = mpsc::Sender<T>;

struct AppConfig {
    qq_api: String,
    qq_token: String,
    server_token: String,
    qq_group: u32,
    qq_self: u32,
    listen: String,
}

lazy_static! {
    static ref BROKER_SENDER: Mutex<Option<Sender<server::Event>>> = Mutex::new(None);
    static ref APP_CONFIG: AppConfig = load_config();
}

fn gen_qq_send_text_message(msg: String) -> String {
    let data = object! {
        "action": "send_group_msg",
        "params": {
            "group_id": APP_CONFIG.qq_group,
            "message": msg,
            "auto_escape": true
        }
    };
    data.dump()
}

fn load_config() -> AppConfig {
    let config_file = std::fs::read_to_string("config.json").expect(
        "Loading config.json failed! Demo: \n{
    \"qq_api\": \"ws://127.0.0.1:3001\",
    \"qq_token\": \"aaaaa\",
    \"server_token\": \"aaaaa\",
    \"qq_group\": 114514,
    \"qq_self\": 1919810,
    \"listen\": \"0.0.0.0:2001\"
}",
    );
    let config_data = json::parse(&config_file).expect("Parsing config.json failed");

    let config = AppConfig {
        qq_api: config_data["qq_api"].to_string(),
        qq_token: config_data["qq_token"].to_string(),
        server_token: config_data["server_token"].to_string(),
        qq_group: config_data["qq_group"].as_u32().unwrap(),
        qq_self: config_data["qq_self"].as_u32().unwrap(),
        listen: config_data["listen"].to_string(),
    };

    return config;
}

#[tokio::main]
async fn main() {
    let authorization_full = format!("Bearer {}", APP_CONFIG.qq_token);
    let uri: Uri = APP_CONFIG.qq_api.parse().unwrap();

    let builder = ClientRequestBuilder::new(uri)
        .with_header("Authorization", authorization_full.to_owned())
        .with_header("Content-Type", "application/json; charset=utf-8");

    let (stream, _) = connect_async(builder).await.expect("Failed to connect");

    let (qq_sender, mut qq_receiver) = mpsc::channel(32);
    let (mut stream_write, mut stream_read) = stream.split();

    tokio::spawn(async move {
        while let Some(s) = qq_receiver.recv().await {
            let _ = stream_write
                .send(Message::Text(gen_qq_send_text_message(s)))
                .await;
        }
    });

    //Listen servers
    tokio::spawn(async {
        let _ = server::tcp_serer(&APP_CONFIG.listen, &BROKER_SENDER, qq_sender).await;
        println!("TCP server stopped");
    });

    loop {
        let msg = stream_read.next().await.expect("Error reading message");
        let msg_text = msg.unwrap().to_string();
        //println!("{}", &msg_text);
        let msg_parsed = match json::parse(&msg_text) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{}", e.to_string());
                continue;
            }
        };

        //Message filter
        if msg_parsed["post_type"] != "message" {
            continue;
        }
        if msg_parsed["message_type"] != "group" {
            continue;
        }
        if msg_parsed["self_id"] != APP_CONFIG.qq_self {
            continue;
        }
        if msg_parsed["group_id"] != APP_CONFIG.qq_group {
            continue;
        }

        //Receive messages
        //println!("QQ|Received: {}; from: {}", msg_parsed["raw_message"], msg_parsed["sender"]["nickname"]);
        //sendmsg_broadcast_from_qq(&msg_parsed["sender"]["nickname"].to_string(), &msg_parsed["raw_message"].to_string());

        //Parse message array
        let mut content: String = String::new();

        for msg_element in msg_parsed["message"].members() {
            let msg_type = msg_element["type"].as_str().unwrap();
            let msg_data = &msg_element["data"];
            match msg_type {
                "text" => {
                    content.push_str(msg_data["text"].as_str().unwrap());
                }
                "image" => {
                    content.push_str("[图片]");
                }
                "face" => {
                    content.push_str("[表情]");
                }
                "recard" => {
                    content.push_str("[语音]");
                }
                "video" => {
                    content.push_str("[影片]");
                }
                "at" => {
                    let param = msg_data["qq"].as_str().unwrap();
                    content.push_str(&format!("[@{}]", param));
                }
                "share" => {
                    let param = msg_data["title"].as_str().unwrap();
                    content.push_str(&format!("[链接:{}]", param));
                }
                _ => {
                    content.push_str("[特殊消息]");
                }
            }
        }

        let content_formatted = format!(
            "[{}]<{}>{}",
            "QQ Group",
            &msg_parsed["sender"]["nickname"].to_string(),
            content
        );

        match BROKER_SENDER.lock().unwrap().as_mut() {
            Some(s) => {
                let _ = s
                    .send(Event::OnBroadcastMessage {
                        content_formatted: content_formatted,
                    })
                    .await;
            }
            None => todo!(),
        }
    }
}
