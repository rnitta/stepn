use crate::stepn_config::{read_config, StepnConfig};
use crate::util::{compute_label_width, pad_with_trailing_space};
use colored::Colorize;
use futures::future::join_all;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, OnceLock, RwLock};
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, System};
use tokio_stream::StreamExt;

mod stepn_config;
mod util;

use seahorse::Context;
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

static CONFIG_PATH: OnceLock<String> = OnceLock::new();

static CONFIG: std::sync::LazyLock<StepnConfig> = std::sync::LazyLock::new(|| {
    let path = CONFIG_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| "proc.toml".to_string());
    read_config(&path).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        std::process::exit(1);
    })
});

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

fn extract_config_flag(args: &mut Vec<String>) -> Option<String> {
    if let Some(i) = args.iter().position(|a| a.starts_with("--file=")) {
        let val = args.remove(i);
        return Some(val.trim_start_matches("--file=").to_string());
    }
    if let Some(i) = args.iter().position(|a| a == "-f" || a == "--file") {
        args.remove(i);
        if i < args.len() {
            return Some(args.remove(i));
        } else {
            eprintln!("error: -f/--file requires a path argument");
            std::process::exit(1);
        }
    }
    None
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-v" || a == "--version") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(path) = extract_config_flag(&mut args) {
        CONFIG_PATH.set(path).expect("CONFIG_PATH already set");
    }
    let app = seahorse::App::new(env!("CARGO_PKG_NAME"))
        .description(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .version(env!("CARGO_PKG_VERSION"))
        .usage("stepn [-f <path>] [command] [args]")
        .action(|c| {
            build_runtime().block_on(run(c));
        })
        .command(
            seahorse::Command::new("run")
                .description("run services from config (optionally specify service names)")
                .alias("r")
                .usage("stepn run(r) [service1 service2 ...]")
                .action(|c| {
                    build_runtime().block_on(run(c));
                }),
        )
        .command(
            seahorse::Command::new("execute")
                .description("execute oneshot command in a service's environment")
                .alias("e")
                .usage("stepn execute(e) <service> <command>")
                .action(|c| {
                    build_runtime().block_on(execute(c));
                }),
        )
        .command(
            seahorse::Command::new("validate")
                .description("validate config file")
                .alias("v")
                .usage("stepn validate(v)")
                .action(|_c| {
                    validate();
                }),
        )
        .command(
            seahorse::Command::new("list")
                .description("list services and dependency tree")
                .alias("l")
                .usage("stepn list(l)")
                .action(|_c| {
                    list();
                }),
        );

    app.run(args);
}

fn config_path() -> String {
    CONFIG_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| "proc.toml".to_string())
}

fn validate() {
    let path = config_path();
    match read_config(&path) {
        Ok(config) => {
            println!(
                "{} {} is valid ({} services)",
                "OK:".green(),
                path,
                config.services.len()
            );
        }
        Err(e) => {
            eprintln!("{} {}\n{:#}", "ERROR:".red(), path, e);
            std::process::exit(1);
        }
    }
}

fn list() {
    let config = &*CONFIG;

    let mut names: Vec<&String> = config.services.keys().collect();
    names.sort();

    println!("{}", "Services:".green().bold());
    for name in &names {
        let service = &config.services[*name];
        println!("  {} {}", "*".green(), name.bold());
        println!("    command: {}", service.command);
        if let Some(deps) = &service.depends_on {
            if !deps.is_empty() {
                println!("    depends_on: {}", deps.join(", "));
            }
        }
        if let Some(hc) = &service.health_checker {
            if let Some(triggers) = &hc.output_trigger {
                println!("    health_checker: [{}]", triggers.join(", "));
            }
        }
        if let Some(delay) = service.delay_sec {
            println!("    delay_sec: {}", delay);
        }
        if service.restart {
            let max = service.effective_max_restarts();
            if max == u32::MAX {
                println!("    restart: true (infinite)");
            } else {
                println!("    restart: true (max: {})", max);
            }
        }
    }

    println!("\n{}", "Dependency Tree:".green().bold());
    let roots: Vec<&&String> = names
        .iter()
        .filter(|n| {
            config.services[**n]
                .depends_on
                .as_ref()
                .map(|d| d.is_empty())
                .unwrap_or(true)
        })
        .collect();

    for (i, root) in roots.iter().enumerate() {
        print_tree(config, root, "", i == roots.len() - 1, true);
    }
}

fn print_tree(config: &StepnConfig, name: &str, prefix: &str, is_last: bool, is_root: bool) {
    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let dep_info = config.services[name]
        .depends_on
        .as_ref()
        .filter(|deps| !deps.is_empty())
        .map(|deps| format!(" (depends on: {})", deps.join(", ")))
        .unwrap_or_default();

    println!("{}{}{}{}", prefix, connector, name.bold(), dep_info);

    let child_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    let children = config.dependents_of(name);
    for (i, child) in children.iter().enumerate() {
        print_tree(config, child, &child_prefix, i == children.len() - 1, false);
    }
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
        eprintln!(
            "failed to start command '{}': {}",
            oneshot_command.join(" "),
            e
        );
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

async fn run(c: &Context) {
    let service_names: Vec<String> = if c.args.is_empty() {
        CONFIG.services.keys().cloned().collect()
    } else {
        for name in &c.args {
            if !CONFIG.services.contains_key(name) {
                eprintln!("error: service '{}' is not defined", name);
                let mut available: Vec<&String> = CONFIG.services.keys().collect();
                available.sort();
                eprintln!(
                    "available services: {}",
                    available
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                std::process::exit(1);
            }
        }
        let resolved = CONFIG.resolve_transitive_deps(&c.args);
        let mut resolved_sorted: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();
        resolved_sorted.sort();
        println!("Running services: {}", resolved_sorted.join(", "));
        resolved.into_iter().collect()
    };

    let label_width = compute_label_width(service_names.iter());

    let healthcheck_map: HashMap<String, bool> =
        service_names.iter().map(|k| (k.clone(), false)).collect();

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

    let futures = service_names.iter().map(|name| {
        let service = &CONFIG.services[name];
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

            let max_restarts = service.effective_max_restarts();
            let mut restart_count: u32 = 0;

            loop {
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
                let child_pid = child.id();
                if let Some(pid) = child_pid {
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

                if let Some(pid) = child_pid {
                    children_ptr
                        .write()
                        .expect("lock poisoned")
                        .retain(|p| *p != pid);
                }

                if !service.restart {
                    break;
                }

                if max_restarts != u32::MAX && restart_count >= max_restarts {
                    println!(
                        "{}{} {}",
                        pad_with_trailing_space(label_width, &name).yellow(),
                        ": ".yellow(),
                        "process exited, max restarts reached".red()
                    );
                    break;
                }

                restart_count += 1;
                let restart_msg = if max_restarts == u32::MAX {
                    format!("process crashed, restarting... (attempt {})", restart_count)
                } else {
                    format!(
                        "process crashed, restarting... (attempt {}/{})",
                        restart_count, max_restarts
                    )
                };
                println!(
                    "{}{} {}",
                    pad_with_trailing_space(label_width, &name).yellow(),
                    ": ".yellow(),
                    restart_msg.yellow()
                );

                healthcheck_map_ptr
                    .write()
                    .expect("lock poisoned")
                    .insert(name.clone(), false);

                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    });
    join_all(futures).await;
    println!("stepn finished");
}
