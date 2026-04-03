use rand::Rng;
use std::sync::Arc;
use tokio::sync::RwLock;

const TOKEN_LENGTH: usize = 32;

#[derive(Clone)]
pub struct AuthState {
    pub valid_token: String,
}

impl AuthState {
    pub fn new() -> Self {
        let token = generate_token();
        Self { valid_token: token }
    }

    pub fn validate(&self, token: &str) -> bool {
        self.valid_token == token
    }

    pub fn token_url(&self, port: u16) -> String {
        format!("http://localhost:{}/?token={}", port, self.valid_token)
    }
}

fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..TOKEN_LENGTH).map(|_| rng.gen::<u8>()).collect();
    hex::encode(bytes)
}

pub async fn extract_and_validate(
    auth_state: Arc<RwLock<AuthState>>,
    token_opt: Option<String>,
) -> Option<String> {
    let token = token_opt?;
    let auth = auth_state.read().await;
    if auth.validate(&token) {
        Some(token)
    } else {
        None
    }
}
