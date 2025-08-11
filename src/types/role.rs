use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

impl From<&str> for Role {
    fn from(s: &str) -> Self {
        match s {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User, // Default to User for unknown values
        }
    }
}

impl From<Option<String>> for Role {
    fn from(s: Option<String>) -> Self {
        match s.as_deref() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("system") => Role::System,
            _ => Role::User, // Default to User
        }
    }
}

impl From<String> for Role {
    fn from(s: String) -> Self {
        Role::from(s.as_str())
    }
}

// Implement ToString for sqlx compatibility
impl ToString for Role {
    fn to_string(&self) -> String {
        self.as_str().to_string()
    }
}

// Implement FromStr for parsing Role from strings
impl FromStr for Role {
    type Err = anyhow::Error;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),  
            "system" => Ok(Role::System),
            other => {
                // Log warning for unknown role, default to User
                eprintln!("Warning: Unknown role '{}', defaulting to 'user'", other);
                Ok(Role::User)
            }
        }
    }
}