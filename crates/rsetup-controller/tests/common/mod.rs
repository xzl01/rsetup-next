use rsetup_controller::{ControllerConfig, DbPool};
use std::sync::Mutex;

pub async fn required_test_db() -> DbPool {
    let database_url = std::env::var("CONTROLLER_TEST_DATABASE_URL")
        .expect("CONTROLLER_TEST_DATABASE_URL must name an isolated test database");
    let db = DbPool::connect(&ControllerConfig {
        database_url,
        listen_address: String::new(),
    })
    .await
    .expect("test database connection");
    let name: String = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&db.0)
        .await
        .expect("selected test database");
    assert!(
        name.contains("test"),
        "database name must identify an isolated test database"
    );
    db
}

#[derive(Default)]
pub struct RecordingSecretSink(Mutex<Vec<String>>);
impl rsetup_controller::BootstrapSecretSink for RecordingSecretSink {
    fn emit(&self, _username: &str, password: &str) {
        self.0.lock().unwrap().push(password.to_owned());
    }
}
impl RecordingSecretSink {
    pub fn emissions(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

pub async fn user_hash(db: &DbPool, username: &str) -> String {
    sqlx::query_scalar("SELECT password_hash FROM users WHERE username = ?")
        .bind(username)
        .fetch_one(&db.0)
        .await
        .unwrap()
}
