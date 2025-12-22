use serde::{Deserialize, Serialize};
use std::fs;
use chrono;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Account {
    pub name: String,
    pub code: String,
    #[serde(rename = "targetServer")]
    pub target_server: Option<String>,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub username: Option<String>,
    #[serde(rename = "pingEnabled")]
    pub ping_enabled: bool,
    pub status: String,
    #[serde(rename = "lastRun")]
    pub last_run: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(rename = "cookies")]
    pub cookies: Option<String>,
    #[serde(rename = "adminRoleId")]
    pub admin_role_id: Option<String>,
    #[serde(rename = "logChannelId")]
    pub log_channel_id: Option<String>,
    #[serde(rename = "muteBotMessages")]
    pub mute_bot_messages: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DbData {
    pub accounts: Vec<Account>,
    pub settings: Settings,
}

pub struct Database {
    pub data: DbData,
}

impl Database {
    pub fn load() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "db.json".to_string());
        
        // --- Diagnostics ---
        if let Ok(cwd) = std::env::current_dir() {
            println!("[DEBUG] Current working directory: {:?}", cwd);
        }
        for dir in [".", "/app", "/"] {
            if let Ok(entries) = fs::read_dir(dir) {
                let files: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.file_name().into_string().unwrap_or_default())).collect();
                println!("[DEBUG] Files in '{}': {:?}", dir, files);
            }
        }
        // --- End Diagnostics ---

        let content = match fs::read_to_string(&path) {
            Ok(c) => {
                println!("[INFO] Loading database from file: {}", path);
                c
            },
            Err(e) => {
                println!("[WARN] Could not find database at {}. Searching fallbacks...", path);
                // Try several fallback locations
                let fallbacks = [
                    "db.json", 
                    "./db.json", 
                    "/app/db.json", 
                    "app/db.json", 
                    "../db.json"
                ];
                let mut found_content = None;
                
                for fb in fallbacks {
                    if let Ok(c) = fs::read_to_string(fb) {
                        println!("[INFO] Found database at fallback: {}", fb);
                        found_content = Some(c);
                        break;
                    }
                }
                
                match found_content {
                    Some(c) => {
                        println!("[INFO] Using database from fallback file.");
                        c
                    },
                    None => {
                        println!("[WARN] No database file found on disk. Using EMBEDDED database fallback.");
                        // Fallback to embedded content so the bot doesn't crash
                        include_str!("../db.json").to_string()
                    }
                }
            }
        };

        match serde_json::from_str::<DbData>(&content) {
            Ok(data) => Ok(Self { data }),
            Err(e) => {
                println!("[ERROR] Failed to parse database JSON: {}", e);
                // If parsing fails, we might as well return the error, 
                // but at least we tried every path.
                Err(e.into())
            }
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "db.json".to_string());
        let content = serde_json::to_string_pretty(&self.data)?;
        
        // Try to save to multiple locations to ensure persistence if possible
        let paths = [path.as_str(), "db.json", "/app/db.json"];
        let mut saved = false;

        for p in paths {
            if let Err(e) = fs::write(p, content.clone()) {
                println!("[WARN] Failed to save database to {}: {}", p, e);
            } else {
                println!("[INFO] Successfully saved database to {}", p);
                saved = true;
                // We only need to save to one location successfully
                break; // Added break here to stop trying once saved
            }
        }

        if !saved {
            println!("[ERROR] Failed to save database to ANY location!");
            return Err("Failed to save database to any location".into());
        }
        Ok(())
    }

    pub fn update_status(&mut self, name: &str, status: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(acc) = self.data.accounts.iter_mut().find(|a| a.name == name) {
            acc.status = status.to_string();
            acc.last_run = Some(chrono::Utc::now().to_rfc3339());
            self.save()?;
        }
        Ok(())
    }

    pub fn add_account(&mut self, account: Account) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.data.accounts.retain(|a| a.name != account.name);
        self.data.accounts.push(account);
        self.save()
    }

    pub fn remove_account(&mut self, name: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let len_before = self.data.accounts.len();
        self.data.accounts.retain(|a| a.name != name);
        let found = self.data.accounts.len() < len_before;
        if found {
            self.save()?;
        }
        Ok(found)
    }

    pub fn reset_all_statuses(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for acc in self.data.accounts.iter_mut() {
            acc.status = "pending".to_string();
        }
        self.save()
    }

    pub fn toggle_ping(&mut self, user_id: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let mut new_state = false;
        let mut first = true;
        let accounts: Vec<_> = self.data.accounts.iter_mut()
            .filter(|a| a.user_id.as_deref() == Some(user_id))
            .collect();
        
        if accounts.is_empty() {
             return Err("No accounts found for this user.".into());
        }

        for acc in accounts {
            if first {
                acc.ping_enabled = !acc.ping_enabled;
                new_state = acc.ping_enabled;
                first = false;
            } else {
                acc.ping_enabled = new_state;
            }
        }
        self.save()?;
        Ok(new_state)
    }

    pub fn set_mute(&mut self, mute: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.data.settings.mute_bot_messages = Some(mute);
        self.save()
    }

    pub fn set_log_channel(&mut self, channel_id: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.data.settings.log_channel_id = Some(channel_id);
        self.save()
    }

    pub fn set_admin_role(&mut self, role_id: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.data.settings.admin_role_id = Some(role_id);
        self.save()
    }

    pub fn get_user_accounts(&self, user_id: &str) -> Vec<Account> {
        self.data.accounts.iter()
            .filter(|a| a.user_id.as_deref() == Some(user_id))
            .cloned()
            .collect()
    }
}
