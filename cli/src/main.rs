// SPDX-FileCopyrightText: © 2021 ChiselStrike <info@chiselstrike.com>

use crate::cmd::apply::apply;
use crate::cmd::dev::cmd_dev;
use crate::project::{create_project, CreateProjectOptions, read_manifest};
use crate::server::{start_server, wait};
use anyhow::{anyhow, Result};
use chisel::chisel_rpc_client::ChiselRpcClient;
use chisel::{
    ChiselDeleteRequest, DescribeRequest, PopulateRequest, RestartRequest, StatusRequest,
};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use structopt::StructOpt;
use structopt::clap::AppSettings;

mod chisel;
mod cmd;
mod project;
mod server;
mod ts;

fn parse_version(version: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!version.is_empty(), "version name can't be empty");
    Ok(version.to_string())
}

pub(crate) static DEFAULT_API_VERSION: &str = "dev";

#[derive(StructOpt, Debug)]
#[structopt(name = "chisel", version = env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"))]
struct Opt {
    /// RPC server address.
    #[structopt(short, long, default_value = "http://localhost:50051")]
    rpc_addr: String,
    #[structopt(subcommand)]
    cmd: Command,
}

#[derive(StructOpt, Debug)]
enum Command {
    /// Create a new ChiselStrike project in current directory.
    Init {
        /// Force project initialization by overwriting files if needed.
        #[structopt(long)]
        force: bool,
        /// Skip generating example code.
        #[structopt(long)]
        no_examples: bool,
        /// Enable the optimizer
        #[structopt(long, parse(try_from_str), default_value = "true")]
        optimize: bool,
        /// Enable auto-indexing.
        #[structopt(long, parse(try_from_str), default_value = "false")]
        auto_index: bool,
    },
    /// Describe the endpoints, types, and policies.
    Describe,
    /// Start a ChiselStrike server for local development.
    #[structopt(settings(&[AppSettings::TrailingVarArg, AppSettings::AllowLeadingHyphen]))]
    Dev {
        /// calls tsc --noEmit to check types. Useful if your IDE isn't doing it.
        #[structopt(long)]
        type_check: bool,
        /// Remaining arguments will be forwarded to the server.
        server_options: Vec<String>,
    },
    /// Create a new ChiselStrike project.
    New {
        /// Path where to create the project.
        path: String,
        /// Skip generating example code.
        #[structopt(long)]
        no_examples: bool,
        /// Enable the optimizer
        #[structopt(long, parse(try_from_str), default_value = "true")]
        optimize: bool,
        /// Enable auto-indexing.
        #[structopt(long, parse(try_from_str), default_value = "false")]
        auto_index: bool,
    },
    /// Start the ChiselStrike server.
    #[structopt(settings(&[AppSettings::TrailingVarArg, AppSettings::AllowLeadingHyphen]))]
    Start {
        /// Enable development mode
        #[structopt(short, long)]
        dev: bool,
        /// calls tsc --noEmit to check types. Useful if your IDE isn't doing it.
        #[structopt(long, requires = "dev")]
        type_check: bool,
        /// Remaining arguments will be forwarded to the server.
        server_options: Vec<String>,
    },
    /// Show ChiselStrike server status.
    Status,
    /// Restart the running ChiselStrike server.
    Restart,
    /// Wait for the ChiselStrike server to start.
    Wait,
    /// Apply configuration to the ChiselStrike server.
    Apply {
        #[structopt(long)]
        allow_type_deletion: bool,
        #[structopt(long, default_value = DEFAULT_API_VERSION, parse(try_from_str=parse_version))]
        version: String,
        /// calls tsc --noEmit to check types. Useful if your IDE isn't doing it.
        #[structopt(long)]
        type_check: bool,
    },
    /// Delete configuration from the ChiselStrike server.
    Delete {
        #[structopt(long, default_value = DEFAULT_API_VERSION, parse(try_from_str=parse_version))]
        version: String,
    },
    Populate {
        #[structopt(long)]
        version: String,
        #[structopt(long)]
        from: String,
    },
}

async fn delete<S: ToString>(server_url: String, version: S) -> Result<()> {
    let version = version.to_string();
    let mut client = ChiselRpcClient::connect(server_url).await?;

    let msg = execute!(
        client
            .delete(tonic::Request::new(ChiselDeleteRequest { version }))
            .await
    );
    println!("{}", msg.result);
    Ok(())
}

async fn populate(server_url: String, to_version: String, from_version: String) -> Result<()> {
    let mut client = ChiselRpcClient::connect(server_url).await?;

    let msg = execute!(
        client
            .populate(tonic::Request::new(PopulateRequest {
                to_version,
                from_version,
            }))
            .await
    );
    println!("{}", msg.msg);
    Ok(())
}

async fn launch_server(server_url: String, dev: bool, type_check: bool, server_options: Vec<String>) -> Result<()> {
    let manifest = if dev { Some(read_manifest()?) } else { None };
    let mut server = start_server(Some(server_options))?;
    wait(server_url.clone()).await?;
    if dev {
        cmd_dev(server_url, manifest.unwrap(), type_check).await?;
    }
    server.wait()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::from_args();
    let server_url = opt.rpc_addr;
    match opt.cmd {
        Command::Init {
            force,
            no_examples,
            optimize,
            auto_index,
        } => {
            let cwd = env::current_dir()?;
            let opts = CreateProjectOptions {
                force,
                examples: !no_examples,
                optimize,
                auto_index,
            };
            create_project(&cwd, opts)?;
        }
        Command::Describe => {
            let mut client = ChiselRpcClient::connect(server_url).await?;
            let request = tonic::Request::new(DescribeRequest {});
            let response = execute!(client.describe(request).await);

            for version_def in response.version_defs {
                println!("Version: {} {{", version_def.version);
                for def in &version_def.type_defs {
                    println!("  class {} {{", def.name);
                    for field in &def.field_defs {
                        let labels = if field.labels.is_empty() {
                            "".into()
                        } else {
                            let mut labels = field
                                .labels
                                .iter()
                                .map(|x| format!("\"{}\", ", x))
                                .collect::<String>();
                            // We add a , and a space in the map() function above to each element,
                            // so for the last element we pop them both.
                            labels.pop();
                            labels.pop();
                            format!("@labels({}) ", labels)
                        };
                        println!(
                            "    {}{}{}{}: {}{};",
                            if field.is_unique { "@unique " } else { "" },
                            labels,
                            field.name,
                            if field.is_optional { "?" } else { "" },
                            field.field_type,
                            field
                                .default_value
                                .as_ref()
                                .map(|d| if field.field_type == "string" {
                                    format!(" = \"{}\"", d)
                                } else {
                                    format!(" = {}", d)
                                })
                                .unwrap_or_else(|| "".into()),
                        );
                    }
                    println!("  }}");
                }
                for def in &version_def.endpoint_defs {
                    println!("  Endpoint: {}", def.path);
                }
                for def in &version_def.label_policy_defs {
                    println!("  Label policy: {}", def.label);
                }
                println!("}}");
            }
        }
        Command::Dev {
            type_check,
            server_options,
        } => {
            launch_server(server_url, true, type_check, server_options).await?;
        }
        Command::New {
            path,
            no_examples,
            optimize,
            auto_index,
        } => {
            let path = Path::new(&path);
            if let Err(e) = fs::create_dir(path) {
                match e.kind() {
                    ErrorKind::AlreadyExists => {
                        anyhow::bail!("Directory `{}` already exists. Use `chisel init` to initialize a project in the directory.", path.display());
                    }
                    _ => {
                        anyhow::bail!(
                            "Unable to create a ChiselStrike project in `{}`: {}",
                            path.display(),
                            e
                        );
                    }
                }
            }
            let opts = CreateProjectOptions {
                force: false,
                examples: !no_examples,
                optimize,
                auto_index,
            };
            create_project(path, opts)?;
        }
        Command::Start {
            dev,
            type_check,
            server_options,
        } => {
            launch_server(server_url, dev, type_check, server_options).await?;
        }
        Command::Status => {
            let mut client = ChiselRpcClient::connect(server_url).await?;
            let request = tonic::Request::new(StatusRequest {});
            let response = execute!(client.get_status(request).await);
            println!("Server status is {}", response.message);
        }
        Command::Restart => {
            let mut client = ChiselRpcClient::connect(server_url.clone()).await?;
            let response = execute!(client.restart(tonic::Request::new(RestartRequest {})).await);
            println!(
                "{}",
                if response.ok {
                    "Server restarted successfully."
                } else {
                    "Server failed to restart."
                }
            );
            wait(server_url.clone()).await?;
        }
        Command::Wait => {
            wait(server_url).await?;
        }
        Command::Apply {
            allow_type_deletion,
            version,
            type_check,
        } => {
            apply(
                server_url,
                version,
                allow_type_deletion.into(),
                type_check.into(),
            )
            .await?;
        }
        Command::Delete { version } => {
            delete(server_url, version).await?;
        }
        Command::Populate { version, from } => {
            populate(server_url, version, from).await?;
        }
    }
    Ok(())
}
