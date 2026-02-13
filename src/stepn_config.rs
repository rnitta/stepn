use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub fn read_config() -> Result<StepnConfig> {
    let current_path = std::env::current_dir()?;
    let filepath = format!("{}/proc.toml", current_path.display());
    let content =
        std::fs::read_to_string(&filepath).with_context(|| format!("{} not found", filepath))?;
    let config: StepnConfig =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", filepath))?;
    config.validate()?;
    Ok(config)
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
    pub delay_sec: Option<u64>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct HealthChecker {
    pub output_trigger: Option<Vec<String>>,
}

impl StepnConfig {
    /// Validate that all depends_on references exist and there are no circular dependencies.
    fn validate(&self) -> Result<()> {
        let service_names: HashSet<&str> = self.services.keys().map(|s| s.as_str()).collect();

        // Check all depends_on references exist
        for (name, service) in &self.services {
            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    if !service_names.contains(dep.as_str()) {
                        bail!(
                            "service '{}' depends on '{}', which is not defined",
                            name,
                            dep
                        );
                    }
                }
            }
        }

        // Detect circular dependencies via DFS
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();
        for name in self.services.keys() {
            if !visited.contains(name.as_str()) {
                self.detect_cycle(name, &mut visited, &mut in_stack)?;
            }
        }

        Ok(())
    }

    fn detect_cycle<'a>(
        &'a self,
        node: &'a str,
        visited: &mut HashSet<&'a str>,
        in_stack: &mut HashSet<&'a str>,
    ) -> Result<()> {
        visited.insert(node);
        in_stack.insert(node);

        if let Some(service) = self.services.get(node) {
            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    if !visited.contains(dep.as_str()) {
                        self.detect_cycle(dep, visited, in_stack)?;
                    } else if in_stack.contains(dep.as_str()) {
                        bail!("circular dependency detected: {} -> {}", node, dep);
                    }
                }
            }
        }

        in_stack.remove(node);
        Ok(())
    }
}
