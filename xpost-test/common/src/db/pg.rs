//! Runtime PostgreSQL settings for the experiment runner.

use std::{
    collections::BTreeMap,
    fmt,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use tokio_postgres::{Client, Config, NoTls, config::SslMode, error::SqlState};

const SETTINGS_PATH: &str = "/root/.db";
const KEYS: [&str; 5] = ["PGHOST", "PGPORT", "PGUSER", "PGPASSWORD", "PGDATABASE"];

pub struct Database(Config);

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Database([redacted])")
    }
}

impl Database {
    pub fn load() -> Result<Self> {
        Self::load_file(Path::new(SETTINGS_PATH), 0)
    }

    fn load_file(path: &Path, owner: u32) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("open PostgreSQL settings {}", path.display()))?;
        let metadata = file
            .metadata()
            .context("inspect PostgreSQL settings file")?;
        ensure!(
            metadata.is_file() && metadata.uid() == owner && metadata.mode() & 0o7777 == 0o600,
            "{} must be a regular file owned by uid {owner} with mode 0600",
            path.display()
        );
        let mut text = String::new();
        file.read_to_string(&mut text)
            .context("read PostgreSQL settings as UTF-8")?;
        Self::parse(&text).with_context(|| format!("parse PostgreSQL settings {}", path.display()))
    }

    #[cfg(test)]
    pub fn load_fixture() -> Result<Self> {
        let path = std::env::var_os("XPOST_CASE_TEST_DB_FILE").context(
            "set XPOST_CASE_TEST_DB_FILE to settings for a disposable Protocol V1 database",
        )?;
        Self::load_file(Path::new(&path), unsafe { libc::geteuid() })
    }

    fn parse(text: &str) -> Result<Self> {
        let mut values = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("expected KEY=VALUE on line {number}"))?;
            let key = key.trim();
            ensure!(KEYS.contains(&key), "unrecognized setting on line {number}");
            let value = dotenv_value(value.trim())
                .with_context(|| format!("invalid {key} on line {number}"))?;
            ensure!(
                !value.is_empty() && !value.contains('\0'),
                "invalid {key} on line {number}"
            );
            ensure!(
                values.insert(key, value).is_none(),
                "duplicate {key} on line {number}"
            );
        }
        for key in KEYS {
            ensure!(values.contains_key(key), "missing {key}");
        }
        let port = values["PGPORT"]
            .parse::<u16>()
            .ok()
            .filter(|&port| port != 0)
            .context("PGPORT must be a nonzero u16")?;
        let mut config = Config::new();
        config
            .host(&values["PGHOST"])
            .port(port)
            .user(&values["PGUSER"])
            .password(&values["PGPASSWORD"])
            .dbname(&values["PGDATABASE"])
            .ssl_mode(SslMode::Disable)
            .connect_timeout(Duration::from_secs(10));
        Ok(Self(config))
    }

    pub async fn connect(&self) -> Result<Client> {
        let (client, connection) = self.0.connect(NoTls).await.map_err(|error| {
            // A server can put arbitrary text in its authentication error.
            match error.code() {
                Some(
                    &SqlState::INVALID_PASSWORD | &SqlState::INVALID_AUTHORIZATION_SPECIFICATION,
                ) => anyhow::anyhow!("protocol database authentication failed"),
                Some(&SqlState::INVALID_CATALOG_NAME) => {
                    anyhow::anyhow!("protocol database does not exist")
                }
                _ => anyhow::anyhow!("connect protocol database failed"),
            }
        })?;
        tokio::spawn(async move {
            if connection.await.is_err() {
                log::warn!("protocol database connection closed with an error");
            }
        });
        Ok(client)
    }
}

fn dotenv_value(value: &str) -> Result<String> {
    let mut chars = value.chars();
    let Some(quote @ ('\'' | '"')) = chars.next() else {
        let end = value.match_indices('#').find_map(|(index, _)| {
            (index == 0 || value[..index].ends_with(char::is_whitespace)).then_some(index)
        });
        return Ok(value[..end.unwrap_or(value.len())].trim_end().into());
    };

    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch == quote {
            let rest = chars.as_str().trim_start();
            ensure!(
                rest.is_empty() || rest.starts_with('#'),
                "unexpected text after quoted value"
            );
            return Ok(result);
        }
        if quote == '"' && ch == '\\' {
            match chars.next() {
                Some(escaped @ ('"' | '\\')) => result.push(escaped),
                _ => bail!("invalid double-quoted escape"),
            }
        } else {
            result.push(ch);
        }
    }
    bail!("unterminated quoted value")
}

#[cfg(test)]
mod runtime_tests {
    use super::Database;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
    use uuid::Uuid;

    const SETTINGS: &str = concat!(
        "# Synthetic database settings\n",
        " PGHOST = 127.0.0.1 # loopback\n",
        "PGPORT=5432\n",
        "PGUSER=\"fixture\"\n",
        "PGPASSWORD='D $() # \\ = ! \"'\n",
        "PGDATABASE=fixture_db\n",
    );

    struct ConfigFile(PathBuf);

    impl ConfigFile {
        fn new(contents: &str) -> Self {
            let file = Self(std::env::temp_dir().join(format!("xpost-db-{}", Uuid::new_v4())));
            fs::write(&file.0, contents).unwrap();
            fs::set_permissions(&file.0, fs::Permissions::from_mode(0o600)).unwrap();
            file
        }

        fn load(&self) -> anyhow::Result<Database> {
            Database::load_file(&self.0, unsafe { libc::geteuid() })
        }
    }

    impl Drop for ConfigFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn runtime_database_loads_literal_credentials_and_redacts_debug() {
        let db = ConfigFile::new(SETTINGS).load().unwrap();
        assert_eq!(
            db.0.get_hosts(),
            &[tokio_postgres::config::Host::Tcp("127.0.0.1".into())]
        );
        assert_eq!(db.0.get_ports(), &[5432]);
        assert_eq!(db.0.get_user(), Some("fixture"));
        assert_eq!(db.0.get_dbname(), Some("fixture_db"));
        assert_eq!(db.0.get_password(), Some(b"D $() # \\ = ! \"".as_slice()));
        assert!(!format!("{db:?}").contains("D $()"));
    }

    #[test]
    fn runtime_database_rejects_bad_settings_without_echoing_their_contents() {
        for (old, new, want) in [
            ("PGPORT=5432", "PGPORT=0", "nonzero u16"),
            ("PGPORT=5432", "PGPORT=65536", "nonzero u16"),
            ("PGPORT=5432", "PGPORT=text", "nonzero u16"),
            ("PGPORT=5432", "", "missing PGPORT"),
            ("PGUSER=\"fixture\"", "PGUSER=  # empty", "invalid PGUSER"),
            ("PGDATABASE=fixture_db", "PGDATABASE", "expected KEY=VALUE"),
            ("PGDATABASE=fixture_db", "PGHOST=again", "duplicate PGHOST"),
        ] {
            let file = ConfigFile::new(&SETTINGS.replace(old, new));
            let error = format!("{:#}", file.load().unwrap_err());
            assert!(error.contains(want), "{error}");
            assert!(!error.contains("D $()"));
        }
        let file = ConfigFile::new(&format!("{SETTINGS}PGPASSWORD='missing-close-secret"));
        let error = format!("{:#}", file.load().unwrap_err());
        assert!(error.contains("unterminated quoted value"));
        assert!(!error.contains("missing-close-secret"));
    }

    #[test]
    fn runtime_database_requires_a_private_file() {
        let file = ConfigFile::new(SETTINGS);
        fs::set_permissions(&file.0, fs::Permissions::from_mode(0o644)).unwrap();
        let error = format!("{:#}", file.load().unwrap_err());
        assert!(error.contains("mode 0600"));
        fs::remove_file(&file.0).unwrap();
        let error = format!("{:#}", file.load().unwrap_err());
        assert!(error.contains("open PostgreSQL settings"));
    }
}
