use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    pub servers: Vec<ServerEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerEntry {
    pub address: String,
    pub client_uuid: String,
    pub minecraft_uuid: String,
    pub pinned_cert: Option<String>,
    pub supports_components: bool,
}

pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let proj_dirs = ProjectDirs::from("", "", "minechat").ok_or("Can't get config dir")?;
    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(config_dir.join("servers.json"))
}

pub fn load_config() -> Result<ServerConfig, Box<dyn std::error::Error>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(ServerConfig {
            servers: Vec::new(),
        });
    }
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}

pub fn save_config(config: &ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path()?;
    let temp_path = path.with_extension("tmp");
    {
        let mut file = File::create(&temp_path)?;
        serde_json::to_writer_pretty(&mut file, config)?;
    }
    std::fs::rename(&temp_path, &path)?;
    Ok(())
}
