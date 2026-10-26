//! Stands in for a real database: a JSON file in the temp folder, so every instance of an example
//! sees the same users. Every query waits 200 ms and prints a line, so a cache hit is easy to tell
//! from a load. A change locks a file next to it, across instances, so changes never overwrite
//! each other, and replaces the whole file at once, so a read never sees half of one.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::PathBuf,
    time::Duration,
};

use crate::User;

type Users = BTreeMap<u64, String>;

pub struct Database {
    path: PathBuf,
}

impl Default for Database {
    fn default() -> Self {
        Self::at(std::env::temp_dir().join("sisa-caching-example-users.json"))
    }
}

impl Database {
    /// The database in `path`, with three users if the file doesn't exist yet.
    fn at(path: PathBuf) -> Self {
        let database = Self { path };
        let _lock = database.lock();

        if !database.path.exists() {
            let users = [(1, "Ada"), (2, "Grace"), (3, "Linus")]
                .map(|(id, name)| (id, name.to_string()))
                .into();

            database.replace(&users);
        }

        database
    }

    /// Lets one change run at a time, in every instance, until the returned file is dropped.
    fn lock(&self) -> File {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))
            .unwrap();

        file.lock().unwrap();
        file
    }

    fn users(&self) -> Users {
        serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap()
    }

    /// Writes the users to a file of their own, then renames it over the old one.
    fn replace(&self, users: &Users) {
        let next = self.path.with_extension("next");

        fs::write(&next, serde_json::to_vec(users).unwrap()).unwrap();
        fs::rename(&next, &self.path).unwrap();
    }

    fn change(&self, edit: impl FnOnce(&mut Users)) {
        let _lock = self.lock();
        let mut users = self.users();

        edit(&mut users);
        self.replace(&users);
    }

    async fn query(&self, what: &str) -> Users {
        tokio::time::sleep(Duration::from_millis(200)).await;
        tracing::info!("database: {what}");

        self.users()
    }

    pub async fn count(&self) -> u64 {
        self.query("counting users").await.len() as u64
    }

    pub async fn find(&self, id: u64) -> Option<User> {
        let name = self
            .query(&format!("loading user {id}"))
            .await
            .remove(&id)?;

        Some(User { id, name })
    }

    pub async fn list(&self) -> Vec<User> {
        self.query("listing users")
            .await
            .into_iter()
            .map(|(id, name)| User { id, name })
            .collect()
    }

    pub fn save(&self, user: &User) {
        self.change(|users| {
            users.insert(user.id, user.name.clone());
        });
    }

    pub fn delete(&self, id: u64) {
        self.change(|users| {
            users.remove(&id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_changes_all_land_and_reads_never_see_half_a_write() {
        let path = std::env::temp_dir().join(format!(
            "sisa-caching-example-users-test-{}.json",
            std::process::id()
        ));
        let database = Database::at(path.clone());

        std::thread::scope(|scope| {
            for id in 100..132 {
                let database = &database;

                scope.spawn(move || {
                    database.save(&User {
                        id,
                        name: format!("user {id}"),
                    });
                    database.users();
                });
            }
            for id in 1..=3 {
                let database = &database;

                scope.spawn(move || database.delete(id));
            }
        });

        let ids: Vec<u64> = database.users().into_keys().collect();

        for path in [path.clone(), path.with_extension("lock")] {
            _ = fs::remove_file(path);
        }
        assert_eq!(ids, Vec::from_iter(100..132));
    }
}
