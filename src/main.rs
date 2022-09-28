use crate::stepn_config::{read_config, StepnConfig};
use colored::Colorize;
use futures::executor::block_on;
use futures::future::join_all;
use futures::StreamExt;
use std::collections::HashMap;
use std::fmt::Error;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

mod stepn_config;
mod util;

use crate::util::{pad_with_trailing_space, MethodChain};
use once_cell::sync::Lazy;
use seahorse::Context;
use sysinfo::{Pid, ProcessExt, SystemExt};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

static CONFIG: Lazy<StepnConfig> = Lazy::new(|| read_config().unwrap());

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    let app = seahorse::App::new(env!("CARGO_PKG_NAME"))
        .description(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .version(env!("CARGO_PKG_VERSION"))
        .usage("cli [args]")
        .action(|c| block_on(run(c)))
        .command(
            seahorse::Command::new("run")
                .description("run command from proc.toml")
                .alias("r")
                .usage("stepn run(r)")
                .action(|c| block_on(run(c))),
        )
        .command(
            seahorse::Command::new("execute")
                .description("execute oneshot command")
                .alias("e")
                .usage("stepn execute(e) <service> <command>")
                .action(|c| block_on(execute(c))),
        );

    app.run(args);
    Ok(())
}

async fn execute(con: &Context) {
    let service_name = con.args.get(0).expect("args not sufficient").clone();
    let service = CONFIG
        .services
        .get(&service_name)
        .expect(&format!("service {} is not defined", service_name));
    let oneshot_command = con
        .args
        .get(1..(con.args.len()))
        .expect("command not passed")
        .to_vec();
    println!("{:?}", oneshot_command);
    let future = tokio::spawn(async move {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&oneshot_command.join(" "))
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
            .expect(&format!(
                "failed to start command: {}",
                oneshot_command.join(" ")
            ));

        let stdout = child.stdout.take().unwrap();
        let mut reader = FramedRead::new(stdout, LinesCodec::new());
        while let Some(Ok(line)) = reader.next().await {
            println!(
                "{}{} {}",
                pad_with_trailing_space(10, &service_name.to_string()).blue(),
                ": ".green(),
                line
            );
        }
    });
    future.await.unwrap();
}

async fn run(c: &Context) {
    println!("{:?}", c.args);
    let healthcheck_map: HashMap<String, bool> =
        CONFIG
            .services
            .iter()
            .fold(HashMap::<String, bool>::new(), |mut acc, (cur, _)| {
                acc.insert(cur.to_string(), false);
                acc
            });

    let children: Arc<RwLock<Vec<i32>>> = Arc::new(RwLock::new(Vec::new()));
    let ptr = Arc::clone(&children);
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C!");
        for pid in ptr.write().unwrap().iter_mut() {
            println!("killing!");
            let pid = nix::unistd::Pid::from_raw(pid.clone());
            nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM)
                .unwrap_or_else(|_| println!("kill signal failed as to pid: {}", pid));
        }
        // wait until truly the process killed
        let s = sysinfo::System::new_all();
        for pid in ptr.write().unwrap().iter_mut() {
            while let Some(process) = s.process(Pid::from(pid.clone())) {
                thread::sleep(Duration::from_secs(2));
                println!("Waiting {} process terminated. pid: {}.", process.name(), pid);
            }
        }
        std::process::exit(1);
    })
    .expect("Error setting Ctrl-C handler");

    let healthcheck_map_ptr: Arc<RwLock<HashMap<String, bool>>> =
        Arc::new(RwLock::new(healthcheck_map));

    let futures = CONFIG.services.iter().map(|(name, service)| {
        let name = name.to_string();
        let healthcheck_map_ptr = Arc::clone(&healthcheck_map_ptr);
        let children_ptr = Arc::clone(&children);
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

            if let Some(delay_sec) = service.clone().delay_sec {
                println!("{}: Delaying {} secs", name, delay_sec);
                std::thread::sleep(Duration::from_secs(delay_sec))
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

            let stdout = child.stdout.take().unwrap();
            if let Some(pid) = child.id() {
                children_ptr.write().unwrap().push(pid as i32);
            }

            let mut reader = FramedRead::new(stdout, LinesCodec::new());
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
}
