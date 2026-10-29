use crate::error::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn install() -> Result<()> {
    ctrlc::set_handler(|| REQUESTED.store(true, Ordering::SeqCst))
        .map_err(|e| Error::new("io", format!("Не удалось установить SIGINT: {e}")))
}
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}
pub fn clear() {
    REQUESTED.store(false, Ordering::SeqCst);
}
