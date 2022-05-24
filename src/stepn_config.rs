use serde::Deserialize;
use std::collections::HashMap;
use std::io::Error;

pub fn read_config() -> Result<StepnConfig, Error> {
    let current_path = std::env::current_dir()?;
    let filepath = format!("{}/proc.toml", current_path.display());
    let content = std::fs::read_to_string(filepath).expect("proc.toml not found");
    let settings = toml::from_str(&content)?;
    Ok(settings)
}

#[derive(Deserialize, Clone, Debug)]
pub struct StepnConfig {
    pub services: HashMap<String, Service>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Service {
    pub command: String,
    pub depends_on: Option<Vec<String>>,
    pub health_checker: Option<HealthChecker>,
    pub environments: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct HealthChecker {
    pub output_trigger: Option<Vec<String>>,
}
