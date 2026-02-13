use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};

pub fn read_config(filepath: &str) -> Result<StepnConfig> {
    let content =
        std::fs::read_to_string(filepath).with_context(|| format!("{} not found", filepath))?;
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
    #[serde(default)]
    pub restart: bool,
    pub max_restarts: Option<u32>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct HealthChecker {
    pub output_trigger: Option<Vec<String>>,
}

impl Service {
    pub fn effective_max_restarts(&self) -> u32 {
        if !self.restart {
            return 0;
        }
        match self.max_restarts {
            None => 3,
            Some(0) => u32::MAX,
            Some(n) => n,
        }
    }
}

impl StepnConfig {
    fn validate(&self) -> Result<()> {
        let service_names: HashSet<&str> = self.services.keys().map(|s| s.as_str()).collect();

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

    pub fn resolve_transitive_deps(&self, names: &[String]) -> HashSet<String> {
        let mut result = HashSet::new();
        let mut queue: VecDeque<String> = names.iter().cloned().collect();
        while let Some(name) = queue.pop_front() {
            if result.insert(name.clone()) {
                if let Some(service) = self.services.get(&name) {
                    if let Some(deps) = &service.depends_on {
                        for dep in deps {
                            if !result.contains(dep) {
                                queue.push_back(dep.clone());
                            }
                        }
                    }
                }
            }
        }
        result
    }

    pub fn dependents_of(&self, name: &str) -> Vec<String> {
        let mut result: Vec<String> = self
            .services
            .iter()
            .filter(|(_, svc)| {
                svc.depends_on
                    .as_ref()
                    .is_some_and(|deps| deps.iter().any(|d| d == name))
            })
            .map(|(n, _)| n.clone())
            .collect();
        result.sort();
        result
    }
}
