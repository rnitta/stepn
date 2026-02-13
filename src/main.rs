use crate::stepn_config::{read_config, StepnConfig};
use crate::util::{compute_label_width, pad_with_trailing_space};
use colored::Colorize;
use futures::future::join_all;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, System};
use tokio_stream::StreamExt;

mod stepn_config;
mod util;

use seahorse::Context;
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

static CONFIG: std::sync::LazyLock<StepnConfig> =
    std::sync::LazyLock::new(|| read_config().expect("failed to load proc.toml"));

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let app = seahorse::App::new(env!("CARGO_PKG_NAME"))
        .description(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .version(env!("CARGO_PKG_VERSION"))
        .usage("stepn [command] [args]")
        .action(|c| {
            build_runtime().block_on(run(c));
        })
        .command(
            seahorse::Command::new("run")
                .description("run command from proc.toml")
                .alias("r")
                .usage("stepn run(r)")
                .action(|c| {
                    build_runtime().block_on(run(c));
                }),
        )
        .command(
            seahorse::Command::new("execute")
                .description("execute oneshot command")
                .alias("e")
                .usage("stepn execute(e) <service> <command>")
                .action(|c| {
                    build_runtime().block_on(execute(c));
                }),
        );

    app.run(args);
}

async fn execute(con: &Context) {
    let service_name = con
        .args
        .first()
        .unwrap_or_else(|| {
            eprintln!("error: service name required");
            std::process::exit(1);
        })
        .clone();
    let service = CONFIG
        .services
        .get(&service_name)
        .unwrap_or_else(|| {
            eprintln!("error: service '{}' is not defined", service_name);
            std::process::exit(1);
        });
    let oneshot_command = con
        .args
        .get(1..con.args.len())
        .unwrap_or_else(|| {
            eprintln!("error: command not passed");
            std::process::exit(1);
        })
        .to_vec();

    let label_width = compute_label_width(std::iter::once(&service_name));

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(oneshot_command.join(" "))
        .env("IS_STEPN", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = &service.environments {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }

    let mut child = cmd.spawn().unwrap_or_else(|e| {
        eprintln!("failed to start command '{}': {}", oneshot_command.join(" "), e);
        std::process::exit(1);
    });

    let stdout = child.stdout.take().expect("stdout not captured");
    let mut reader = FramedRead::new(stdout, LinesCodec::new());
    while let Some(Ok(line)) = reader.next().await {
        println!(
            "{}{} {}",
            pad_with_trailing_space(label_width, &service_name).blue(),
            ": ".green(),
            line
        );
    }
}

async fn run(_c: &Context) {
    let label_width = compute_label_width(CONFIG.services.keys());

    let healthcheck_map: HashMap<String, bool> = CONFIG
        .services
        .keys()
        .map(|k| (k.clone(), false))
        .collect();

    let children: Arc<RwLock<Vec<u32>>> = Arc::new(RwLock::new(Vec::new()));
    let ptr = Arc::clone(&children);
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C!");
        let pids: Vec<u32> = ptr.read().expect("lock poisoned").clone();
        for pid in &pids {
            let nix_pid = nix::unistd::Pid::from_raw(*pid as i32);
            nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGTERM)
                .unwrap_or_else(|_| eprintln!("kill signal failed for pid: {}", pid));
        }

        let mut s = System::new_all();
        for pid in &pids {
            s.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            while s.process(Pid::from_u32(*pid)).is_some() {
                thread::sleep(Duration::from_millis(500));
                s.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                eprintln!("waiting for process {} to terminate...", pid);
            }
        }
        std::process::exit(1);
    })
    .expect("failed to set Ctrl-C handler");

    let healthcheck_map_ptr: Arc<RwLock<HashMap<String, bool>>> =
        Arc::new(RwLock::new(healthcheck_map));

    let futures = CONFIG.services.iter().map(|(name, service)| {
        let name = name.to_string();
        let healthcheck_map_ptr = Arc::clone(&healthcheck_map_ptr);
        let children_ptr = Arc::clone(&children);
        tokio::spawn(async move {
            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    loop {
                        println!("{} is waiting for {} booting...", name, dep.green());
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        if *healthcheck_map_ptr
                            .read()
                            .expect("lock poisoned")
                            .get(dep.as_str())
                            .unwrap_or(&false)
                        {
                            break;
                        }
                    }
                }
            }

            if let Some(delay_sec) = service.delay_sec {
                println!("{}: Delaying {} secs", name, delay_sec);
                tokio::time::sleep(Duration::from_secs(delay_sec)).await;
            }

            let mut pending_triggers: HashMap<String, bool> = service
                .health_checker
                .as_ref()
                .and_then(|hc| hc.output_trigger.as_ref())
                .map(|triggers| triggers.iter().map(|t| (t.clone(), false)).collect())
                .unwrap_or_default();

            let mut cmd = Command::new("sh");
            cmd.kill_on_drop(true)
                .arg("-c")
                .arg(&service.command)
                .env("IS_STEPN", "true")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(env) = &service.environments {
                for (k, v) in env {
                    cmd.env(k, v);
                }
            }

            let mut child = cmd.spawn().unwrap_or_else(|e| {
                panic!("failed to start command '{}': {}", service.command, e);
            });

            let stdout = child.stdout.take().expect("stdout not captured");
            if let Some(pid) = child.id() {
                children_ptr.write().expect("lock poisoned").push(pid);
            }
            let stderr = child.stderr.take().expect("stderr not captured");

            let stdout_reader = FramedRead::new(stdout, LinesCodec::new());
            let stderr_reader = FramedRead::new(stderr, LinesCodec::new());
            let mut merged_stream = stdout_reader.merge(stderr_reader.map(|r| {
                r.map(|line| format!("{}", format!("*stderr* {}", line).red()))
            }));

            while let Some(Ok(line)) = merged_stream.next().await {
                println!(
                    "{}{} {}",
                    pad_with_trailing_space(label_width, &name).green(),
                    ": ".green(),
                    line
                );

                if pending_triggers.values().any(|done| !done) {
                    let unmatched: Vec<String> = pending_triggers
                        .iter()
                        .filter(|(_, done)| !**done)
                        .map(|(k, _)| k.clone())
                        .collect();
                    for keyword in &unmatched {
                        if line.contains(keyword.as_str()) {
                            pending_triggers.insert(keyword.clone(), true);
                        }
                    }
                } else if !*healthcheck_map_ptr
                    .read()
                    .expect("lock poisoned")
                    .get(name.as_str())
                    .unwrap_or(&false)
                {
                    healthcheck_map_ptr
                        .write()
                        .expect("lock poisoned")
                        .insert(name.clone(), true);
                }
            }
        })
    });
    join_all(futures).await;
    println!("stepn finished");
}
