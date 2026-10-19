use std::sync::Mutex;

use infinity_ipc::{IpcError, IpcErrorCode, IpcResult};

pub trait TokenStore: Send + Sync + std::fmt::Debug {
    fn load(&self) -> IpcResult<Option<String>>;
    fn save(&self, refresh_token: &str) -> IpcResult<()>;
    fn clear(&self) -> IpcResult<()>;
}

#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
    account: String,
}

impl KeyringStore {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> IpcResult<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account).map_err(keyring_err)
    }
}

impl TokenStore for KeyringStore {
    fn load(&self) -> IpcResult<Option<String>> {
        match self.entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_err(e)),
        }
    }

    fn save(&self, refresh_token: &str) -> IpcResult<()> {
        self.entry()?
            .set_password(refresh_token)
            .map_err(keyring_err)
    }

    fn clear(&self) -> IpcResult<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_err(e)),
        }
    }
}

fn keyring_err(e: keyring::Error) -> IpcError {
    IpcError::new(IpcErrorCode::IoError, format!("credential store: {e}"))
}

/// store for test session or platforms that wont support keyring access
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl TokenStore for MemoryStore {
    fn load(&self) -> IpcResult<Option<String>> {
        Ok(self.0.lock().map_err(poisoned)?.clone())
    }

    fn save(&self, refresh_token: &str) -> IpcResult<()> {
        *self.0.lock().map_err(poisoned)? = Some(refresh_token.to_owned());
        Ok(())
    }

    fn clear(&self) -> IpcResult<()> {
        *self.0.lock().map_err(poisoned)? = None;
        Ok(())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> IpcError {
    IpcError::internal("token store lock poisoned")
}
