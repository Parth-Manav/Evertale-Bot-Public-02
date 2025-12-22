use futures_util::{SinkExt, StreamExt, stream::{SplitSink, SplitStream}};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use regex::Regex;

use crate::db::Account; // Import Account struct

const BASE_URL: &str = "wss://evertext.sytes.net/socket.io/?EIO=4&transport=websocket";

#[allow(dead_code)]
pub struct EvertextClient {
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    ping_interval: u64,
    history: String,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
enum GameState {
    Connected,
    WaitingForCommandPrompt,
    SentD,
    WaitingForRestorePrompt,
    SentCode,
    WaitingForServerList,
    ServerSelected,
    WaitingProcedure,
    RapidFire,
    Finished,
}

impl EvertextClient {
    pub async fn connect(cookie: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut request = BASE_URL.into_client_request()?;
        let headers = request.headers_mut();
        let cookie_header = format!("session={}", cookie);
        headers.insert("Cookie", HeaderValue::from_str(&cookie_header)?);
        headers.insert("User-Agent", HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"));

        println!("[INFO] Connecting to EverText WebSocket...");
        let (mut ws_stream, _) = connect_async(request).await?;

        // 1. Wait for "Open" packet (Type 0)
        let msg = ws_stream.next().await.ok_or("Stream closed")??;
        let msg_str = msg.to_string();
        
        if msg_str.starts_with('0') {
            let json_part = &msg_str[1..];
            let data: serde_json::Value = serde_json::from_str(json_part)?;
            
            let sid = data["sid"].as_str().ok_or("No SID found")?.to_string();
            let ping = data["pingInterval"].as_u64().unwrap_or(25000);
            
            println!("[INFO] Connected! Session ID: {}", sid);
            
            // 2. Send "40" to upgrade namespace
            ws_stream.send(Message::Text("40".into())).await?;
            
            let (write, read) = ws_stream.split();

            return Ok(Self {
                write,
                read,
                ping_interval: ping,
                history: String::new(),
            });
        }

        Err("Failed to handshake".into())
    }

    pub async fn run_loop(&mut self, account: &Account, decrypted_code: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut last_ping = Instant::now();
        let mut state = GameState::Connected;
        let mut waiting_started: Option<Instant> = None;

        println!("[INFO][PID:{}] Starting session for account: {}", std::process::id(), account.name);

        loop {
            // Heartbeat Logic
            if last_ping.elapsed().as_millis() as u64 > self.ping_interval {
                // Ping handled by incoming '2' usually, but we can send '3' periodically if needed.
                // The server usually initiates via '2'.
            }

            // Timeout Logic for states
             if let Some(start_time) = waiting_started {
                if state == GameState::WaitingProcedure {
                     if start_time.elapsed().as_secs() >= 200 { // 3:20 minutes
                        println!("[INFO] 200s Wait Complete. Starting Rapid Fire.");
                        #[allow(unused_assignments)]
                        {
                            state = GameState::RapidFire;
                            waiting_started = None;
                        }
                        
                        // Execute Rapid Fire
                        let commands = ["y", "auto", "exit", "exit", "exit", "exit"];
                        for cmd in commands {
                            println!("[ACTION] Sending '{}'", cmd);
                            self.send_command(cmd).await?;
                             tokio::time::sleep(Duration::from_millis(500)).await;
                        }

                         println!("[INFO] Rapid Fire done. Waiting 120s...");
                         tokio::time::sleep(Duration::from_secs(120)).await;
                         println!("[INFO] Session Complete.");
                         return Ok(());
                     }
                }
             }

            tokio::select! {
                msg = self.read.next() => {
                    match msg {
                        Some(Ok(m)) => {
                            let text = m.to_string();
                            println!("[DEBUG] Received: {}", text);
                            if text == "2" {
                                self.write.send(Message::Text("3".into())).await?;
                                last_ping = Instant::now();
                            } else if text.starts_with("40") {
                                // Namespace join acknowledged
                                println!("[INFO] Namespace joined. Initializing session...");
                                
                                // Send 'stop' first to ensure it's not already running
                                println!("[ACTION] Sending 'stop' event...");
                                let stop_payload = json!(["stop", {}]); // Assuming empty object based on subagent
                                self.write.send(Message::Text(format!("42{}", stop_payload.to_string()).into())).await?;
                                
                                tokio::time::sleep(Duration::from_millis(500)).await;

                                println!("[ACTION] Sending 'start' event...");
                                let start_payload = json!(["start", {"args": ""}]);
                                self.write.send(Message::Text(format!("42{}", start_payload.to_string()).into())).await?;
                            } else if text.starts_with("42") {
                                self.handle_event(&text, &mut state, account, decrypted_code, &mut waiting_started).await?;
                            }
                        }
                        Some(Err(e)) => return Err(e.into()),
                        None => return Err("Socket closed".into()),
                    }
                }
            }
        }
    }

    async fn send_command(&mut self, cmd: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
         // Payload structure found via browser sniffing: { input: "cmd" }
         let payload = json!(["input", {"input": cmd}]); 
         let packet = format!("42{}", payload.to_string());
         self.write.send(Message::Text(packet.into())).await?;
         Ok(())
    }

    async fn handle_event(&mut self, text: &str, state: &mut GameState, account: &Account, code: &str, wait_timer: &mut Option<Instant>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let json_part = &text[2..];
        let event: serde_json::Value = serde_json::from_str(json_part)?;
        
        if let Some(event_array) = event.as_array() {
            let event_name = event_array[0].as_str().unwrap_or("");
            let event_data = event_array.get(1);

            if event_name == "output" {
                 if let Some(data) = event_data {
                     if let Some(output_text) = data["data"].as_str() {
                         println!("[TERMINAL] {}", output_text.replace("\n", " ").chars().take(100).collect::<String>());
                         
                         // Update history for multi-line parsing
                         self.history.push_str(output_text);
                         if self.history.len() > 10000 {
                             let drain_len = self.history.len() - 10000;
                             self.history.replace_range(..drain_len, "");
                         }

                         // 1. Initial login prompts
                         if output_text.contains("Enter Command to use") {
                             println!("[DEBUG] State check for 'd': {:?}", state);
                             if *state == GameState::Connected || *state == GameState::WaitingProcedure {
                                 println!("[PID:{}] [From TERMINAL] Enter Command to use. Sending 'd'...", std::process::id());
                                 *state = GameState::SentD;
                                 self.send_command("d").await?;
                             }
                         }
                         
                         if output_text.contains("Enter Restore code") {
                             println!("[DEBUG] State check for code: {:?}", state);
                             if *state == GameState::SentD || *state == GameState::Connected || *state == GameState::WaitingProcedure {
                                 println!("[PID:{}] [From TERMINAL] Enter Restore code. Sending Code...", std::process::id());
                                 *state = GameState::SentCode;
                                 self.send_command(code).await?;
                             }
                         }

                         // 2. Server selection
                         if output_text.contains("Which acc u want to Login") {
                             if *state == GameState::SentCode || *state == GameState::Connected || *state == GameState::WaitingProcedure {
                                 println!("[From TERMINAL] Server Selection List Received. Parsing...");
                                 
                                 let mut selected_index = "1".to_string();
                                 if let Some(target) = &account.target_server {
                                     // Regex to find: "1--> [account_name] (E-15)"
                                     let re = Regex::new(r"(\d+)-->.*?\((.*?)\)").unwrap();
                                     let mut found = false;
                                     
                                     for cap in re.captures_iter(&self.history) {
                                         let index = &cap[1];
                                         let server_name = &cap[2];
                                         
                                         // Match "E-21" or "All of them" (mapped from "All")
                                         if server_name.contains(target) || 
                                            (target.to_lowercase() == "all" && server_name.contains("All of them")) {
                                             println!("[INFO] Found match for target '{}': Server '{}' at index {}", target, server_name, index);
                                             selected_index = index.to_string();
                                             found = true;
                                             break;
                                         }
                                     }
                                     
                                     if !found {
                                         println!("[WARN] Target server '{}' not found in list. Defaulting to '1'.", target);
                                     }
                                 } else {
                                     println!("[INFO] No targetServer specified. Defaulting to '1'.");
                                 }

                                 println!("[ACTION] Selecting server index: {}", selected_index);
                                 self.send_command(&selected_index).await?;
                                 *state = GameState::ServerSelected;
                             }
                         }

                         // 3. Success / Resumed session detection
                         if output_text.contains("Performing Dailies") || 
                            output_text.contains("Press y") {
                             
                             if *state != GameState::WaitingProcedure && *state != GameState::RapidFire && *state != GameState::Finished {
                                 println!("[INFO] Session active/resumed. Starting 200s wait.");
                                 *state = GameState::WaitingProcedure;
                                 *wait_timer = Some(Instant::now());
                             }
                         }
                         
                         // 4. Fallback for immediate wait transition
                         if *state == GameState::SentCode && account.target_server.is_none() {
                             // If no server selection needed, we can jump to wait if some output happens
                             // but it's safer to wait for a specific prompt like above. 
                             // However, automation.js just waits after the command.
                             // Let's keep it but prioritize the prompt detection.
                         }

                         // 5. Error handling
                         if output_text.contains("Either Zigza error or Incorrect Restore Code Entered") {
                             println!("[ERROR] Zigza Error Detected!");
                             return Err("ZIGZA_DETECTED".into());
                         }

                         if output_text.contains("Server reached maximum limit of restore accounts") {
                             println!("[ERROR] Server Full Detected!");
                             return Err("SERVER_FULL".into());
                         }

                         if output_text.contains("Access to start bot is restricted only for logged in users") {
                             println!("[ERROR] Login Required / Cookie Expired!");
                             return Err("LOGIN_REQUIRED".into());
                         }
                     }
                 }
            }
        }
        Ok(())
    }
}
