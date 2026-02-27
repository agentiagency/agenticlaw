//! Authentication handling

use agenticlaw_core::{AuthConfig, AuthMode, Error, Result};
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Clone, Debug)]
pub struct ResolvedAuth {
    pub mode: AuthMode,
    pub token: Option<String>,
}

impl ResolvedAuth {
    pub fn from_config(config: &AuthConfig, env_token: Option<String>) -> Self {
        let token = config.token.clone().or(env_token);
        Self { mode: config.mode.clone(), token }
    }

    pub fn verify_token(&self, provided: Option<&str>) -> Result<()> {
        match self.mode {
            AuthMode::None => Ok(()),
            AuthMode::Token => {
                let expected = self.token.as_deref().ok_or_else(|| Error::auth_failed("no token configured"))?;
                let provided = provided.ok_or_else(|| Error::auth_failed("token required"))?;
                if !constant_time_eq(expected.as_bytes(), provided.as_bytes()) {
                    return Err(Error::auth_failed("invalid token"));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_auth() {
        let auth = ResolvedAuth { mode: AuthMode::Token, token: Some("test-token-123".into()) };
        assert!(auth.verify_token(Some("test-token-123")).is_ok());
        assert!(auth.verify_token(Some("wrong-token")).is_err());
        assert!(auth.verify_token(None).is_err());
    }

    #[test]
    fn test_no_auth() {
        let auth = ResolvedAuth { mode: AuthMode::None, token: None };
        assert!(auth.verify_token(None).is_ok());
        assert!(auth.verify_token(Some("anything")).is_ok());
    }
}
