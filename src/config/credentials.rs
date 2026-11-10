//! Local credentials for the HTTP MCP endpoint.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub mcp_token: String,
}

impl Credentials {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.is_file() {
            return Self::load(path);
        }
        let credentials = Self {
            mcp_token: uuid::Uuid::new_v4().simple().to_string(),
        };
        credentials.save(path)?;
        Ok(credentials)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read credentials {}", path.display()))?;
        let credentials: Self = toml::from_str(&text)
            .with_context(|| format!("cannot parse credentials {}", path.display()))?;
        if credentials.mcp_token.trim().is_empty() {
            bail!("mcp_token must not be empty");
        }
        Ok(credentials)
    }

    fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let text = toml::to_string_pretty(self).context("cannot serialize credentials")?;
        temporary.write_all(text.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }
}
