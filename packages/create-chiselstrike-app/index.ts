#!/usr/bin/env node

import Handlebars from "handlebars";
import Commander from "commander";
import chalk from "chalk";
import fs from "fs";
import os from "os";
import packageJson from "./package.json";
import path from "path";
import spawn from "cross-spawn";

function isDirEmpty(dirname: string) {
    return fs.readdirSync(dirname).length === 0;
}

function run(projectDirectory: string, chiselVersion: string) {
    const projectName = path.basename(projectDirectory);
    projectDirectory = path.resolve(projectDirectory);
    if (fs.existsSync(projectDirectory)) {
        if (!isDirEmpty(projectDirectory)) {
            console.log(
                `Cannot create ChiselStrike project: directory ${
                    chalk.red(projectDirectory)
                } already exists.`,
            );
            process.exit(1);
        }
    } else {
        fs.mkdirSync(projectDirectory);
    }
    console.log(
        `Creating a new ChiselStrike project in ${
            chalk.green(projectDirectory)
        } ...`,
    );

    const routesPath = path.join(projectDirectory, "routes");
    const eventsPath = path.join(projectDirectory, "events");
    const modelsPath = path.join(projectDirectory, "models");
    const policiesPath = path.join(projectDirectory, "policies");

    fs.mkdirSync(path.join(projectDirectory, ".vscode"));
    fs.mkdirSync(routesPath);
    fs.mkdirSync(eventsPath);
    fs.closeSync(fs.openSync(path.join(eventsPath, ".gitkeep"), "w"));
    fs.mkdirSync(modelsPath);
    fs.closeSync(fs.openSync(path.join(modelsPath, ".gitkeep"), "w"));
    fs.mkdirSync(policiesPath);
    fs.closeSync(fs.openSync(path.join(policiesPath, ".gitkeep"), "w"));
    const rootFiles = [
        "Chisel.toml",
        "package.json",
        "tsconfig.json",
    ];
    for (const f of rootFiles) {
        const source = fs.readFileSync(
            path.join(__dirname, "template", f),
            "utf8",
        );
        const template = Handlebars.compile(source);
        fs.writeFileSync(
            path.join(projectDirectory, f),
            template({ projectName, chiselVersion }),
        );
    }
    fs.appendFileSync(
        path.join(projectDirectory, "Chisel.toml"),
        'modules = "node"\n',
    );

    const readmeSrc = fs.readFileSync(
        path.join(__dirname, "template", "README-template.md"),
        "utf8",
    );
    const readme = Handlebars.compile(readmeSrc);
    fs.writeFileSync(
        path.join(projectDirectory, "README.md"),
        readme({ projectName }),
    );
    fs.copyFileSync(
        path.join(__dirname, "template", "settings.json"),
        path.join(projectDirectory, ".vscode", "settings.json"),
    );
    fs.copyFileSync(
        path.join(__dirname, "template", "hello.ts"),
        path.join(projectDirectory, "routes", "hello.ts"),
    );
    fs.copyFileSync(
        path.join(__dirname, "template", "hello.ts"),
        path.join(projectDirectory, "routes", "hello.ts"),
    );
    fs.copyFileSync(
        path.join(__dirname, "template", "gitignore"),
        path.join(projectDirectory, "", ".gitignore"),
    );
    console.log("Installing packages. This might take a couple of minutes.");
    process.chdir(projectDirectory);
    spawn("npm", ["install"], {
        stdio: "inherit",
    });
}

if (os.type() == "Windows_NT") {
    console.log(chalk.red("Error: Failed to create a ChiselStrike project."));
    console.log("");
    console.log(
        "ChiselStrike is currently supported on Windows through Windows Subsystem for Linux (WSL).",
    );
    console.log("");
    console.log(
        "Please create your project in an ext4 filesystem (like the $HOME folder) to support hot reloading of routes.",
    );
    console.log("");
    console.log(
        "For more information, see the documentation at: https://docs.chiselstrike.com",
    );
    process.exit(1);
}

const _program = new Commander.Command(packageJson.name)
    .version(packageJson.version)
    .arguments("<project-directory>")
    .option(
        "-c, --chisel-version <version>",
        "ChiselStrike version to use.",
        packageJson.version,
    )
    .action((projectDirectory, options) => {
        run(projectDirectory, options.chiselVersion);
    })
    .parse(process.argv);
