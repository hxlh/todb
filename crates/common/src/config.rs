use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub node: NodeConfig,
    pub log: LogConfig,
    pub rpc: RpcConfig,
}

#[derive(Debug, Deserialize)]
pub struct NodeConfig {
    pub node_id: u64,
    pub data_dir: String,
}

#[derive(Debug, Deserialize)]
pub struct LogConfig {
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
}

fn default_log_format() -> String {
    "json".to_string()
}

#[derive(Debug, Deserialize)]
pub struct RpcConfig {
    pub listen_addr: String,
}

impl Config {
    pub fn load(path: &str) -> crate::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::Error::Known {
                code: crate::ErrorCode::Config,
                message: format!("failed to read config file {}: {}", path, e),
            })?;
        serde_yaml::from_str(&content).map_err(|e| crate::Error::Known {
            code: crate::ErrorCode::Config,
            message: format!("failed to parse config: {}", e),
        })
    }
}
