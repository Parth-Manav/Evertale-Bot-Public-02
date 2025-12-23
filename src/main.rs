mod protocol;
mod db;

use protocol::socket::EvertextClient;
use db::{Database, Account};

use std::sync::Arc;
use tokio::sync::Mutex;
use serenity::all::*;
use serenity::async_trait;
use chrono::{Utc, Timelike};
use chrono_tz::Asia::Jakarta;

struct Handler {
    db: Arc<Mutex<Database>>,
    is_processing: Arc<Mutex<bool>>,
}

impl Handler {
    async fn is_admin(&self, ctx: &Context, interaction: &CommandInteraction) -> bool {
        let db = self.db.lock().await;
        if let Some(role_id_str) = &db.data.settings.admin_role_id {
            if let Ok(role_id) = role_id_str.parse::<u64>() {
                if let Some(member) = &interaction.member {
                    return member.roles.contains(&RoleId::new(role_id));
                }
            }
        }
        // Fallback to guild owner if no role set or failed to check
        if let Some(guild_id) = interaction.guild_id {
            if let Ok(guild) = guild_id.to_partial_guild(&ctx.http).await {
                return interaction.user.id == guild.owner_id;
            }
        }
        false
    }

    async fn log_message(db: Arc<Mutex<Database>>, http: Arc<Http>, message: String, skip_channel: Option<ChannelId>) {
        let db = db.lock().await;
        if let Some(true) = db.data.settings.mute_bot_messages {
            return;
        }
        if let Some(channel_id_str) = &db.data.settings.log_channel_id {
            if let Ok(channel_id) = channel_id_str.parse::<u64>() {
                let channel = ChannelId::new(channel_id);
                if Some(channel) == skip_channel {
                    return;
                }
                let _ = channel.say(&http, message).await;
            }
        }
    }

    async fn process_queue(&self, ctx: Context, user_id_filter: Option<String>, source_channel: Option<ChannelId>) {
        let db_clone = Arc::clone(&self.db);
        let processing_clone = Arc::clone(&self.is_processing);
        let http_clone = ctx.http.clone();

        tokio::spawn(async move {
            let already_running = {
                let mut is_proc = processing_clone.lock().await;
                if *is_proc {
                    true
                } else {
                    *is_proc = true;
                    false
                }
            };

            if already_running {
                if let Some(chan) = source_channel {
                    let _ = chan.say(&http_clone, "[WARN] Queue Manager: Already in progress.").await;
                }
                return;
            }

            if let Some(chan) = source_channel {
                    let _ = chan.say(&http_clone, "[INFO] Queue Manager: Starting automation sequence...").await;
            }

            loop {
                // Check if we were told to stop
                {
                    let is_proc = processing_clone.lock().await;
                    if !*is_proc { break; }
                }

                let next_account = {
                    let db = db_clone.lock().await;
                    let mut accs: Vec<Account> = db.data.accounts.iter()
                        .filter(|a| a.status != "done")
                        .cloned()
                        .collect();
                    
                    if let Some(uid) = &user_id_filter {
                        accs.retain(|a| a.user_id.as_deref() == Some(uid));
                    }
                    
                    // Explicitly prioritize:
                    // 1. Pending accounts (in insertion order)
                    // 2. Error/Retrying accounts (in insertion order)
                    let (mut pending, errors): (Vec<Account>, Vec<Account>) = accs.into_iter()
                        .partition(|a| !a.status.starts_with("error"));
                    
                    pending.extend(errors);
                    pending.into_iter().next()
                };

                let acc = match next_account {
                    Some(a) => a,
                    None => break,
                };
                
                let cookie = {
                    let db = db_clone.lock().await;
                    db.data.settings.cookies.clone().unwrap_or_default()
                };

                if cookie.is_empty() {
                     break;
                }

                match EvertextClient::connect(&cookie).await {
                    Ok(mut client) => {
                        match client.run_loop(&acc, &acc.code).await {
                             Ok(_) => {
                                {
                                    let mut db = db_clone.lock().await;
                                    let _ = db.update_status(&acc.name, "done");
                                }
                                if let Some(chan) = source_channel {
                                    let _ = chan.say(&http_clone, format!("[SUCCESS] **{}** completed.", acc.name)).await;
                                }
                                Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), format!("[SUCCESS] Automation: **{}** completed successfully.", acc.name), source_channel).await;
                            },
                            Err(e) => {
                                let err_str = e.to_string();
                                
                                if err_str.contains("SESSION_COMPLETE") {
                                    {
                                        let mut db = db_clone.lock().await;
                                        let _ = db.update_status(&acc.name, "done");
                                    }
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, format!("[SUCCESS] **{}** completed.", acc.name)).await;
                                    }
                                    Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), format!("[SUCCESS] Automation: **{}** completed through prompt flow.", acc.name), source_channel).await;

                                } else if err_str.contains("INVALID_COMMAND_RESTART") {
                                    if let Some(chan) = source_channel {
                                         let _ = chan.say(&http_clone, format!("[WARN] Invalid Command on **{}**. Restarting session immediately.", acc.name)).await;
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                                } else if err_str.contains("ZIGZA_DETECTED") {
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, format!("[WARN] Zigza error on **{}**. Waiting 10 mins before retry.", acc.name)).await;
                                    }
                                    Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), format!("[WARN] Automation: Zigza detected on **{}**. Retrying in 10m.", acc.name), source_channel).await;
                                    {
                                        let mut db = db_clone.lock().await;
                                        let _ = db.update_status(&acc.name, "error: Zigza Retrying");
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;

                                } else if err_str.contains("SERVER_FULL") {
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, format!("[WARN] Server Full. Retrying **{}** in 5 mins.", acc.name)).await;
                                    }
                                    Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), format!("[WARN] Automation: Server full. Retrying **{}** in 5m.", acc.name), source_channel).await;
                                    tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;

                                } else if err_str.contains("LOGIN_REQUIRED") {
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, "⚠️ **CRITICAL: Session cookie expired!** Stopping queue.").await;
                                    }
                                    Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), "⚠️ **[CRITICAL] Automation: Session cookie expired!** Stopping queue.".to_string(), source_channel).await;
                                    break;

                                } else if err_str.contains("IDLE_TIMEOUT") || err_str.contains("CONNECTION_FAILED") || err_str.contains("SERVER_DISCONNECT") || err_str.contains("Connection handshake timed out") {
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, format!("[WARN] Connection issue on **{}** (Reason: {}). Retrying in 5s...", acc.name, err_str)).await;
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                                } else {
                                    {
                                        let mut db = db_clone.lock().await;
                                        let _ = db.update_status(&acc.name, &format!("error: {}", err_str));
                                    }
                                    if let Some(chan) = source_channel {
                                        let _ = chan.say(&http_clone, format!("[ERROR] **{}** failed: {}", acc.name, err_str)).await;
                                    }
                                    Self::log_message(Arc::clone(&db_clone), Arc::clone(&http_clone), format!("[ERROR] Automation: **{}** failed. Reason: {}", acc.name, err_str), source_channel).await;
                                }
                            }
                        }
                    },
                    Err(e) => {
                        if let Some(chan) = source_channel {
                            let _ = chan.say(&http_clone, format!("[ERROR] Connection failed for **{}**: {}", acc.name, e)).await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
                // Small delay to prevent tight loops in edge cases
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }

            {
                let mut is_proc = processing_clone.lock().await;
                *is_proc = false;
            }
            if let Some(chan) = source_channel {
                let _ = chan.say(&http_clone, "[INFO] Queue Manager: Processing finished.").await;
            }
        });
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("[INFO] Discord: Bot successfully logged in as {}", ready.user.name);

        let _ = Command::set_global_commands(&ctx.http, vec![
            CreateCommand::new("add_account")
                .description("Add a new game account")
                .add_option(CreateCommandOption::new(CommandOptionType::String, "name", "Account Name").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::String, "code", "Restore Code").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::Boolean, "toggle_server_selection", "Enable server selection?").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::String, "server", "Target server (e.g., E-15, All)").required(false)),
            CreateCommand::new("remove_account")
                .description("Remove a game account")
                .add_option(CreateCommandOption::new(CommandOptionType::String, "name", "Account Name").required(true)),
            CreateCommand::new("list_accounts")
                .description("List all configured accounts"),
            CreateCommand::new("list_my_accounts")
                .description("List only your accounts"),
            CreateCommand::new("toggle_ping")
                .description("Toggle ping notifications for your accounts"),
            CreateCommand::new("force_run")
                .description("Force run automation. Use 'all' to run all your accounts.")
                .add_option(CreateCommandOption::new(CommandOptionType::String, "name", "Account Name or 'all'").required(false)),
            CreateCommand::new("force_run_all")
                .description("[ADMIN] Run all accounts in the system"),
            CreateCommand::new("force_stop_all")
                .description("[ADMIN] Stop all running processes"),
            CreateCommand::new("mute_bot")
                .description("[ADMIN] Mute automatic bot messages"),
            CreateCommand::new("unmute_bot")
                .description("[ADMIN] Unmute automatic bot messages"),
            CreateCommand::new("set_log_channel")
                .description("[ADMIN] Set channel for automatic messages")
                .add_option(CreateCommandOption::new(CommandOptionType::Channel, "channel", "Log Channel").required(true)),
            CreateCommand::new("set_admin_role")
                .description("[ADMIN] Set admin role for bot management")
                .add_option(CreateCommandOption::new(CommandOptionType::Role, "role", "Admin Role").required(true)),
            CreateCommand::new("set_cookies")
                .description("[ADMIN] Set session cookie to bypass login")
                .add_option(CreateCommandOption::new(CommandOptionType::String, "cookie", "The 'session' cookie value").required(true)),
        ]).await;

        println!("[INFO] Discord: Slash commands registered successfully");

        // Start Scheduler
        let db_clone = Arc::clone(&self.db);
        let ctx_clone = ctx.clone();
        let is_processing_clone = Arc::clone(&self.is_processing);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = Utc::now().with_timezone(&Jakarta);
                if now.hour() == 0 && now.minute() == 0 {
                    println!("[INFO] Scheduler: Daily reset triggered at {}", now);
                    {
                        let mut db = db_clone.lock().await;
                        let _ = db.reset_all_statuses();
                    }
                    
                    // Trigger queue for all accounts
                     let db_c = Arc::clone(&db_clone);
                     let proc_c = Arc::clone(&is_processing_clone);
                     let ctx_c = ctx_clone.clone();

                     tokio::spawn(async move {
                         let h = Handler { db: db_c, is_processing: proc_c };
                         h.process_queue(ctx_c, None, None).await;
                     });
                }
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let user_id = command.user.id.to_string();
            let mut content = "Processing...".to_string();

            match command.data.name.as_str() {
                "list_accounts" => {
                    let db = self.db.lock().await;
                    content = if db.data.accounts.is_empty() {
                        "No accounts registered.".to_string()
                    } else {
                        db.data.accounts.iter()
                            .map(|a| format!("- **{}**: {} (Last Run: {})", a.name, a.status, a.last_run.as_deref().unwrap_or("Never")))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                },
                "list_my_accounts" => {
                    let db = self.db.lock().await;
                    let my_accs = db.get_user_accounts(&user_id);
                    content = if my_accs.is_empty() {
                        "You have no accounts registered.".to_string()
                    } else {
                        my_accs.iter()
                            .map(|a| format!("- **{}**: {} (Last Run: {})", a.name, a.status, a.last_run.as_deref().unwrap_or("Never")))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                },
                "add_account" => {
                    let name = command.data.options.iter().find(|o| o.name == "name").and_then(|o| o.value.as_str()).unwrap_or("").to_string();
                    let code = command.data.options.iter().find(|o| o.name == "code").and_then(|o| o.value.as_str()).unwrap_or("").to_string();
                    let server = command.data.options.iter().find(|o| o.name == "server").and_then(|o| o.value.as_str()).map(|s| s.to_string());
                    
                    {
                        let mut db = self.db.lock().await;
                        let new_acc = Account {
                            name: name.clone(),
                            code: code,
                            target_server: server,
                            user_id: Some(user_id.clone()),
                            username: Some(command.user.name.clone()),
                            discord_nickname: command.member.as_ref().and_then(|m| m.nick.clone()),
                            ping_enabled: false,
                            status: "pending".to_string(),
                            last_run: None,
                        };
                        let _ = db.add_account(new_acc);
                    }
                    content = format!("Successfully added account **{}**.", name);
                    self.process_queue(ctx.clone(), Some(user_id), Some(command.channel_id)).await;
                },
                "remove_account" => {
                    let mut db = self.db.lock().await;
                    let name = command.data.options.iter().find(|o| o.name == "name").and_then(|o| o.value.as_str()).unwrap_or("");
                    match db.remove_account(name) {
                        Ok(true) => content = format!("Successfully removed account **{}**.", name),
                        _ => content = format!("Account **{}** not found.", name),
                    }
                },
                "toggle_ping" => {
                    let mut db = self.db.lock().await;
                    match db.toggle_ping(&user_id) {
                        Ok(state) => content = format!("Pings now **{}** for all your accounts.", if state { "enabled" } else { "disabled" }),
                        Err(e) => content = format!("Error: {}", e),
                    }
                },
                "force_run" => {
                    let name = command.data.options.iter().find(|o| o.name == "name").and_then(|o| o.value.as_str());
                    
                    let target_name = name.unwrap_or("all");
                    
                    if target_name.to_lowercase() == "all" {
                        // Run all for THIS user
                        self.process_queue(ctx.clone(), Some(user_id), Some(command.channel_id)).await;
                        content = "Queued all your accounts for execution.".to_string();
                    } else {
                        // Start single
                        let db_clone = Arc::clone(&self.db);
                        let processing_clone = Arc::clone(&self.is_processing);
                        let http_clone = ctx.http.clone();
                        let channel_id = command.channel_id;
                        let n_owned = target_name.to_string();
                        
                         tokio::spawn(async move {
                            let (cookie, acc) = {
                                let mut is_proc = processing_clone.lock().await;
                                if *is_proc {
                                    let _ = channel_id.say(&http_clone, "[WARN] Already in progress.").await;
                                    return;
                                }
                                *is_proc = true;
                                
                                let db = db_clone.lock().await;
                                (db.data.settings.cookies.clone().unwrap_or_default(), 
                                 db.data.accounts.iter().find(|a| a.name == n_owned).cloned())
                            };
                            
                            if let Some(acc) = acc {
                                if cookie.is_empty() {
                                    let _ = channel_id.say(&http_clone, "[ERROR] No cookies set.").await;
                                } else {
                                    let _ = channel_id.say(&http_clone, format!("[INFO] Force running **{}**...", acc.name)).await;
                                    match EvertextClient::connect(&cookie).await {
                                        Ok(mut client) => {
                                            match client.run_loop(&acc, &acc.code).await {
                                                Ok(_) => {
                                                    let mut db = db_clone.lock().await;
                                                    let _ = db.update_status(&acc.name, "done");
                                                    let _ = channel_id.say(&http_clone, format!("[SUCCESS] **{}** finished.", acc.name)).await;
                                                },
                                                Err(e) => {
                                                    let err_str = e.to_string();
                                                    if err_str.contains("SESSION_COMPLETE") {
                                                        let mut db = db_clone.lock().await;
                                                        let _ = db.update_status(&acc.name, "done");
                                                        let _ = channel_id.say(&http_clone, format!("[SUCCESS] **{}** finished.", acc.name)).await;
                                                    } else {
                                                        let _ = channel_id.say(&http_clone, format!("[ERROR] **{}** failed: {}", acc.name, err_str)).await;
                                                    }
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            let _ = channel_id.say(&http_clone, format!("[ERROR] Connection failed: {}", e)).await;
                                        }
                                    }
                                }
                            } else {
                                let _ = channel_id.say(&http_clone, format!("[ERROR] Account **{}** not found.", n_owned)).await;
                            }
                            
                            let mut is_proc = processing_clone.lock().await;
                            *is_proc = false;
                        });
                        content = format!("Force run initiated for **{}**.", target_name);
                    }
                },
                "force_run_all" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        self.process_queue(ctx.clone(), None, Some(command.channel_id)).await;
                        content = "Starting ALL pending accounts...".to_string();
                    }
                },
                "force_stop_all" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        let mut is_proc = self.is_processing.lock().await;
                        *is_proc = false;
                        content = "Queue processing halted.".to_string();
                    }
                },
                "mute_bot" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        let mut db = self.db.lock().await;
                        let _ = db.set_mute(true);
                        content = "Bot messages muted.".to_string();
                    }
                },
                "unmute_bot" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        let mut db = self.db.lock().await;
                        let _ = db.set_mute(false);
                        content = "Bot messages unmuted.".to_string();
                    }
                },
                "set_log_channel" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        let channel = command.data.options.iter().find(|o| o.name == "channel").and_then(|o| o.value.as_channel_id());
                        if let Some(chan) = channel {
                            let mut db = self.db.lock().await;
                            let _ = db.set_log_channel(chan.to_string());
                            content = format!("Log channel set to <#{}>.", chan);
                        }
                    }
                },
                "set_admin_role" => {
                    // Check if owner
                    let is_owner = if let Some(guild_id) = command.guild_id {
                        if let Ok(guild) = guild_id.to_partial_guild(&ctx.http).await {
                            command.user.id == guild.owner_id
                        } else { false }
                    } else { false };

                    if !is_owner {
                        content = "Only the server owner can set the admin role.".to_string();
                    } else {
                        let role = command.data.options.iter().find(|o| o.name == "role").and_then(|o| o.value.as_role_id());
                        if let Some(r) = role {
                            let mut db = self.db.lock().await;
                            let _ = db.set_admin_role(r.to_string());
                            content = format!("Admin role set to <@&{}>.", r);
                        }
                    }
                },
                "set_cookies" => {
                    if !self.is_admin(&ctx, &command).await {
                        content = "Admin permissions required.".to_string();
                    } else {
                        let mut db = self.db.lock().await;
                        if let Some(option) = command.data.options.iter().find(|o| o.name == "cookie") {
                            if let Some(cookie) = option.value.as_str() {
                                db.data.settings.cookies = Some(cookie.to_string());
                                let _ = db.save();
                                content = "Session cookies updated.".to_string();
                            }
                        }
                    }
                },
                _ => content = "Unknown command.".to_string(),
            }

            let _ = command.create_response(&ctx.http, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content(content)
            )).await;
        }
    }
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    env_logger::init();
    
    let token = std::env::var("DISCORD_TOKEN").expect("Expected a DISCORD_TOKEN in the environment");
    let database_res = Database::load();
    let database = match database_res {
        Ok(db) => Arc::new(Mutex::new(db)),
        Err(e) => {
            println!("[CRITICAL] Failed to load database: {}. Bot may not function correctly.", e);
            // We still need a database object to continue, so we'll try to create a dummy one if possible
            // or just exit gracefully instead of panicking.
            return; 
        }
    };
    
    let handler = Handler {
        db: database,
        is_processing: Arc::new(Mutex::new(false)),
    };

    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::DIRECT_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

    println!("[INFO] Starting EverText Rust Bot...");
    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .expect("Err creating client");

    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
