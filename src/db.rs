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
        
        let content = match fs::read_to_string(&path) {
            Ok(c) => {
                println!("[INFO] Loading database from: {}", path);
                c
            },
            Err(e) if path == "db.json" => {
                let fallback = "app/db.json";
                match fs::read_to_string(fallback) {
                    Ok(c) => {
                        println!("[INFO] Loading database from fallback: {}", fallback);
                        c
                    },
                    Err(_) => return Err(e.into()),
                }
            },
            Err(e) => return Err(e.into()),
        };

        let data: DbData = serde_json::from_str(&content)?;
        Ok(Self { data })
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "db.json".to_string());
        let content = serde_json::to_string_pretty(&self.data)?;
        
        if let Err(e) = fs::write(&path, content.clone()) {
            if path == "db.json" {
                let fallback = "app/db.json";
                fs::write(fallback, content)?;
            } else {
                return Err(e.into());
            }
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
