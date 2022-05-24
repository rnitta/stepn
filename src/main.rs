use crate::stepn_config::{read_config, StepnConfig};
use colored::Colorize;
use futures::future::join_all;
use futures::StreamExt;
use std::collections::HashMap;
use std::fmt::Error;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

mod stepn_config;
mod util;

use crate::util::{pad_with_trailing_space, MethodChain};
use once_cell::sync::Lazy;
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

static CONFIG: Lazy<StepnConfig> = Lazy::new(|| read_config().unwrap());

#[tokio::main]
async fn main() -> Result<(), Error> {
    let healthcheck_map: HashMap<String, bool> =
        CONFIG
            .services
            .iter()
            .fold(HashMap::<String, bool>::new(), |mut acc, (cur, _)| {
                acc.insert(cur.to_string(), false);
                acc
            });
    let healthcheck_map_ptr: Arc<RwLock<HashMap<String, bool>>> =
        Arc::new(RwLock::new(healthcheck_map));

    let futures = CONFIG.services.iter().map(|(name, service)| {
        let name = name.to_string();
        let healthcheck_map_ptr = Arc::clone(&healthcheck_map_ptr);
        let future = tokio::spawn(async move {
            if let Some(depends_on) = service.clone().depends_on {
                depends_on.iter().for_each(|dep| 'wait: loop {
                    println!("{} is waiting for {} booting...", name, dep.green());
                    std::thread::sleep(Duration::from_secs(1));
                    if *healthcheck_map_ptr.read().unwrap().get(dep).unwrap() {
                        break 'wait;
                    }
                })
            }

            let mut dependents = if let Some(health_checker) = &service.health_checker {
                if let Some(output_trigger) = &health_checker.output_trigger {
                    output_trigger
                        .iter()
                        .fold(HashMap::<String, bool>::new(), |mut acc, cur| {
                            acc.insert(cur.to_string(), false);
                            acc
                        })
                } else {
                    HashMap::new()
                }
            } else {
                HashMap::new()
            };

            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&service.command)
                .env("IS_STEPN", "true")
                .then(Box::new(|c: &mut Command| {
                    let env = &service.environments;
                    if let Some(env) = env {
                        env.iter().fold(c, |acc, (k, v)| acc.env(k, v))
                    } else {
                        c
                    }
                }))
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect(&format!("failed to start command: {}", service.command));

            let mut reader = FramedRead::new(child.stdout.take().unwrap(), LinesCodec::new());
            while let Some(Ok(line)) = reader.next().await {
                println!(
                    "{}{} {}",
                    pad_with_trailing_space(10, &name.to_string()).red(),
                    ": ".green(),
                    line
                );

                if dependents.iter().any(|(_, flag)| !*flag) {
                    let yet_activated_dependents: Vec<String> = dependents
                        .iter()
                        .filter(|(_, flag)| !**flag)
                        .map(|(k, _)| k.to_string())
                        .collect();
                    yet_activated_dependents.iter().for_each(|keyword| {
                        if line.contains(keyword) {
                            dependents.insert(keyword.to_string(), true);
                        }
                    })
                } else if !*healthcheck_map_ptr
                    .read()
                    .unwrap()
                    .get(&name.to_string())
                    .unwrap()
                {
                    healthcheck_map_ptr
                        .write()
                        .unwrap()
                        .insert(name.to_string(), true);
                }
            }
        });
        future
    });
    join_all(futures).await;
    println!("stepn finished");

    Ok(())
}
